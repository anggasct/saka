use serde_json::{Value, json};

use crate::autonomy::Ceilings;
use crate::blocker::primary_action;
use crate::ids::{ProjectId, SpecDraftId, SpecId, WorkItemId};
use crate::model::{Blocker, DecisionRequest, Draft, PermissionRequest, Spec, Task};
use crate::task::{auto_merge_enabled, merge_ready};
use crate::vocab::{
    BlockerCode, DecisionState, InboxActionKind, InboxKind, InboxState, InboxSubject,
    PermissionState, SpecState, Stage, StagePhase, WorkItemOrigin, WorkItemState,
};

#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    Task(&'a Task),
    Draft(&'a Draft),
    Spec(&'a Spec),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    WorkItem(WorkItemId),
    Draft(SpecDraftId),
    Spec(SpecId),
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboxAction {
    pub kind: InboxActionKind,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboxDraft {
    pub kind: InboxKind,
    pub subject: InboxSubject,
    pub source_id: String,
    pub state: InboxState,
    pub project_id: ProjectId,
    pub work_item_id: Option<WorkItemId>,
    pub spec_id: Option<SpecId>,
    pub params: Value,
    pub action: InboxAction,
}

pub fn scope(source: Source<'_>) -> Scope {
    match source {
        Source::Task(task) => Scope::WorkItem(task.item.id),
        Source::Draft(draft) => Scope::Draft(draft.id()),
        Source::Spec(spec) => Scope::Spec(spec.id),
    }
}

pub fn dismissible(kind: InboxKind, subject: InboxSubject) -> bool {
    kind == InboxKind::Approval && subject == InboxSubject::Spec
}

pub fn desired(source: Source<'_>, ceilings: &Ceilings) -> Vec<InboxDraft> {
    match source {
        Source::Task(task) => for_task(task, ceilings),
        Source::Draft(draft) => for_draft(draft),
        Source::Spec(spec) => for_spec(spec),
    }
}

fn for_task(task: &Task, ceilings: &Ceilings) -> Vec<InboxDraft> {
    if matches!(
        task.item.state,
        WorkItemState::TakenOver
            | WorkItemState::Done
            | WorkItemState::Cancelled
            | WorkItemState::Superseded
    ) {
        return Vec::new();
    }
    let live = task.live_session().map(|s| s.id);
    let mut out: Vec<InboxDraft> = task
        .permissions
        .iter()
        .filter(|p| p.state == PermissionState::Pending && live == Some(p.session_id))
        .map(|p| permission_item(task.item.project_id, Some(task.item.id), p))
        .collect();
    let owner = task.run_owner();
    out.extend(
        task.decisions
            .iter()
            .filter(|d| Some(d.owner) == owner && d.is_open())
            .map(|d| decision_item(task.item.project_id, Some(task.item.id), d)),
    );
    if let Some(blocker) = task.open_blocker()
        && let Some(primary) = primary_action(task)
    {
        let mut params = primary.params;
        if let Value::Object(map) = &mut params {
            map.insert("blocker_id".into(), json!(blocker.id));
        }
        let unclear = blocker.code == BlockerCode::SpecUnclear;
        out.push(InboxDraft {
            kind: if unclear {
                InboxKind::SpecUnclear
            } else {
                InboxKind::Blocker
            },
            subject: InboxSubject::Blocker,
            source_id: blocker.id.to_string(),
            state: InboxState::Open,
            project_id: task.item.project_id,
            work_item_id: Some(task.item.id),
            spec_id: task.item.spec_id,
            params: blocker_params(task, blocker),
            action: InboxAction {
                kind: primary.kind.into(),
                params,
            },
        });
    }
    if task.item.origin == WorkItemOrigin::SpecStep {
        if task.item.state == WorkItemState::AwaitingStart {
            out.push(step_item(
                task,
                InboxSubject::StepStart,
                InboxActionKind::StartStep,
            ));
        }
        if task.is_at(Stage::Merge, StagePhase::AwaitingMerge)
            && !auto_merge_enabled(task, ceilings)
            && merge_ready(task)
        {
            out.push(step_item(task, InboxSubject::Merge, InboxActionKind::Merge));
        }
    }
    out
}

fn blocker_params(task: &Task, blocker: &Blocker) -> Value {
    let mut params = match &blocker.params {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    params.insert("code".into(), json!(blocker.code));
    if !params.contains_key("head_sha") {
        let head = task.stage_run.as_ref().and_then(|r| r.head_sha.clone());
        params.insert("head_sha".into(), json!(head));
    }
    Value::Object(params)
}

fn step_item(task: &Task, subject: InboxSubject, action: InboxActionKind) -> InboxDraft {
    InboxDraft {
        kind: InboxKind::Approval,
        subject,
        source_id: task.item.id.to_string(),
        state: InboxState::Open,
        project_id: task.item.project_id,
        work_item_id: Some(task.item.id),
        spec_id: task.item.spec_id,
        params: json!({
            "head_sha": task.change_request.as_ref().map(|cr| cr.head_sha.clone()),
        }),
        action: InboxAction {
            kind: action,
            params: json!({ "work_item_id": task.item.id, "version": task.item.version }),
        },
    }
}

fn for_draft(draft: &Draft) -> Vec<InboxDraft> {
    let live = draft.live_session().map(|s| s.id);
    let owner = draft.run_owner();
    let mut out: Vec<InboxDraft> = draft
        .permissions
        .iter()
        .filter(|p| p.state == PermissionState::Pending && live == Some(p.session_id))
        .map(|p| permission_item(draft.draft.project_id, None, p))
        .collect();
    out.extend(
        draft
            .decisions
            .iter()
            .filter(|d| Some(d.owner) == owner && d.is_open())
            .map(|d| decision_item(draft.draft.project_id, None, d)),
    );
    out
}

fn decision_item(
    project_id: ProjectId,
    work_item_id: Option<WorkItemId>,
    d: &DecisionRequest,
) -> InboxDraft {
    InboxDraft {
        kind: InboxKind::Decision,
        subject: InboxSubject::Decision,
        source_id: d.id.to_string(),
        state: if d.state == DecisionState::Answered {
            InboxState::Answered
        } else {
            InboxState::Open
        },
        project_id,
        work_item_id,
        spec_id: None,
        params: json!({
            "question": d.question,
            "context": d.context,
            "options": d.options,
            "expires_at": d.expires_at,
        }),
        action: InboxAction {
            kind: InboxActionKind::AnswerDecision,
            params: json!({ "decision_request_id": d.id }),
        },
    }
}

fn permission_item(
    project_id: ProjectId,
    work_item_id: Option<WorkItemId>,
    p: &PermissionRequest,
) -> InboxDraft {
    InboxDraft {
        kind: InboxKind::Permission,
        subject: InboxSubject::Permission,
        source_id: p.id.to_string(),
        state: InboxState::Open,
        project_id,
        work_item_id,
        spec_id: None,
        params: json!({ "tool": p.tool, "summary": p.summary }),
        action: InboxAction {
            kind: InboxActionKind::AnswerPermission,
            params: json!({ "permission_request_id": p.id }),
        },
    }
}

fn for_spec(spec: &Spec) -> Vec<InboxDraft> {
    if spec.state != SpecState::Ready {
        return Vec::new();
    }
    vec![InboxDraft {
        kind: InboxKind::Approval,
        subject: InboxSubject::Spec,
        source_id: format!("{}:{}", spec.id, spec.latest_revision_id),
        state: InboxState::Open,
        project_id: spec.project_id,
        work_item_id: None,
        spec_id: Some(spec.id),
        params: json!({ "revision": spec.latest_revision_id }),
        action: InboxAction {
            kind: InboxActionKind::ApproveSpec,
            params: json!({ "spec_id": spec.id, "revision": spec.latest_revision_id }),
        },
    }]
}

impl From<crate::vocab::BlockerAction> for InboxActionKind {
    fn from(action: crate::vocab::BlockerAction) -> InboxActionKind {
        use crate::vocab::BlockerAction as B;
        match action {
            B::Retry => InboxActionKind::Retry,
            B::Cancel => InboxActionKind::Cancel,
            B::Takeover => InboxActionKind::Takeover,
            B::SetSecret => InboxActionKind::SetSecret,
            B::LinkAccount => InboxActionKind::LinkAccount,
            B::SetUserToken => InboxActionKind::SetUserToken,
            B::EditSpec => InboxActionKind::EditSpec,
            B::UpdateBranch => InboxActionKind::UpdateBranch,
        }
    }
}
