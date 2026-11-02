use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::gate::Check;
use crate::ids::{
    BlockerId, ChangeRequestId, EnvironmentId, GateResultId, PermissionRequestId, PermissionRuleId,
    ProjectId, RepoId, SessionId, SpecId, SpecRevisionId, StageRunId, UsageRecordId, WorkItemId,
};
use crate::time::Timestamp;
use crate::vocab::{
    Autonomy, BlockerCode, BlockerResolution, BudgetDimension, ChangeState, EnvironmentState,
    GateOutcome, GateSource, InterruptBy, PermissionState, ResumeCause, SessionState, SpecOrigin,
    SpecState, Stage, StagePhase, StageRunState, Trigger, WorkItemOrigin, WorkItemState,
    WorkflowPreset,
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
    pub ac: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
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
    pub budget: Option<Budget>,
    pub budget_warnings: Vec<BudgetDimension>,
    pub resumed_lost: bool,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub running_minutes: u32,
    pub turns: u32,
    pub tokens: i64,
    pub cost_usd_micros: i64,
}

impl Budget {
    pub fn limit(&self, dimension: BudgetDimension) -> i64 {
        match dimension {
            BudgetDimension::RunningMinutes => i64::from(self.running_minutes) * 60_000,
            BudgetDimension::Turns => i64::from(self.turns),
            BudgetDimension::Tokens => self.tokens,
            BudgetDimension::CostUsd => self.cost_usd_micros,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub running_ms: i64,
    pub turns: u32,
    pub tokens: i64,
    pub cost_usd_micros: i64,
}

impl Usage {
    pub fn used(&self, dimension: BudgetDimension) -> i64 {
        match dimension {
            BudgetDimension::RunningMinutes => self.running_ms,
            BudgetDimension::Turns => i64::from(self.turns),
            BudgetDimension::Tokens => self.tokens,
            BudgetDimension::CostUsd => self.cost_usd_micros,
        }
    }

    pub fn add(&mut self, other: Usage) {
        self.running_ms += other.running_ms;
        self.turns += other.turns;
        self.tokens += other.tokens;
        self.cost_usd_micros += other.cost_usd_micros;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub id: UsageRecordId,
    pub work_item_id: WorkItemId,
    pub stage_run_id: StageRunId,
    pub session_id: SessionId,
    pub account: String,
    pub turn: u32,
    pub status: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cost_usd_micros: Option<i64>,
    pub running_ms: i64,
    pub ended_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionActivity {
    pub session_id: SessionId,
    pub offset: u64,
    pub work_item_id: WorkItemId,
    pub at: Timestamp,
    pub kind: String,
    pub summary: String,
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountQuota {
    pub account: String,
    pub window: String,
    pub utilization: f64,
    pub status: String,
    pub resets_at: Option<Timestamp>,
    pub observed_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub id: PermissionRuleId,
    pub project_id: ProjectId,
    pub tool: String,
    pub command: Option<String>,
    pub permission_request_id: PermissionRequestId,
    pub created_by: String,
    pub created_at: Timestamp,
    pub revoked_at: Option<Timestamp>,
    pub revoke_reason: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EnvironmentSetup {
    pub port_start: Option<u16>,
    pub port_count: Option<u16>,
    pub recipe: Option<Value>,
    pub recipe_sha256: Option<String>,
    pub setup: Vec<Value>,
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
    pub port_start: Option<u16>,
    pub port_count: Option<u16>,
    pub recipe: Option<Value>,
    pub recipe_sha256: Option<String>,
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
    pub resumed_from_session_id: Option<SessionId>,
    pub resume_cause: Option<ResumeCause>,
    pub idle_since: Option<Timestamp>,
    pub interrupt_requested: Option<InterruptBy>,
    pub steers: u32,
    pub last_offset: u64,
    pub recorded_turn: u32,
    pub recorded_running_ms: i64,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Session {
    pub fn running_ms_at(&self, now: Timestamp) -> i64 {
        self.running_ms + self.running_since.map_or(0, |since| now.since(since))
    }

    pub fn usage_at(&self, now: Timestamp) -> Usage {
        Usage {
            running_ms: self.running_ms_at(now),
            turns: self.turns,
            tokens: self.input_tokens.unwrap_or(0) + self.output_tokens.unwrap_or(0),
            cost_usd_micros: self.cost_usd_micros.unwrap_or(0),
        }
    }

    pub fn qualified_account(&self) -> String {
        format!("{}:{}", self.cli, self.account)
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
    pub retired_sessions: Vec<Session>,
    pub prior_usage: Usage,
    pub usage_records: Vec<UsageRecord>,
    pub quota_observations: Vec<AccountQuota>,
    pub added_rules: Vec<PermissionRule>,
}

impl Task {
    pub fn new(item: WorkItem) -> Task {
        Task {
            item,
            stage_run: None,
            gate: None,
            blocker: None,
            change_request: None,
            environment: None,
            session: None,
            permissions: Vec::new(),
            attempts: BTreeMap::new(),
            retired_runs: Vec::new(),
            retired_sessions: Vec::new(),
            prior_usage: Usage::default(),
            usage_records: Vec::new(),
            quota_observations: Vec::new(),
            added_rules: Vec::new(),
        }
    }

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

    pub fn stage_usage(&self, now: Timestamp) -> Usage {
        let mut usage = self.prior_usage;
        if let (Some(s), Some(run)) = (&self.session, &self.stage_run)
            && s.stage_run_id == run.id
        {
            usage.add(s.usage_at(now));
        }
        usage
    }

    pub fn is_at(&self, stage: Stage, phase: StagePhase) -> bool {
        self.item.state == WorkItemState::InStage
            && self.item.stage == Some(stage)
            && self.item.stage_phase == Some(phase)
    }
}
