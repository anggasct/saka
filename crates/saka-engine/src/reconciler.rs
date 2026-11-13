use saka_core::ids::WorkItemId;
use saka_core::job::{Job, JobKind};
use saka_core::model::Task;
use saka_core::reconcile::{Action, Snapshot, reconcile, waiting};
use saka_core::spec;
use saka_core::task::{TaskInput, retry_entry};
use saka_core::time::Timestamp;
use saka_core::vocab::BlockerCode;
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
                tasks: read::load_tasks(c, TaskSet::Reconcile, &self.cfg.workflow_policy)?,
                work_item_dependencies: read::queued_work_item_dependencies(c)?,
                spec_dependencies: read::spec_dependencies(c, None)?,
                quotas: read::account_quotas(c)?,
                port_blocks: Some(self.cfg.env.port_blocks()),
                user_links: saka_store::user_link::active_user_links(c)?
                    .into_iter()
                    .filter(|l| l.owner == crate::OPERATOR)
                    .collect(),
                authorships: saka_store::authorship::project_authorships(c)?,
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
                    let auto_retry = matches!(input, TaskInput::AutoRetry)
                        .then(|| pending_auto_retry(&snapshot, id))
                        .flatten();
                    let resumed = match &input {
                        TaskInput::Resume { trigger } => Some(*trigger),
                        _ => None,
                    };
                    let advance = matches!(input, TaskInput::Admit)
                        .then(|| latest_merged_dependency(&snapshot, id))
                        .flatten();
                    let events =
                        self.decide(id, Some(input), Mode::Fact, &cause, Extra::default())?;
                    if !events.is_empty() {
                        report.decisions += 1;
                        if let Some((after, merged_at)) = advance {
                            let waited = u64::try_from(now.since(merged_at)).unwrap_or(0);
                            let seconds = std::time::Duration::from_millis(waited).as_secs_f64();
                            self.counters.step_advance(seconds);
                            tracing::info!(
                                work_item_id = %id,
                                after_work_item_id = %after,
                                seconds,
                                "delivery.advance"
                            );
                        }
                        if let Some(trigger) = resumed {
                            tracing::info!(
                                work_item_id = %id,
                                reason = trigger.as_str(),
                                "delivery.retry.resumed"
                            );
                        }
                        if let Some((code, attempt)) = auto_retry {
                            self.counters.auto_retry(code.as_str());
                            tracing::info!(
                                work_item_id = %id,
                                code = code.as_str(),
                                attempt,
                                "delivery.retry.auto"
                            );
                        }
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
                        .find(|t| saka_core::job::work_item_of(&job.payload) == Some(t.item.id))
                        .map(|t| t.item.project_id);
                    self.push_job(&mut jobs, job, project, &cause);
                }
            }
        }
        report.decisions += self.reconcile_drafts(&snapshot.quotas, &cause, &mut jobs)?;
        self.push_job(&mut jobs, crate::webhooks::prune_job(now), None, &cause);
        for (project, job) in self.reload_jobs(now)? {
            self.push_job(&mut jobs, job, Some(project), &cause);
        }
        for (project, job) in self.scan_jobs(now)? {
            self.push_job(&mut jobs, job, Some(project), &cause);
        }
        for (project, job) in self.issue_scan_jobs(now)? {
            self.push_job(&mut jobs, job, Some(project), &cause);
        }
        report.decisions += self.reconcile_issue_intakes(&cause)?;
        self.reconcile_user_links(now, &cause, &mut jobs)?;
        for job in self.monitor_sessions(snapshot.tasks.iter().filter_map(Task::live_session)) {
            let project = project_of(&snapshot.tasks, &job);
            self.push_job(&mut jobs, job, project, &cause);
        }
        if !jobs.jobs.is_empty() {
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
                read::load_tasks(c, TaskSet::Reconcile, &self.cfg.workflow_policy)?,
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
            let event_id = self.push_event(&mut batch, e, Some(spec.project_id), cause, now);
            self.plan_spec_hooks(&mut batch, &spec, event_id, cause, now)?;
        }
        batch.specs.extend(after);
        self.commit(&batch)?;
        Ok(true)
    }

    pub fn recover(&self) -> Result<usize> {
        let tasks = self
            .store
            .read(|c| read::load_tasks(c, TaskSet::Reconcile, &self.cfg.workflow_policy))?;
        let cause = self.cause("system:recovery");
        let mut batch = Batch::new(self.now());
        for job in self.monitor_sessions(tasks.iter().filter_map(Task::live_session)) {
            let project = Some(project_of(&tasks, &job));
            self.push_job(&mut batch, job, project.flatten(), &cause);
        }
        self.recover_drafts(&cause, &mut batch)?;
        let queued = batch.jobs.len();
        if queued > 0 {
            self.commit(&batch)?;
        }
        Ok(queued)
    }

    pub(crate) fn monitor_sessions<'a>(
        &self,
        sessions: impl Iterator<Item = &'a saka_core::model::Session>,
    ) -> Vec<Job> {
        let mut jobs = Vec::new();
        for session in sessions {
            match self.ports.agents.link(session.id) {
                Link::Open { silent_ms } if silent_ms > RUNNER_SILENCE_MS => {
                    tracing::info!(session_id = %session.id, silent_ms, "runner is silent; reconnecting");
                    self.ports.agents.disconnect(session.id);
                }
                Link::Open { .. } => {}
                Link::Closed { generation } => jobs.push(Job::new(
                    JobKind::SessionReattach,
                    format!("{}:{}:{generation}", session.id, self.boot),
                    json!({ "owner": session.owner, "session_id": session.id }),
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
        .find(|t| saka_core::job::work_item_of(&job.payload) == Some(t.item.id))
        .map(|t| t.item.project_id)
}

fn pending_auto_retry(snapshot: &Snapshot, id: WorkItemId) -> Option<(BlockerCode, u32)> {
    let task = snapshot.tasks.iter().find(|t| t.item.id == id)?;
    let blocker = task.open_blocker()?;
    let (stage, _) = retry_entry(blocker.code, &blocker.params)?;
    let done = task.retries.get(&stage).map_or(0, |r| r.auto);
    Some((blocker.code, done + 1))
}

fn latest_merged_dependency(
    snapshot: &Snapshot,
    id: WorkItemId,
) -> Option<(WorkItemId, Timestamp)> {
    snapshot
        .work_item_dependencies
        .iter()
        .filter(|d| d.work_item_id == id)
        .filter_map(|d| {
            let t = snapshot
                .tasks
                .iter()
                .find(|t| t.item.id == d.depends_on_work_item_id)?;
            Some((t.item.id, t.change_request.as_ref()?.merged_at?))
        })
        .max_by_key(|(_, at)| *at)
}
