//! Domain events. The serialized form is `{"kind": "<context>.<entity>.<action>", "payload": {…}}`;
//! every kind here is version 1. Payloads carry ids and the data needed to rebuild the
//! projections, never secrets.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::gate::Check;
use crate::ids::{
    BlockerId, ChangeRequestId, EnvironmentId, GateResultId, PermissionRequestId, ProjectId,
    RepoId, SessionId, SpecId, SpecRevisionId, StageRunId, WorkItemId,
};
use crate::model::{Coded, ProjectRepo};
use crate::time::Timestamp;
use crate::vocab::{
    BlockerCode, BlockerResolution, GateOutcome, MergeMethod, PermissionState, SessionState, Stage,
    StagePhase, Trigger,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload")]
pub enum Event {
    #[serde(rename = "workspace.repo.registered")]
    RepoRegistered {
        repo_id: RepoId,
        integration_id: String,
        provider: String,
        full_name: String,
        url: String,
        default_branch: String,
    },
    #[serde(rename = "workspace.project.registered")]
    ProjectRegistered {
        project_id: ProjectId,
        name: String,
        workspace_repo_id: RepoId,
        project_path: String,
        base_branch: String,
        config_commit: String,
        repos: Vec<ProjectRepo>,
    },
    #[serde(rename = "spec.spec.registered")]
    SpecRegistered {
        spec_id: SpecId,
        project_id: ProjectId,
        path: String,
        slug: String,
    },
    #[serde(rename = "spec.revision.recorded")]
    SpecRevisionRecorded {
        spec_id: SpecId,
        revision_id: SpecRevisionId,
        source_commit: String,
        content_hash: String,
        lint_ok: bool,
        issue_codes: Vec<String>,
        title: String,
        /// The same content was recorded before; `revision_id` is that earlier revision.
        reused: bool,
    },
    #[serde(rename = "spec.spec.approved")]
    SpecApproved {
        spec_id: SpecId,
        revision_id: SpecRevisionId,
        reason: Option<String>,
    },
    #[serde(rename = "spec.spec.shipped")]
    SpecShipped {
        spec_id: SpecId,
        revision_id: SpecRevisionId,
    },

    #[serde(rename = "delivery.work_item.created")]
    WorkItemCreated {
        work_item_id: WorkItemId,
        project_id: ProjectId,
        spec_id: SpecId,
        spec_revision_id: SpecRevisionId,
        step_id: String,
        step_index: u32,
        task_key: String,
        work: String,
        branch: String,
        repo_id: RepoId,
        priority: i64,
    },
    #[serde(rename = "delivery.work_item.admitted")]
    WorkItemAdmitted { work_item_id: WorkItemId },
    #[serde(rename = "delivery.stage_run.created")]
    StageRunCreated {
        work_item_id: WorkItemId,
        stage_run_id: StageRunId,
        stage: Stage,
        attempt: u32,
        trigger: Trigger,
        entry_phase: StagePhase,
        reason: Option<String>,
        /// Set when the run retries a blocker; the session prompt uses it.
        retry_of: Option<Coded>,
    },
    #[serde(rename = "delivery.stage_run.phase_changed")]
    StagePhaseChanged {
        work_item_id: WorkItemId,
        stage_run_id: StageRunId,
        phase: StagePhase,
        head_sha: Option<String>,
    },
    #[serde(rename = "delivery.stage_run.succeeded")]
    StageRunSucceeded {
        work_item_id: WorkItemId,
        stage_run_id: StageRunId,
        head_sha: Option<String>,
    },
    #[serde(rename = "delivery.gate.observed")]
    GateObserved {
        work_item_id: WorkItemId,
        stage_run_id: StageRunId,
        gate_result_id: GateResultId,
        head_sha: String,
        outcome: GateOutcome,
        checks: Vec<Check>,
    },
    #[serde(rename = "delivery.merge.requested")]
    MergeRequested {
        work_item_id: WorkItemId,
        head_sha: String,
        merge_method: MergeMethod,
        reason: Option<String>,
    },
    #[serde(rename = "delivery.work_item.blocked")]
    WorkItemBlocked {
        work_item_id: WorkItemId,
        stage_run_id: Option<StageRunId>,
        blocker_id: BlockerId,
        code: BlockerCode,
        params: Value,
        stage: Option<Stage>,
        phase: Option<StagePhase>,
    },
    #[serde(rename = "delivery.blocker.resolved")]
    BlockerResolved {
        work_item_id: WorkItemId,
        blocker_id: BlockerId,
        resolution: BlockerResolution,
        reason: Option<String>,
    },
    #[serde(rename = "delivery.work_item.cancelled")]
    WorkItemCancelled {
        work_item_id: WorkItemId,
        reason: String,
    },
    #[serde(rename = "delivery.work_item.done")]
    WorkItemDone {
        work_item_id: WorkItemId,
        merge_commit_sha: String,
    },

    #[serde(rename = "runtime.environment.prepared")]
    EnvironmentPrepared {
        work_item_id: WorkItemId,
        environment_id: EnvironmentId,
        repo_id: RepoId,
        root_path: String,
        worktree_path: String,
        branch: String,
        base_sha: String,
    },
    #[serde(rename = "runtime.environment.torn_down")]
    EnvironmentTornDown {
        work_item_id: WorkItemId,
        environment_id: EnvironmentId,
    },
    #[serde(rename = "runtime.session.started")]
    SessionStarted {
        work_item_id: WorkItemId,
        session_id: SessionId,
        stage_run_id: StageRunId,
        environment_id: EnvironmentId,
        cli: String,
        cli_version: Option<String>,
        model: Option<String>,
        account: String,
        runner_unit: String,
        socket_path: String,
        log_path: String,
    },
    #[serde(rename = "runtime.session.initialized")]
    SessionInitialized {
        work_item_id: WorkItemId,
        session_id: SessionId,
        cli_session_id: String,
        model: Option<String>,
    },
    #[serde(rename = "runtime.session.state_changed")]
    SessionStateChanged {
        work_item_id: WorkItemId,
        session_id: SessionId,
        state: SessionState,
        running_ms: i64,
    },
    /// Saka decided to send a turn; a job delivers it to the runner.
    #[serde(rename = "runtime.session.turn_requested")]
    SessionTurnRequested {
        work_item_id: WorkItemId,
        session_id: SessionId,
        turn: u32,
        prompt: Prompt,
    },
    #[serde(rename = "runtime.session.turn_ended")]
    SessionTurnEnded {
        work_item_id: WorkItemId,
        session_id: SessionId,
        turn: u32,
        status: String,
        cost_usd_micros: Option<i64>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
    },
    #[serde(rename = "runtime.session.ended")]
    SessionEnded {
        work_item_id: WorkItemId,
        session_id: SessionId,
        end_reason: Coded,
    },
    #[serde(rename = "runtime.session.lost")]
    SessionLost {
        work_item_id: WorkItemId,
        session_id: SessionId,
        cause: String,
    },
    #[serde(rename = "runtime.permission.requested")]
    PermissionRequested {
        work_item_id: WorkItemId,
        session_id: SessionId,
        permission_request_id: PermissionRequestId,
        cli_request_id: String,
        tool: String,
        summary: String,
        input: Value,
    },
    #[serde(rename = "runtime.permission.answered")]
    PermissionAnswered {
        work_item_id: WorkItemId,
        permission_request_id: PermissionRequestId,
        state: PermissionState,
        answer_source: String,
        message: Option<String>,
    },
    #[serde(rename = "runtime.permission.delivered")]
    PermissionDelivered {
        work_item_id: WorkItemId,
        permission_request_id: PermissionRequestId,
    },

    #[serde(rename = "scm.branch.pushed")]
    BranchPushed {
        work_item_id: WorkItemId,
        branch: String,
        sha: String,
    },
    #[serde(rename = "scm.change_request.opened")]
    ChangeRequestOpened {
        work_item_id: WorkItemId,
        change_request_id: ChangeRequestId,
        repo_id: RepoId,
        number: u64,
        url: String,
        head_branch: String,
        base_branch: String,
        head_sha: String,
    },
    #[serde(rename = "scm.change_request.updated")]
    ChangeRequestUpdated {
        work_item_id: WorkItemId,
        change_request_id: ChangeRequestId,
        head_sha: String,
    },
    #[serde(rename = "scm.change_request.merged")]
    ChangeRequestMerged {
        work_item_id: WorkItemId,
        change_request_id: ChangeRequestId,
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
        merged_by_saka: bool,
    },
    #[serde(rename = "scm.change_request.closed")]
    ChangeRequestClosed {
        work_item_id: WorkItemId,
        change_request_id: ChangeRequestId,
    },
}

/// What a turn asks of the agent. The runtime renders the text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Prompt {
    /// The work packet: task, criteria, commands, and the pinned spec.
    WorkPacket { retry_of: Option<Coded> },
    /// The worktree is dirty or has no commit yet.
    Uncommitted { dirty: bool, commits_ahead: u32 },
}

/// The aggregate an event belongs to, as `(stream_type, stream_id)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Repo(RepoId),
    Project(ProjectId),
    Spec(SpecId),
    WorkItem(WorkItemId),
    Session(SessionId),
    PermissionRequest(PermissionRequestId),
}

impl Stream {
    pub fn kind(self) -> &'static str {
        match self {
            Stream::Repo(_) => "repo",
            Stream::Project(_) => "project",
            Stream::Spec(_) => "spec",
            Stream::WorkItem(_) => "work_item",
            Stream::Session(_) => "session",
            Stream::PermissionRequest(_) => "permission_request",
        }
    }

    pub fn id(self) -> uuid::Uuid {
        match self {
            Stream::Repo(id) => id.0,
            Stream::Project(id) => id.0,
            Stream::Spec(id) => id.0,
            Stream::WorkItem(id) => id.0,
            Stream::Session(id) => id.0,
            Stream::PermissionRequest(id) => id.0,
        }
    }
}

impl Event {
    pub const VERSION: u32 = 1;

    /// The wire kind, e.g. `delivery.work_item.admitted`.
    pub fn kind(&self) -> String {
        match serde_json::to_value(self) {
            Ok(Value::Object(map)) => map
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            _ => String::new(),
        }
    }

    /// The payload object without the kind.
    pub fn payload(&self) -> Value {
        match serde_json::to_value(self) {
            Ok(Value::Object(mut map)) => map.remove("payload").unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    pub fn stream(&self) -> Stream {
        use Event as E;
        match self {
            E::RepoRegistered { repo_id, .. } => Stream::Repo(*repo_id),
            E::ProjectRegistered { project_id, .. } => Stream::Project(*project_id),
            E::SpecRegistered { spec_id, .. }
            | E::SpecRevisionRecorded { spec_id, .. }
            | E::SpecApproved { spec_id, .. }
            | E::SpecShipped { spec_id, .. } => Stream::Spec(*spec_id),
            E::SessionStarted { session_id, .. }
            | E::SessionInitialized { session_id, .. }
            | E::SessionStateChanged { session_id, .. }
            | E::SessionTurnRequested { session_id, .. }
            | E::SessionTurnEnded { session_id, .. }
            | E::SessionEnded { session_id, .. }
            | E::SessionLost { session_id, .. } => Stream::Session(*session_id),
            E::PermissionRequested {
                permission_request_id,
                ..
            }
            | E::PermissionAnswered {
                permission_request_id,
                ..
            }
            | E::PermissionDelivered {
                permission_request_id,
                ..
            } => Stream::PermissionRequest(*permission_request_id),
            E::WorkItemCreated { work_item_id, .. }
            | E::WorkItemAdmitted { work_item_id }
            | E::StageRunCreated { work_item_id, .. }
            | E::StagePhaseChanged { work_item_id, .. }
            | E::StageRunSucceeded { work_item_id, .. }
            | E::GateObserved { work_item_id, .. }
            | E::MergeRequested { work_item_id, .. }
            | E::WorkItemBlocked { work_item_id, .. }
            | E::BlockerResolved { work_item_id, .. }
            | E::WorkItemCancelled { work_item_id, .. }
            | E::WorkItemDone { work_item_id, .. }
            | E::EnvironmentPrepared { work_item_id, .. }
            | E::EnvironmentTornDown { work_item_id, .. }
            | E::BranchPushed { work_item_id, .. }
            | E::ChangeRequestOpened { work_item_id, .. }
            | E::ChangeRequestUpdated { work_item_id, .. }
            | E::ChangeRequestMerged { work_item_id, .. }
            | E::ChangeRequestClosed { work_item_id, .. } => Stream::WorkItem(*work_item_id),
        }
    }

    /// The task an event concerns, if any.
    pub fn work_item_id(&self) -> Option<WorkItemId> {
        use Event as E;
        match self {
            E::SpecRegistered { .. }
            | E::SpecRevisionRecorded { .. }
            | E::SpecApproved { .. }
            | E::SpecShipped { .. } => None,
            E::SessionStarted { work_item_id, .. }
            | E::SessionInitialized { work_item_id, .. }
            | E::SessionStateChanged { work_item_id, .. }
            | E::SessionTurnRequested { work_item_id, .. }
            | E::SessionTurnEnded { work_item_id, .. }
            | E::SessionEnded { work_item_id, .. }
            | E::SessionLost { work_item_id, .. }
            | E::PermissionRequested { work_item_id, .. }
            | E::PermissionAnswered { work_item_id, .. }
            | E::PermissionDelivered { work_item_id, .. } => Some(*work_item_id),
            _ => match self.stream() {
                Stream::WorkItem(id) => Some(id),
                _ => None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    #[test]
    fn kind_and_payload_are_split() {
        let e = Event::WorkItemAdmitted {
            work_item_id: WorkItemId(Uuid::nil()),
        };
        assert_eq!(e.kind(), "delivery.work_item.admitted");
        assert_eq!(
            e.payload(),
            serde_json::json!({ "work_item_id": Uuid::nil() })
        );
        assert_eq!(e.stream(), Stream::WorkItem(WorkItemId(Uuid::nil())));
        let back: Event = serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }
}
