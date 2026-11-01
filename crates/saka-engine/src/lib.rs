//! Shell around the core: loads state, calls the core, and writes events, projections, and
//! jobs; runs workers and the reconciler.
//!
//! Every decision is `load → decide → commit` under one lock, so two decisions about the
//! same task never interleave. Side effects (port calls) run outside the lock; their
//! results come back as facts the core decides on.
//!
//! The pieces are callable one at a time (`run_next_job`, `reconcile`, `on_runner_event`),
//! which is how the simulation drives the engine deterministically. [`Engine::run`] loops
//! them for the server.

mod commands;
mod reconciler;
mod runner;
mod text;
mod worker;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use saka_core::Ctx;
use saka_core::event::Event;
use saka_core::ids::{ProjectId, WorkItemId};
use saka_core::job::Job;
use saka_core::model::Task;
use saka_core::policy::Policy;
use saka_core::react::react;
use saka_core::rejection::Rejection;
use saka_core::task::{self, LoopConfig, TaskInput};
use saka_core::time::Timestamp;
use saka_ports::{Agents, Clock, Environments, Ids, IdsSource, Scm};
use saka_store::read;
use saka_store::{Batch, EventRecord, JobOutcome, NewJob, Store, StoreError};
use tokio::sync::{Notify, watch};
use uuid::Uuid;

pub use commands::{NewProject, RegisteredSpec};
pub use worker::JobReport;

/// Operator settings the engine applies.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub loop_cfg: LoopConfig,
    /// `scheduler.max_active_tasks`.
    pub max_active_tasks: usize,
    /// Operator permission rules, compiled.
    pub policy: Policy,
    /// Lease owner name for this process's workers.
    pub worker_id: String,
}

/// The adapters the engine uses.
#[derive(Clone)]
pub struct Ports {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn Ids>,
    pub env: Arc<dyn Environments>,
    pub scm: Arc<dyn Scm>,
    pub agents: Arc<dyn Agents>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The core refused the command.
    #[error(transparent)]
    Rejected(#[from] Rejection),
    #[error("{field}: {code}")]
    Invalid { field: &'static str, code: String },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// Who caused a write: stored on each event and job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cause {
    pub actor: String,
    pub causation_id: Option<Uuid>,
    pub correlation_id: Uuid,
}

/// The local operator, as recorded on events and permission answers.
pub const OPERATOR: &str = "operator:local";

/// How a decision treats a refusal from the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// A command from a person: a refusal is returned and nothing is written.
    Command,
    /// A fact from outside: a refusal means it no longer applies; the rest is written.
    Fact,
}

/// Extra rows written with a decision.
#[derive(Debug, Default)]
struct Extra {
    outcome: Option<JobOutcome>,
    events: Vec<Event>,
    jobs: Vec<Job>,
}

pub struct Engine {
    store: Arc<Store>,
    ports: Ports,
    cfg: EngineConfig,
    lock: Mutex<()>,
    changed: Notify,
}

impl Engine {
    pub fn new(store: Arc<Store>, ports: Ports, cfg: EngineConfig) -> Engine {
        Engine {
            store,
            ports,
            cfg,
            lock: Mutex::new(()),
            changed: Notify::new(),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    pub fn now(&self) -> Timestamp {
        self.ports.clock.now()
    }

    fn new_id(&self) -> Uuid {
        self.ports.ids.new_id()
    }

    /// A cause with a fresh correlation id.
    pub fn cause(&self, actor: &str) -> Cause {
        Cause {
            actor: actor.to_string(),
            causation_id: None,
            correlation_id: self.new_id(),
        }
    }

    /// Resolves after the next commit that wrote events.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    fn locked(&self) -> Result<MutexGuard<'_, ()>> {
        self.lock
            .lock()
            .map_err(|_| EngineError::Internal("engine lock poisoned".into()))
    }

    fn load_task(&self, id: WorkItemId) -> Result<Task> {
        self.store
            .read(|c| read::load_task(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))
    }

    /// `load → decide → commit` for one task.
    fn decide(
        &self,
        id: WorkItemId,
        input: Option<TaskInput>,
        mode: Mode,
        cause: &Cause,
        extra: Extra,
    ) -> Result<Vec<Event>> {
        let _guard = self.locked()?;
        let now = self.now();
        let task = self.load_task(id)?;
        let events = match input {
            Some(input) => {
                let mut ids = IdsSource(&*self.ports.ids);
                let mut ctx = Ctx { now, ids: &mut ids };
                match task::decide(&task, input, &self.cfg.loop_cfg, &mut ctx) {
                    Ok(events) => events,
                    Err(rejection) if mode == Mode::Fact => {
                        tracing::debug!(work_item_id = %id, code = rejection.code(), "fact no longer applies");
                        Vec::new()
                    }
                    Err(rejection) => return Err(rejection.into()),
                }
            }
            None => Vec::new(),
        };
        let mut batch = Batch::new(now);
        batch.job_outcome = extra.outcome;
        let project = Some(task.item.project_id);
        let after = self.apply(&mut batch, task, &events, project, cause, now)?;
        for e in &extra.events {
            self.push_event(&mut batch, e, project, cause, now);
        }
        for job in extra.jobs {
            self.push_job(&mut batch, job, project, cause);
        }
        if !events.is_empty() {
            batch.tasks.push(after);
        }
        self.commit(&batch)?;
        Ok(events)
    }

    /// Evolves the task through `events`, adding the event rows and the jobs they cause.
    fn apply(
        &self,
        batch: &mut Batch,
        task: Task,
        events: &[Event],
        project: Option<ProjectId>,
        cause: &Cause,
        now: Timestamp,
    ) -> Result<Task> {
        let mut after = task;
        for e in events {
            after = task::evolve(Some(after), e, now).ok_or_else(|| {
                EngineError::Internal(format!("evolve dropped the task on {}", e.kind()))
            })?;
            for job in react(&after, e, &self.cfg.loop_cfg, now) {
                self.push_job(batch, job, project, cause);
            }
            self.push_event(batch, e, project, cause, now);
        }
        Ok(after)
    }

    fn push_event(
        &self,
        batch: &mut Batch,
        e: &Event,
        project: Option<ProjectId>,
        cause: &Cause,
        now: Timestamp,
    ) {
        let stream = e.stream();
        batch.events.push(EventRecord {
            id: self.new_id(),
            kind: e.kind(),
            version: Event::VERSION,
            stream_type: stream.kind().to_string(),
            stream_id: stream.id(),
            project_id: project,
            actor: cause.actor.clone(),
            causation_id: cause.causation_id,
            correlation_id: cause.correlation_id,
            occurred_at: now,
            payload: e.payload(),
        });
    }

    fn push_job(&self, batch: &mut Batch, job: Job, project: Option<ProjectId>, cause: &Cause) {
        batch.jobs.push(NewJob {
            id: self.new_id(),
            job,
            project_id: project,
            causation_id: cause.causation_id,
            correlation_id: cause.correlation_id,
        });
    }

    fn commit(&self, batch: &Batch) -> Result<Vec<i64>> {
        let seqs = self.store.commit(batch)?;
        if !batch.events.is_empty() || !batch.jobs.is_empty() {
            self.changed.notify_waiters();
        }
        Ok(seqs)
    }

    /// Runs workers and the reconciler until `shutdown` turns true. Call [`Engine::recover`]
    /// first.
    pub async fn run(self: Arc<Self>, workers: usize, mut shutdown: watch::Receiver<bool>) {
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..workers.max(1) {
            let engine = Arc::clone(&self);
            let shutdown = shutdown.clone();
            tasks.spawn(async move { engine.worker_loop(shutdown).await });
        }
        loop {
            if let Err(e) = self.reconcile() {
                tracing::error!(error = %e, "reconcile failed");
            }
            tokio::select! {
                _ = tokio::time::sleep(RECONCILE_INTERVAL) => {}
                _ = self.changed() => {}
                _ = shutdown.changed() => {}
            }
            if *shutdown.borrow() {
                break;
            }
        }
        tasks.join_all().await;
    }

    async fn worker_loop(&self, mut shutdown: watch::Receiver<bool>) {
        while !*shutdown.borrow() {
            match self.run_next_job().await {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(e) => tracing::error!(error = %e, "job run failed"),
            }
            tokio::select! {
                _ = tokio::time::sleep(IDLE_POLL) => {}
                _ = self.changed() => {}
                _ = shutdown.changed() => {}
            }
        }
    }
}

/// How often the reconciler runs when nothing changed.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
const IDLE_POLL: Duration = Duration::from_millis(500);
