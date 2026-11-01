use uuid::Uuid;

use crate::Ctx;
use crate::event::Event;
use crate::ids::{ProjectId, RepoId, SpecId, SpecRevisionId, WorkItemId};
use crate::model::{RevisionView, Spec};
use crate::rejection::Rejection;
use crate::time::Timestamp;
use crate::vocab::{SpecOrigin, SpecState};

#[derive(Debug, Clone, PartialEq)]
pub struct NewRevision {
    pub source_commit: String,
    pub content_hash: String,
    pub lint_ok: bool,
    pub issue_codes: Vec<String>,
    pub title: String,
}

pub fn record_revision(
    existing: Option<&Spec>,
    project_id: ProjectId,
    path: &str,
    slug: &str,
    revision: NewRevision,
    known: Option<SpecRevisionId>,
    ctx: &mut Ctx<'_>,
) -> Vec<Event> {
    let mut events = Vec::new();
    let spec_id = match existing {
        Some(spec) => {
            if known == Some(spec.latest_revision_id) {
                return events;
            }
            spec.id
        }
        None => {
            let id = SpecId::new(ctx.ids);
            events.push(Event::SpecRegistered {
                spec_id: id,
                project_id,
                path: path.to_string(),
                slug: slug.to_string(),
            });
            id
        }
    };
    events.push(Event::SpecRevisionRecorded {
        spec_id,
        revision_id: known.unwrap_or_else(|| SpecRevisionId::new(ctx.ids)),
        source_commit: revision.source_commit,
        content_hash: revision.content_hash,
        lint_ok: revision.lint_ok,
        issue_codes: revision.issue_codes,
        title: revision.title,
        reused: known.is_some(),
    });
    events
}

pub fn approve(
    spec: &Spec,
    latest: &RevisionView,
    if_match: SpecRevisionId,
    reason: Option<String>,
    default_repo: RepoId,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<Event>, Rejection> {
    if if_match != spec.latest_revision_id || latest.id != spec.latest_revision_id {
        return Err(Rejection::RevisionStale);
    }
    if !latest.lint_ok {
        return Err(Rejection::SpecNotReady {
            issues: latest.issues.clone(),
        });
    }
    match spec.state {
        SpecState::Ready => {}
        SpecState::Approved => return Err(Rejection::SpecAlreadyApproved),
        other => {
            return Err(Rejection::InvalidState {
                state: other.as_str().to_string(),
            });
        }
    }
    let mut events = vec![Event::SpecApproved {
        spec_id: spec.id,
        revision_id: latest.id,
        reason,
    }];
    for step in &latest.steps {
        let key = format!("{}/s{}", spec.slug, step.index);
        events.push(Event::WorkItemCreated {
            work_item_id: WorkItemId::new(ctx.ids),
            project_id: spec.project_id,
            spec_id: spec.id,
            spec_revision_id: latest.id,
            step_id: step.id.clone(),
            step_index: step.index,
            branch: format!("saka/{key}"),
            task_key: key,
            work: step.work.clone(),
            repo_id: default_repo,
            priority: spec.priority,
        });
    }
    Ok(events)
}

pub fn ship(spec: &Spec, all_tasks_done: bool) -> Result<Vec<Event>, Rejection> {
    match (spec.state, spec.approved_revision_id) {
        (SpecState::Approved, Some(revision_id)) if all_tasks_done => {
            Ok(vec![Event::SpecShipped {
                spec_id: spec.id,
                revision_id,
            }])
        }
        (state, _) => Err(Rejection::InvalidState {
            state: state.as_str().to_string(),
        }),
    }
}

pub fn evolve(spec: Option<Spec>, event: &Event, now: Timestamp) -> Option<Spec> {
    match (spec, event) {
        (
            None,
            Event::SpecRegistered {
                spec_id,
                project_id,
                path,
                slug,
            },
        ) => Some(Spec {
            id: *spec_id,
            project_id: *project_id,
            path: path.clone(),
            slug: slug.clone(),
            title: String::new(),
            state: SpecState::Draft,
            origin: SpecOrigin::Manual,
            priority: 0,
            latest_revision_id: SpecRevisionId(Uuid::nil()),
            approved_revision_id: None,
            approved_at: None,
            shipped_at: None,
            created_at: now,
            updated_at: now,
        }),
        (
            Some(mut s),
            Event::SpecRevisionRecorded {
                revision_id,
                lint_ok,
                title,
                ..
            },
        ) => {
            s.latest_revision_id = *revision_id;
            s.title = title.clone();
            if matches!(s.state, SpecState::Draft | SpecState::Ready) {
                s.state = if *lint_ok {
                    SpecState::Ready
                } else {
                    SpecState::Draft
                };
            }
            s.updated_at = now;
            Some(s)
        }
        (Some(mut s), Event::SpecApproved { revision_id, .. }) => {
            s.state = SpecState::Approved;
            s.approved_revision_id = Some(*revision_id);
            s.approved_at = Some(now);
            s.updated_at = now;
            Some(s)
        }
        (Some(mut s), Event::SpecShipped { .. }) => {
            s.state = SpecState::Shipped;
            s.shipped_at = Some(now);
            s.updated_at = now;
            Some(s)
        }
        (spec, _) => spec,
    }
}
