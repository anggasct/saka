use saka_core::reconcile::{Action, Snapshot, reconcile, waiting};
use saka_core::spec;
use saka_core::task::TaskInput;
use saka_store::{Batch, TaskSet, read};

use crate::{Engine, EngineError, Extra, Mode, Result};

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
        let mut lost = 0;
        for task in tasks.iter().filter(|t| t.live_session().is_some()) {
            match self.decide(
                task.item.id,
                Some(TaskInput::ServerRestarted),
                Mode::Fact,
                &cause,
                Extra::default(),
            ) {
                Ok(events) if !events.is_empty() => lost += 1,
                Ok(_) => {}
                Err(EngineError::Rejected(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(lost)
    }
}
