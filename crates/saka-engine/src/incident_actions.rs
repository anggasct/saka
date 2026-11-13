use saka_core::ids::{IncidentId, ProjectId, SpecDraftId, WorkItemId};
use saka_core::incident::{self, DraftFacts, DraftSuspect, Incident, Input, Recovery};
use saka_core::job::JobKind;
use saka_core::model::Draft;
use saka_core::rejection::Rejection;
use saka_core::vocab::{
    DraftLocale, DraftMode, IncidentCause, IncidentResolution, IncidentState, SpecDraftState,
    SpecState, TargetKind, WorkItemState, WorkKind,
};
use saka_store::{Batch, incident as rows, issue_intake, read};

use crate::action::{Action, Produced};
use crate::commands::reason;
use crate::draft::{NOTE_MAX, NewSpecDraft};
use crate::incident::{IncidentRow, Suspect};
use crate::incident_ingest::INTAKE_ACTOR;
use crate::worker::{Effect, Execution, field, internal};
use crate::{Cause, Engine, EngineError, Result};
use saka_store::ClaimedJob;

fn suspect_label(suspect: &Suspect) -> DraftSuspect {
    match suspect {
        Suspect::Task { key, change, .. } => DraftSuspect {
            label: key.clone(),
            change_url: change.as_ref().map(|(_, url)| url.clone()),
        },
        Suspect::Change { number, url } => DraftSuspect {
            label: format!("PR #{number}"),
            change_url: Some(url.clone()),
        },
    }
}

impl Engine {
    pub(crate) fn link_incident(
        &self,
        batch: &mut Batch,
        id: IncidentId,
        recovery: Recovery,
        project: ProjectId,
        cause: &Cause,
        now: saka_core::time::Timestamp,
    ) -> Result<()> {
        let current = self
            .store
            .read(|c| rows::load(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let events = incident::decide(&current, Input::Handle(recovery))?;
        self.record_incident(batch, current, &events, project, cause, now)?;
        Ok(())
    }

    fn record_incident(
        &self,
        batch: &mut Batch,
        current: Incident,
        events: &[saka_core::event::Event],
        project: ProjectId,
        cause: &Cause,
        now: saka_core::time::Timestamp,
    ) -> Result<Incident> {
        let after = incident::evolve_all(Some(current), events, now)
            .ok_or_else(|| EngineError::Internal("incident dropped by its events".into()))?;
        for event in events {
            self.push_event(batch, event, Some(project), cause, now);
            tracing::info!(
                incident_id = %after.id,
                state = after.state.as_str(),
                event = %event.kind(),
                "incident.step"
            );
        }
        batch.incidents.retain(|i| i.id != after.id);
        batch.incidents.push(after.clone());
        Ok(after)
    }

    pub async fn incident_draft_fix_with(
        &self,
        id: IncidentId,
        note: Option<&str>,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Draft> {
        if note.is_some_and(|n| n.chars().count() > NOTE_MAX.min(incident::DRAFT_NOTE_MAX_CHARS)) {
            return Err(crate::register::invalid("note", "too_long"));
        }
        let detail = self.incident(id)?;
        let found = &detail.row.incident;
        match found.state {
            IncidentState::Handling => return Err(Rejection::IncidentHandling.into()),
            IncidentState::Resolved | IncidentState::Dismissed => {
                return Err(Rejection::IncidentClosed.into());
            }
            IncidentState::Open => {}
        }
        let project = self.load_project_row(found.project_id)?;
        let source = self
            .incident_source_config(&found.source)
            .ok_or_else(|| crate::register::invalid("source", "unknown"))?;
        let repo = source
            .bind(&project.repos)
            .map(|binding| binding.name.clone())
            .map_err(|code| crate::register::invalid("repo", code))?;
        let suspects: Vec<DraftSuspect> = detail.suspects.iter().map(suspect_label).collect();
        let deployment = detail.row.deployment.as_ref();
        let idea = incident::draft_idea(
            &DraftFacts {
                incident: found,
                deployment_sha: deployment.and_then(|d| d.sha.as_deref()),
                previous_sha: deployment.and_then(|d| d.previous_sha.as_deref()),
                repo: Some(&repo),
                suspects: &suspects,
            },
            note,
        );
        let path = incident::draft_path(&project.project_path, &found.source, found.id);
        self.create_spec_draft(
            project.id,
            NewSpecDraft {
                path,
                idea,
                locale: DraftLocale::En,
                mode: DraftMode::Draft,
                target_kind: TargetKind::New,
                kind: WorkKind::Bugfix,
                context_paths: Vec::new(),
                deployment_id: None,
                incident_id: Some(id),
            },
            cause,
            action,
        )
        .await
    }

    pub fn incident_revert_with(
        &self,
        id: IncidentId,
        work_item_id: WorkItemId,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<()> {
        let detail = self.incident(id)?;
        match detail.row.incident.state {
            IncidentState::Handling => return Err(Rejection::IncidentHandling.into()),
            IncidentState::Resolved | IncidentState::Dismissed => {
                return Err(Rejection::IncidentClosed.into());
            }
            IncidentState::Open => {}
        }
        let suspected = detail.suspects.iter().any(|s| {
            matches!(s, Suspect::Task { work_item_id: suspect, .. } if *suspect == work_item_id)
        });
        if !suspected {
            return Err(Rejection::IncidentSuspectUnknown.into());
        }
        self.revert_linked(work_item_id, None, why, Some(id), cause, action)?;
        Ok(())
    }

    pub fn incident_dismiss_with(
        &self,
        id: IncidentId,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<IncidentRow> {
        let why = reason(why)?;
        let _guard = self.locked()?;
        let now = self.now();
        let current = self
            .store
            .read(|c| rows::load(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let project = current.project_id;
        let events = incident::decide(
            &current,
            Input::Dismiss {
                reason: why,
                actor: Some(cause.actor.clone()),
            },
        )?;
        let mut batch = Batch::new(now);
        let after = self.record_incident(&mut batch, current, &events, project, cause, now)?;
        let row = self.incident_row(after)?;
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Incident(&row));
            batch.action = Some(action.record(self, cause, Some(project), Some(id.0), reply));
        }
        self.commit(&batch)?;
        Ok(row)
    }

    pub(crate) async fn incident_draft_job(&self, job: &ClaimedJob) -> Execution {
        debug_assert_eq!(job.kind, JobKind::IncidentDraft);
        let id: IncidentId = field(&job.payload, "incident_id")?;
        let Some(found) = self
            .store
            .read(|c| rows::load(c, id))
            .map_err(|e| internal(e.into()))?
        else {
            return Ok(Effect::Skipped);
        };
        if found.state != IncidentState::Open || found.draft_id.is_some() {
            return Ok(Effect::Skipped);
        }
        let cause = self.cause(INTAKE_ACTOR);
        match self.incident_draft_fix_with(id, None, &cause, None).await {
            Ok(_) => Ok(Effect::Nothing),
            Err(EngineError::Rejected(Rejection::IncidentHandling | Rejection::IncidentClosed)) => {
                Ok(Effect::Skipped)
            }
            Err(e) => Err(internal(e)),
        }
    }

    pub(crate) fn reconcile_incidents(&self, cause: &Cause) -> Result<usize> {
        let handling = self.store.read(rows::handling)?;
        let mut changed = 0;
        for incident in handling {
            let Some(input) = self.recovery_input(&incident)? else {
                continue;
            };
            let _guard = self.locked()?;
            let Some(fresh) = self.store.read(|c| rows::load(c, incident.id))? else {
                continue;
            };
            if fresh.state != IncidentState::Handling {
                continue;
            }
            let now = self.now();
            let project = fresh.project_id;
            let events = match incident::decide(&fresh, input) {
                Ok(events) => events,
                Err(_) => continue,
            };
            let mut batch = Batch::new(now);
            self.record_incident(&mut batch, fresh, &events, project, cause, now)?;
            self.commit(&batch)?;
            changed += 1;
        }
        Ok(changed)
    }

    fn recovery_input(&self, incident: &Incident) -> Result<Option<Input>> {
        if let Some(draft_id) = incident.draft_id {
            return self.draft_recovery(draft_id);
        }
        let Some(revert_id) = incident.revert_id else {
            return Ok(None);
        };
        let task = self
            .store
            .read(|c| read::load_task(c, revert_id, &self.cfg.workflow_policy))?;
        Ok(match task.map(|t| t.item.state) {
            Some(WorkItemState::Done) => Some(Input::Resolve(IncidentResolution::Reverted)),
            Some(WorkItemState::Cancelled) => Some(Input::Release(IncidentCause::RevertCancelled)),
            _ => None,
        })
    }

    fn draft_recovery(&self, draft_id: SpecDraftId) -> Result<Option<Input>> {
        let Some(outcome) = self
            .store
            .read(|c| issue_intake::draft_outcome(c, draft_id))?
        else {
            return Ok(None);
        };
        Ok(match (outcome.state, outcome.spec_id) {
            (SpecDraftState::Failed | SpecDraftState::Cancelled, _) => {
                Some(Input::Release(IncidentCause::DraftFailed))
            }
            (SpecDraftState::Succeeded, Some(spec_id)) => {
                let state = self
                    .store
                    .read(|c| read::load_spec(c, spec_id))?
                    .map(|s| s.state);
                match state {
                    Some(SpecState::Shipped) => Some(Input::Resolve(IncidentResolution::Fixed)),
                    Some(SpecState::Withdrawn) => {
                        Some(Input::Release(IncidentCause::SpecWithdrawn))
                    }
                    _ => None,
                }
            }
            _ => None,
        })
    }
}
