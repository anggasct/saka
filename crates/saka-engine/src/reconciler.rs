use saka_core::job::{Job, JobKind};
use saka_core::model::Task;
use saka_core::reconcile::{Action, Snapshot, reconcile, waiting};
use saka_core::spec;
use saka_store::{Batch, TaskSet, read};

use saka_ports::Link;
use serde_json::json;

use crate::{Engine, Extra, Mode, Result};

const RUNNER_SILENCE_MS: i64 = 30_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub decisions: usize,
    pub shipped: usize,
}

impl Engine {
    pub fn reconcile(&self) -> Result<ReconcileReport> {
        let now = self.now();
        let snapshot = self.store.read(|c| {
            Ok(Snapshot {
                specs: read::reconcile_specs(c)?,
                tasks: read::load_tasks(c, TaskSet::Reconcile)?,
                work_item_dependencies: read::queued_work_item_dependencies(c)?,
                spec_dependencies: read::spec_dependencies(c, None)?,
                quotas: read::account_quotas(c)?,
            })
        })?;
        self.counters
            .note_waiting(waiting(&snapshot, &self.cfg.loop_cfg, now));
        let actions = reconcile(
            &snapshot,
            &self.cfg.loop_cfg,
            self.cfg.max_active_tasks,
            now,
        );
        let cause = self.cause("system:reconciler");
        let mut report = ReconcileReport::default();
        let mut jobs = Batch::new(now);
        for action in actions {
            match action {
                Action::Decide(id, input) => {
                    let events =
                        self.decide(id, Some(input), Mode::Fact, &cause, Extra::default())?;
                    if !events.is_empty() {
                        report.decisions += 1;
                    }
                }
                Action::ShipSpec(spec_id) => {
                    if self.ship(spec_id, &cause)? {
                        report.shipped += 1;
                    }
                }
                Action::Enqueue(job) => {
                    let project = snapshot
                        .tasks
                        .iter()
                        .find(|t| {
                            job.payload.get("work_item_id").and_then(|v| v.as_str())
                                == Some(t.item.id.to_string().as_str())
                        })
                        .map(|t| t.item.project_id);
                    self.push_job(&mut jobs, job, project, &cause);
                }
            }
        }
        for job in self.monitor(&snapshot.tasks) {
            let project = project_of(&snapshot.tasks, &job);
            self.push_job(&mut jobs, job, project, &cause);
        }
        if !jobs.jobs.is_empty() {
            // Jobs that already exist keep their row: keys are unique.
            self.store.commit(&jobs)?;
        }
        Ok(report)
    }

    fn ship(&self, spec_id: saka_core::ids::SpecId, cause: &crate::Cause) -> Result<bool> {
        let _guard = self.locked()?;
        let now = self.now();
        let (spec, tasks) = self.store.read(|c| {
            Ok((
                read::load_spec(c, spec_id)?,
                read::load_tasks(c, TaskSet::Reconcile)?,
            ))
        })?;
        let Some(spec) = spec else {
            return Ok(false);
        };
        let mine: Vec<_> = tasks
            .iter()
            .filter(|t| t.item.spec_id == Some(spec_id))
            .collect();
        let ready = spec::ships(mine.iter().map(|t| t.item.state));
        let Ok(events) = spec::ship(&spec, ready) else {
            return Ok(false);
        };
        let mut batch = Batch::new(now);
        let mut after = Some(spec.clone());
        for e in &events {
            after = spec::evolve(after, e, now);
            self.push_event(&mut batch, e, Some(spec.project_id), cause, now);
        }
        batch.specs.extend(after);
        self.commit(&batch)?;
        Ok(true)
    }

    pub fn recover(&self) -> Result<usize> {
        let tasks = self
            .store
            .read(|c| read::load_tasks(c, TaskSet::Reconcile))?;
        let cause = self.cause("system:recovery");
        let mut batch = Batch::new(self.now());
        for job in self.monitor(&tasks) {
            let project = Some(project_of(&tasks, &job));
            self.push_job(&mut batch, job, project.flatten(), &cause);
        }
        let queued = batch.jobs.len();
        if queued > 0 {
            self.commit(&batch)?;
        }
        Ok(queued)
    }

    fn monitor(&self, tasks: &[Task]) -> Vec<Job> {
        let mut jobs = Vec::new();
        for (task, session) in tasks
            .iter()
            .filter_map(|t| t.live_session().map(|s| (t, s)))
        {
            match self.ports.agents.link(session.id) {
                Link::Open { silent_ms } if silent_ms > RUNNER_SILENCE_MS => {
                    tracing::info!(session_id = %session.id, silent_ms, "runner is silent; reconnecting");
                    self.ports.agents.disconnect(session.id);
                }
                Link::Open { .. } => {}
                Link::Closed { generation } => jobs.push(Job::new(
                    JobKind::SessionReattach,
                    format!("{}:{}:{generation}", session.id, self.boot),
                    json!({ "work_item_id": task.item.id, "session_id": session.id }),
                    self.now(),
                )),
            }
        }
        jobs
    }
}

fn project_of(tasks: &[Task], job: &Job) -> Option<saka_core::ids::ProjectId> {
    tasks
        .iter()
        .find(|t| {
            job.payload.get("work_item_id").and_then(|v| v.as_str())
                == Some(t.item.id.to_string().as_str())
        })
        .map(|t| t.item.project_id)
}
