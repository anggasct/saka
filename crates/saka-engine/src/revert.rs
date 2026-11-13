use saka_core::Ctx;
use saka_core::event::Event;
use saka_core::ids::WorkItemId;
use saka_core::model::Task;
use saka_core::rejection::Rejection;
use saka_core::revert;
use saka_core::task;
use saka_ports::IdsSource;
use saka_store::{Batch, read};

use crate::action::{Action, Produced};
use crate::text::{change_title, task_marker};
use crate::{Cause, Engine, EngineError, Result};

const REASON_MAX: usize = 500;

fn revert_reason(value: &str) -> Result<String> {
    let flat: String = value
        .chars()
        .map(|c| {
            if matches!(c, '\n' | '\r' | '\t') {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !c.is_control())
        .collect();
    let trimmed = flat.trim();
    let len = trimmed.chars().count();
    if len == 0 || len > REASON_MAX {
        return Err(EngineError::Invalid {
            field: "reason",
            code: "length".into(),
        });
    }
    Ok(trimmed.to_string())
}

fn title_of(origin: &Task) -> String {
    let landed = origin
        .external
        .as_ref()
        .map_or_else(|| change_title(&origin.item.work), |e| e.title.clone());
    change_title(&format!("Revert \"{landed}\""))
}

impl Engine {
    pub fn revert_with(
        &self,
        id: WorkItemId,
        expected_version: u64,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        self.revert_linked(id, Some(expected_version), why, None, cause, action)
            .map(|(events, _)| events)
    }

    pub(crate) fn revert_linked(
        &self,
        id: WorkItemId,
        expected_version: Option<u64>,
        why: &str,
        incident: Option<saka_core::ids::IncidentId>,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<(Vec<Event>, Task)> {
        let reason = revert_reason(why)?;
        let _guard = self.locked()?;
        let now = self.now();
        let origin = self.load_task(id)?;
        if expected_version.is_some_and(|v| origin.item.version != v) {
            return Err(Rejection::VersionStale.into());
        }
        let reverts = self.store.read(|c| read::reverts_of(c, id))?;
        let mut ids = IdsSource(&*self.ports.ids);
        let mut ctx = Ctx { now, ids: &mut ids };
        let events = revert::request(&origin, &reverts, &title_of(&origin), reason, &mut ctx)?;
        let [requested, created] = events.as_slice() else {
            return Err(EngineError::Internal("revert request made no task".into()));
        };
        let project = Some(origin.item.project_id);
        let mut batch = Batch::new(now);
        let origin_after = self.apply(
            &mut batch,
            origin,
            std::slice::from_ref(requested),
            project,
            cause,
            now,
        )?;
        let revert_task = task::evolve(None, created, now)
            .ok_or_else(|| EngineError::Internal("revert event made no task".into()))?;
        self.push_event(&mut batch, created, project, cause, now);
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Task(&revert_task));
            batch.action = Some(action.record(self, cause, project, Some(id.0), reply));
        }
        batch.tasks.push(origin_after);
        batch.tasks.push(revert_task.clone());
        if let Some(incident_id) = incident {
            let origin_project = revert_task.item.project_id;
            self.link_incident(
                &mut batch,
                incident_id,
                saka_core::incident::Recovery::Revert(revert_task.item.id),
                origin_project,
                cause,
                now,
            )?;
        }
        self.commit(&batch)?;
        tracing::info!(
            work_item_id = %id,
            revert_id = %revert_task.item.id,
            task_key = revert_task.item.task_key,
            "delivery.revert.requested"
        );
        Ok((events, revert_task))
    }

    pub(crate) fn finish_revert(
        &self,
        batch: &mut Batch,
        reverted: &Task,
        events: &[Event],
        project: Option<saka_core::ids::ProjectId>,
        cause: &Cause,
        now: saka_core::time::Timestamp,
    ) -> Result<Option<Task>> {
        let Some(link) = &reverted.revert else {
            return Ok(None);
        };
        let Some(sha) = events.iter().find_map(|e| match e {
            Event::WorkItemDone {
                merge_commit_sha, ..
            } => Some(merge_commit_sha.clone()),
            _ => None,
        }) else {
            return Ok(None);
        };
        let origin = self.load_task(link.of)?;
        let completed = revert::completed(&origin, reverted.item.id, sha);
        Ok(Some(
            self.apply(batch, origin, &completed, project, cause, now)?,
        ))
    }

    pub(crate) fn revert_text(&self, task: &Task) -> Result<(String, String)> {
        let origin = self
            .origin_of(task)
            .map_err(|e| EngineError::Internal(e.to_string()))?
            .ok_or_else(|| EngineError::Internal("revert task without its origin".into()))?;
        let change = origin.change_request.as_ref();
        let landed = change
            .and_then(|c| c.merge_commit_sha.as_deref())
            .unwrap_or_default();
        let short: String = landed.chars().take(7).collect();
        let reason = task
            .revert
            .as_ref()
            .map(|r| r.reason.as_str())
            .unwrap_or_default();
        let mut body = format!(
            "Reverts {} (merge commit `{short}`).\n\nReason: {reason}\n",
            change.map(|c| c.url.as_str()).unwrap_or_default()
        );
        if let Some(base) = &self.cfg.public_url {
            body.push_str(&format!(
                "\nTask: {}/delivery/tasks/{}\n",
                base.trim_end_matches('/'),
                origin.item.id
            ));
        }
        body.push_str(&format!("\n{}\n", task_marker(&task.item.task_key)));
        Ok((change_title(&task.item.work), body))
    }
}
