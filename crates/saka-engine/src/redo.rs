use saka_core::Ctx;
use saka_core::deploy::FixFailure;
use saka_core::event::Event;
use saka_core::ids::WorkItemId;
use saka_core::model::Task;
use saka_core::redo::{self, Facts, FailedDeployment, Landed, Outcome};
use saka_core::rejection::Rejection;
use saka_core::task;
use saka_core::vocab::{DeployCheckState, DeploymentState, WorkItemState};
use saka_ports::IdsSource;
use saka_store::{Batch, deployment, read};

use crate::action::{Action, Produced};
use crate::revert::{REASON_MAX, operator_text};
use crate::{Cause, Engine, EngineError, Result};

const NOTE_MAX: usize = 2_000;

fn landed(task: &Task) -> Option<Landed> {
    task.change_request.as_ref().map(|c| Landed {
        url: c.url.clone(),
        merge_commit: c.merge_commit_sha.clone(),
    })
}

impl Engine {
    pub fn redo_with(
        &self,
        id: WorkItemId,
        expected_version: u64,
        why: &str,
        note: Option<&str>,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        let reason = operator_text(why, "reason", REASON_MAX)?;
        let note = note
            .filter(|n| !n.trim().is_empty())
            .map(|n| operator_text(n, "note", NOTE_MAX))
            .transpose()?;
        let _guard = self.locked()?;
        let now = self.now();
        let origin = self.load_task(id)?;
        if origin.item.version != expected_version {
            return Err(Rejection::VersionStale.into());
        }
        let mut ids = IdsSource(&*self.ports.ids);
        let mut ctx = Ctx { now, ids: &mut ids };
        let events = redo::request(&origin, reason, note, &mut ctx)?;
        let [requested, superseded, created] = events.as_slice() else {
            return Err(EngineError::Internal("redo request made no task".into()));
        };
        let project = Some(origin.item.project_id);
        let mut batch = Batch::new(now);
        let origin_after = self.apply(
            &mut batch,
            origin,
            &[requested.clone(), superseded.clone()],
            project,
            cause,
            now,
        )?;
        let redo_task = task::evolve(None, created, now)
            .ok_or_else(|| EngineError::Internal("redo event made no task".into()))?;
        self.push_event(&mut batch, created, project, cause, now);
        batch
            .moved_work_item_dependencies
            .push((id, redo_task.item.id));
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Task(&redo_task));
            batch.action = Some(action.record(self, cause, project, Some(id.0), reply));
        }
        batch.tasks.push(origin_after);
        batch.tasks.push(redo_task.clone());
        self.commit(&batch)?;
        tracing::info!(
            work_item_id = %id,
            redo_id = %redo_task.item.id,
            task_key = redo_task.item.task_key,
            "delivery.redo.requested"
        );
        Ok(events)
    }

    pub(crate) fn redo_attempt_text(&self, task: &Task) -> Result<Option<String>> {
        let Some(link) = &task.redo else {
            return Ok(None);
        };
        let origin = self.load_task(link.of)?;
        let reverts = self.store.read(|c| read::reverts_of(c, origin.item.id))?;
        let revert_link = reverts.iter().find(|r| r.state == WorkItemState::Done);
        let revert_task = revert_link.map(|r| self.load_task(r.id)).transpose()?;
        let mut deployments = Vec::new();
        for gate in &origin.deploy_gates {
            let Some(shipped) = gate
                .deployment
                .as_ref()
                .filter(|d| d.state == DeploymentState::Failed)
            else {
                continue;
            };
            let found = self.store.read(|c| {
                Ok((
                    deployment::load(c, shipped.id)?,
                    deployment::checks(c, shipped.id)?,
                ))
            })?;
            let (Some(failed), checks) = found else {
                continue;
            };
            deployments.push(FailedDeployment {
                environment: failed.environment.clone(),
                sha: failed.sha.clone(),
                phase: failed.phase.map(|p| p.as_str().to_string()),
                code: failed
                    .error
                    .as_ref()
                    .map_or_else(|| "unknown".to_string(), |e| e.code.clone()),
                failures: checks
                    .iter()
                    .filter(|c| c.state != DeployCheckState::Passed)
                    .map(|c| FixFailure {
                        phase: c.phase,
                        command: c.command.clone(),
                        state: c.state,
                        exit_code: c.exit_code,
                        output_tail: c.output_tail.clone(),
                    })
                    .collect(),
            });
        }
        Ok(Some(redo::previous_attempt(&Facts {
            key: origin.item.task_key.clone(),
            outcome: if revert_task.is_some() {
                Outcome::Reverted
            } else {
                Outcome::Cancelled
            },
            change: landed(&origin),
            revert_key: revert_task.as_ref().map(|r| r.item.task_key.clone()),
            revert: revert_task.as_ref().and_then(landed),
            deployments,
            reason: link.reason.clone(),
            note: link.note.clone(),
        })))
    }
}
