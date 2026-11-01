use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::gate::Check;
use crate::ids::{
    BlockerId, ChangeRequestId, EnvironmentId, GateResultId, PermissionRequestId, ProjectId,
    RepoId, SessionId, SpecId, SpecRevisionId, StageRunId, WorkItemId,
};
use crate::time::Timestamp;
use crate::vocab::{
    Autonomy, BlockerCode, BlockerResolution, ChangeState, EnvironmentState, GateOutcome,
    GateSource, PermissionState, SessionState, SpecOrigin, SpecState, Stage, StagePhase,
    StageRunState, Trigger, WorkItemOrigin, WorkItemState, WorkflowPreset,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Repo {
    pub id: RepoId,
    pub integration_id: String,
    pub provider: String,
    pub full_name: String,
    pub url: String,
    pub default_branch: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub workspace_repo_id: RepoId,
    pub project_path: String,
    pub base_branch: String,
    pub config_commit: String,
    pub autonomy: Autonomy,
    pub workflow_preset: WorkflowPreset,
    pub repos: Vec<ProjectRepo>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectRepo {
    pub repo_id: RepoId,
    pub name: String,
    pub is_default: bool,
    pub path_scope: Vec<String>,
}

impl Project {
    pub fn default_repo(&self) -> Option<RepoId> {
        self.repos.iter().find(|r| r.is_default).map(|r| r.repo_id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Coded {
    pub code: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub id: SpecId,
    pub project_id: ProjectId,
    pub path: String,
    pub slug: String,
    pub title: String,
    pub state: SpecState,
    pub origin: SpecOrigin,
    pub priority: i64,
    pub latest_revision_id: SpecRevisionId,
    pub approved_revision_id: Option<SpecRevisionId>,
    pub approved_at: Option<Timestamp>,
    pub shipped_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepPlan {
    pub id: String,
    pub index: u32,
    pub work: String,
    #[serde(default)]
    pub after: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevisionView {
    pub id: SpecRevisionId,
    pub lint_ok: bool,
    pub issues: Value,
    pub steps: Vec<StepPlan>,
    pub parser_version: u32,
    pub priority: i64,
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependsOn {
    pub slug: String,
    pub spec_id: SpecId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecDependency {
    pub spec_id: SpecId,
    pub depends_on_spec_id: SpecId,
    pub depends_on_slug: String,
    pub revision_id: SpecRevisionId,
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItemDependency {
    pub work_item_id: WorkItemId,
    pub depends_on_work_item_id: WorkItemId,
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: WorkItemId,
    pub project_id: ProjectId,
    pub origin: WorkItemOrigin,
    pub spec_id: Option<SpecId>,
    pub spec_revision_id: Option<SpecRevisionId>,
    pub step_id: Option<String>,
    pub step_index: Option<u32>,
    pub task_key: String,
    pub work: String,
    pub repo_id: RepoId,
    pub branch: String,
    pub state: WorkItemState,
    pub stage: Option<Stage>,
    pub stage_phase: Option<StagePhase>,
    pub priority: i64,
    pub version: u64,
    pub admitted_at: Option<Timestamp>,
    pub done_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageRun {
    pub id: StageRunId,
    pub work_item_id: WorkItemId,
    pub stage: Stage,
    pub attempt: u32,
    pub state: StageRunState,
    pub trigger: Trigger,
    pub reason: Option<String>,
    pub entry_phase: StagePhase,
    pub head_sha: Option<String>,
    pub failure: Option<Coded>,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateResult {
    pub id: GateResultId,
    pub stage_run_id: StageRunId,
    pub work_item_id: WorkItemId,
    pub source: GateSource,
    pub head_sha: String,
    pub outcome: GateOutcome,
    pub checks: Vec<Check>,
    pub observed_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Blocker {
    pub id: BlockerId,
    pub work_item_id: WorkItemId,
    pub stage_run_id: Option<StageRunId>,
    pub code: BlockerCode,
    pub params: Value,
    pub resolution: Option<BlockerResolution>,
    pub opened_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeRequest {
    pub id: ChangeRequestId,
    pub work_item_id: WorkItemId,
    pub repo_id: RepoId,
    pub number: u64,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub head_sha: String,
    pub state: ChangeState,
    pub merge_commit_sha: Option<String>,
    pub merged_at: Option<Timestamp>,
    pub last_polled_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    pub id: EnvironmentId,
    pub work_item_id: WorkItemId,
    pub repo_id: RepoId,
    pub root_path: String,
    pub worktree_path: String,
    pub branch: String,
    pub base_sha: Option<String>,
    pub state: EnvironmentState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub work_item_id: WorkItemId,
    pub stage_run_id: StageRunId,
    pub environment_id: EnvironmentId,
    pub cli: String,
    pub cli_version: Option<String>,
    pub cli_session_id: Option<String>,
    pub model: Option<String>,
    pub account: String,
    pub state: SessionState,
    pub runner_unit: String,
    pub socket_path: String,
    pub log_path: String,
    pub turns: u32,
    pub followups: u32,
    pub running_ms: i64,
    pub running_since: Option<Timestamp>,
    pub cost_usd_micros: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub end_reason: Option<Coded>,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Session {
    pub fn running_ms_at(&self, now: Timestamp) -> i64 {
        self.running_ms + self.running_since.map_or(0, |since| now.since(since))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: PermissionRequestId,
    pub work_item_id: WorkItemId,
    pub session_id: SessionId,
    pub cli_request_id: String,
    pub tool: String,
    pub input: Value,
    pub summary: String,
    pub state: PermissionState,
    pub answer_source: Option<String>,
    pub message: Option<String>,
    pub requested_at: Timestamp,
    pub answered_at: Option<Timestamp>,
    pub delivered_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub item: WorkItem,
    pub stage_run: Option<StageRun>,
    pub gate: Option<GateResult>,
    pub blocker: Option<Blocker>,
    pub change_request: Option<ChangeRequest>,
    pub environment: Option<Environment>,
    pub session: Option<Session>,
    pub permissions: Vec<PermissionRequest>,
    pub attempts: BTreeMap<Stage, u32>,
    pub retired_runs: Vec<StageRun>,
}

impl Task {
    pub fn open_blocker(&self) -> Option<&Blocker> {
        self.blocker.as_ref().filter(|b| b.resolution.is_none())
    }

    pub fn live_session(&self) -> Option<&Session> {
        self.session.as_ref().filter(|s| s.state.is_live())
    }

    pub fn ready_environment(&self) -> Option<&Environment> {
        self.environment
            .as_ref()
            .filter(|e| e.state == EnvironmentState::Ready)
    }

    pub fn is_at(&self, stage: Stage, phase: StagePhase) -> bool {
        self.item.state == WorkItemState::InStage
            && self.item.stage == Some(stage)
            && self.item.stage_phase == Some(phase)
    }
}
