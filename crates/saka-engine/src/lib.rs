mod action;
mod commands;
mod context;
mod metrics;
pub mod packet;
mod project_config;
pub mod recipe;
mod reconciler;
mod register;
mod runner;
mod secrets;
mod text;
mod timeline;
pub mod toolchain;
mod views;
pub mod webhooks;
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
use saka_ports::{
    Agents, Clock, DocSource, Environments, Ids, IdsSource, PortError, Scm, SecretStore, Toolchains,
};
use saka_store::read;
use saka_store::{Batch, EventRecord, JobOutcome, NewJob, Store, StoreError};
use tokio::sync::{Notify, watch};
use uuid::Uuid;

pub use action::{Action, Produced, Reply, Respond, StoredAction};
pub use commands::{NewProject, RegisteredSpec, SessionCommand};
pub use metrics::{MetricsSnapshot, Readiness, SecondsHistogram};
pub use register::{AddProject, Integration, parse_repo_url};
pub use saka_ports::{RunnerEvent, RunnerEventKind};
pub use saka_store::StoredEvent;
pub use saka_store::list::{Key, Page, PermissionFilter, WorkItemFilter};
pub use saka_store::read::{TimelineCursor, TimelinePage};
pub use secrets::SecretInfo;
pub use views::{
    AccountView, BudgetView, DependencyView, Paged, PermissionView, ProjectView, RevisionInfo,
    SessionView, SpecView, TaskDetail, TaskView, UsageView,
};
pub use worker::{JobReport, JobResult};

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub loop_cfg: LoopConfig,
    pub max_active_tasks: usize,
    pub policy: Policy,
    pub worker_id: String,
    pub integrations: Vec<Integration>,
    pub env: EnvConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvConfig {
    pub port_first: u16,
    pub port_last: u16,
    pub ports_per_env: u16,
}

impl EnvConfig {
    pub fn port_blocks(&self) -> usize {
        let span = usize::from(self.port_last.saturating_sub(self.port_first)) + 1;
        span / usize::from(self.ports_per_env.max(1))
    }

    pub fn block_start(&self, index: usize) -> Option<u16> {
        (index < self.port_blocks())
            .then(|| u32::from(self.port_first) + index as u32 * u32::from(self.ports_per_env))
            .and_then(|start| u16::try_from(start).ok())
    }
}

#[derive(Clone)]
pub struct Ports {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn Ids>,
    pub env: Arc<dyn Environments>,
    pub scm: Arc<dyn Scm>,
    pub agents: Arc<dyn Agents>,
    pub docs: Arc<dyn DocSource>,
    pub toolchains: Arc<dyn Toolchains>,
    pub secrets: Arc<dyn SecretStore>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Rejected(#[from] Rejection),
    #[error("{field}: {code}")]
    Invalid { field: &'static str, code: String },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("port: {}", .0.code)]
    Port(#[from] PortError),
    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cause {
    pub actor: String,
    pub causation_id: Option<Uuid>,
    pub correlation_id: Uuid,
}

pub const OPERATOR: &str = "operator:local";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Command,
    Fact,
}

#[derive(Default)]
struct Extra<'a> {
    outcome: Option<JobOutcome>,
    events: Vec<Event>,
    jobs: Vec<Job>,
    action: Option<(&'a Action<'a>, Shows)>,
    activity: Option<saka_core::model::SessionActivity>,
    offset: Option<(saka_core::ids::SessionId, u64)>,
}

#[derive(Debug, Clone, Copy)]
enum Shows {
    Task,
    Permission(saka_core::ids::PermissionRequestId),
    Session(saka_core::ids::SessionId),
}

pub struct Engine {
    store: Arc<Store>,
    ports: Ports,
    cfg: EngineConfig,
    lock: Mutex<()>,
    changed: Notify,
    counters: metrics::Counters,
    boot: Uuid,
    ports_in_flight: Mutex<std::collections::BTreeMap<saka_core::ids::EnvironmentId, u16>>,
}

impl Engine {
    pub fn new(store: Arc<Store>, ports: Ports, cfg: EngineConfig) -> Engine {
        let boot = ports.ids.new_id();
        Engine {
            boot,
            store,
            ports,
            cfg,
            lock: Mutex::new(()),
            changed: Notify::new(),
            counters: metrics::Counters::default(),
            ports_in_flight: Mutex::new(std::collections::BTreeMap::new()),
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

    pub fn cause(&self, actor: &str) -> Cause {
        Cause {
            actor: actor.to_string(),
            causation_id: None,
            correlation_id: self.new_id(),
        }
    }

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

    fn decide(
        &self,
        id: WorkItemId,
        input: Option<TaskInput>,
        mode: Mode,
        cause: &Cause,
        extra: Extra<'_>,
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
        batch
            .activities
            .extend(extra.activity.map(|a| (task.item.project_id, a)));
        batch.session_offsets.extend(extra.offset);
        let project = Some(task.item.project_id);
        let after = self.apply(&mut batch, task, &events, project, cause, now)?;
        for e in &extra.events {
            self.push_event(&mut batch, e, project, cause, now);
        }
        for job in extra.jobs {
            self.push_job(&mut batch, job, project, cause);
        }
        if let Some((action, shows)) = extra.action {
            let reply = match shows {
                Shows::Task => (action.respond)(Produced::Task(&after)),
                Shows::Permission(pid) => {
                    let request = after
                        .permissions
                        .iter()
                        .find(|p| p.id == pid)
                        .cloned()
                        .ok_or_else(|| {
                            EngineError::Internal("answered request not in the task".into())
                        })?;
                    let view = PermissionView {
                        request,
                        project_id: after.item.project_id,
                    };
                    (action.respond)(Produced::Permission(&view))
                }
                Shows::Session(sid) => {
                    let session = after
                        .session
                        .iter()
                        .chain(after.retired_sessions.iter())
                        .find(|s| s.id == sid)
                        .ok_or_else(|| EngineError::Internal("session not in the task".into()))?;
                    (action.respond)(Produced::Session(session))
                }
            };
            batch.action = Some(action.record(self, cause, project, Some(id.0), reply));
        }
        let verification = metrics::dod_verification(&events, &after);
        if !events.is_empty() {
            batch.tasks.push(after);
        }
        self.commit(&batch)?;
        match verification {
            Some(metrics::DodVerification::Outcome(outcome)) => self.counters.dod(outcome),
            Some(metrics::DodVerification::Started { head_sha, sources }) => {
                tracing::info!(work_item_id = %id, head_sha, sources = ?sources, "delivery.dod.verify");
            }
            None => {}
        }
        Ok(events)
    }

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
            log_runtime(e, &cause.actor);
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
        if !batch.events.is_empty()
            || !batch.jobs.is_empty()
            || batch.action.is_some()
            || !batch.activities.is_empty()
        {
            self.changed.notify_waiters();
        }
        Ok(seqs)
    }

    pub async fn run(self: Arc<Self>, workers: usize, mut shutdown: watch::Receiver<bool>) {
        let mut tasks = tokio::task::JoinSet::new();
        self.counters
            .worker_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        for _ in 0..workers.max(1) {
            let engine = Arc::clone(&self);
            let shutdown = shutdown.clone();
            tasks.spawn(async move { engine.worker_loop(shutdown).await });
        }
        self.counters
            .reconciler_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
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

pub(crate) fn log_runtime(e: &Event, actor: &str) {
    use Event as E;
    match e {
        E::SessionStarted {
            work_item_id,
            session_id,
            resumed_from_session_id: Some(from),
            resume_cause,
            ..
        } => {
            tracing::info!(%work_item_id, %session_id, resumed_from = %from, cause = ?resume_cause, "session resumed")
        }
        E::SessionReattached {
            session_id,
            runner_version,
            from_offset,
            ..
        } => tracing::info!(%session_id, %runner_version, from_offset, "session reattached"),
        E::SessionSteered {
            session_id,
            turn,
            n,
            ..
        } => {
            tracing::info!(%session_id, turn, n, %actor, "session steered");
        }
        E::SessionInterruptRequested {
            session_id,
            actor: by,
            ..
        } => tracing::info!(%session_id, by = %by, %actor, "session interrupted"),
        E::SessionStopRequested { session_id, .. } => {
            tracing::info!(%session_id, %actor, "session stopped");
        }
        E::PermissionAnswered {
            permission_request_id,
            state: saka_core::vocab::PermissionState::Expired,
            answer_source,
            ..
        } if answer_source == task::WAIT_LIMIT_SOURCE => {
            tracing::info!(%permission_request_id, "permission request expired");
        }
        E::PermissionRuleAdded { rule_id, tool, .. } => {
            tracing::info!(%rule_id, %tool, %actor, "permission rule added");
        }
        E::PermissionRuleRevoked { rule_id, .. } => {
            tracing::info!(%rule_id, %actor, "permission rule revoked");
        }
        E::BudgetWarned {
            stage_run_id,
            dimension,
            used,
            limit,
            ..
        } => tracing::info!(%stage_run_id, %dimension, %used, %limit, "budget warning"),
        E::WorkItemBlocked {
            work_item_id,
            code: saka_core::vocab::BlockerCode::BudgetExhausted,
            params,
            ..
        } => {
            tracing::info!(
                %work_item_id,
                dimension = params["dimension"].as_str().unwrap_or_default(),
                used = %params["used"],
                limit = %params["limit"],
                "budget exhausted"
            )
        }
        _ => {}
    }
}

pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
const IDLE_POLL: Duration = Duration::from_millis(500);
