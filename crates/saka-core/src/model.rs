use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::autonomy::RepoPolicy;
use crate::decision::DecisionOption;
use crate::gate::Check;
use crate::ids::{
    BlockerId, ChangeRequestId, DecisionRequestId, EnvironmentId, GateResultId,
    PermissionRequestId, PermissionRuleId, ProjectId, RepoId, ReviewId, SessionId,
    SpecDraftAttemptId, SpecDraftId, SpecId, SpecRevisionId, StageRunId, UsageRecordId, WorkItemId,
};
use crate::review::Finding;
use crate::time::Timestamp;
use crate::vocab::{
    Autonomy, BlockerCode, BlockerResolution, BudgetDimension, ChangeRole, ChangeState,
    ContextOrigin, DecisionState, DocsMode, DraftAttemptState, DraftLocale, DraftMode, Effect,
    EnvironmentState, ExternalCause, GateOutcome, GateSource, InterruptBy, LintOutcome, MatchKind,
    PacketOrigin, PermissionState, ResumeCause, ReviewBasis, ReviewEvent, ReviewOutcome,
    SessionState, SpecDraftState, SpecOrigin, SpecState, Stage, StagePhase, StageRunState,
    TargetKind, Tool, Trigger, WorkItemOrigin, WorkItemState, WorkKind,
};
use crate::workflow::Workflow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunOwner {
    StageRun {
        work_item_id: WorkItemId,
        stage_run_id: StageRunId,
    },
    DraftAttempt {
        spec_draft_id: SpecDraftId,
        attempt_id: SpecDraftAttemptId,
    },
}

impl RunOwner {
    pub fn work_item_id(&self) -> Option<WorkItemId> {
        match self {
            RunOwner::StageRun { work_item_id, .. } => Some(*work_item_id),
            RunOwner::DraftAttempt { .. } => None,
        }
    }

    pub fn stage_run_id(&self) -> Option<StageRunId> {
        match self {
            RunOwner::StageRun { stage_run_id, .. } => Some(*stage_run_id),
            RunOwner::DraftAttempt { .. } => None,
        }
    }

    pub fn spec_draft_id(&self) -> Option<SpecDraftId> {
        match self {
            RunOwner::StageRun { .. } => None,
            RunOwner::DraftAttempt { spec_draft_id, .. } => Some(*spec_draft_id),
        }
    }

    pub fn attempt_id(&self) -> Option<SpecDraftAttemptId> {
        match self {
            RunOwner::StageRun { .. } => None,
            RunOwner::DraftAttempt { attempt_id, .. } => Some(*attempt_id),
        }
    }

    pub fn run_id(&self) -> uuid::Uuid {
        match self {
            RunOwner::StageRun { stage_run_id, .. } => stage_run_id.0,
            RunOwner::DraftAttempt { attempt_id, .. } => attempt_id.0,
        }
    }
}

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
    pub config_sha256: String,
    pub config_error: Option<ConfigError>,
    pub workflow: Workflow,
    pub hook_skip: Vec<String>,
    pub repos: Vec<ProjectRepo>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectRepo {
    pub repo_id: RepoId,
    pub name: String,
    pub is_default: bool,
    pub base_branch: String,
    pub path_scope: Vec<String>,
    pub autonomy: Option<Autonomy>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigError {
    pub commit: String,
    pub code: String,
    pub params: Value,
    pub observed_at: Timestamp,
}

pub fn step_repo(repos: &[ProjectRepo], name: Option<&str>) -> Option<RepoId> {
    repos
        .iter()
        .find(|r| match name {
            None => r.is_default,
            Some(name) => r.name == name,
        })
        .map(|r| r.repo_id)
}

impl Project {
    pub fn default_repo(&self) -> Option<RepoId> {
        self.repos.iter().find(|r| r.is_default).map(|r| r.repo_id)
    }

    pub fn binding(&self, repo_id: RepoId) -> Option<&ProjectRepo> {
        self.repos.iter().find(|r| r.repo_id == repo_id)
    }

    pub fn effective_scope(&self, binding: &ProjectRepo) -> Vec<String> {
        let docs =
            (binding.repo_id == self.workspace_repo_id).then_some(self.project_path.as_str());
        crate::scope::PathScope::effective(&binding.path_scope, docs)
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
    pub kind: WorkKind,
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
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RevisionView {
    pub id: SpecRevisionId,
    pub lint_ok: bool,
    pub issues: Value,
    pub steps: Vec<StepPlan>,
    pub parser_version: u32,
    pub kind: WorkKind,
    pub priority: i64,
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Retries {
    pub auto: u32,
    pub manual: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextRef {
    pub path: String,
    pub origin: ContextOrigin,
    pub line: u32,
    pub bytes: u64,
    pub content_hash: String,
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
    pub kind: Option<WorkKind>,
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
    pub start_approved_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecLink {
    pub spec_id: SpecId,
    pub step_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExternalChange {
    pub title: String,
    pub author: String,
    pub from_fork: bool,
    pub draft: bool,
    pub spec: Option<SpecLink>,
    pub cause: ExternalCause,
    pub actor: Option<String>,
    pub created_at: Timestamp,
    #[serde(skip)]
    pub spec_slug: Option<String>,
    #[serde(skip)]
    pub workflow: Workflow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FixRound {
    pub number: u32,
    pub cause: Coded,
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
    pub docs_head_sha: Option<String>,
    pub queued_at: Option<Timestamp>,
    pub queue_native: bool,
    pub fix: Option<FixRound>,
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
    pub owner: RunOwner,
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
    pub owner: RunOwner,
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
    pub effect: Effect,
    pub tool: String,
    pub match_kind: MatchKind,
    pub command: Option<String>,
    pub path: Option<String>,
    pub note: Option<String>,
    pub permission_request_id: Option<PermissionRequestId>,
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
    pub role: ChangeRole,
    pub repo_id: RepoId,
    pub number: u64,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub head_sha: String,
    pub update_head_sha: Option<String>,
    pub state: ChangeState,
    pub merge_commit_sha: Option<String>,
    pub merged_at: Option<Timestamp>,
    pub skipped_at: Option<Timestamp>,
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
    pub toolchains: Vec<ToolchainUse>,
    pub path_scope: Vec<String>,
    pub docs: Option<DocsCheckout>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocsCheckout {
    pub repo_id: RepoId,
    pub full_name: String,
    pub commit: String,
    pub project_path: String,
    pub path: String,
    pub mode: DocsMode,
    pub branch: Option<String>,
    pub base_sha: String,
    pub pushed_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolchainUse {
    pub tool: Tool,
    pub version: String,
    pub bin_dir: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub downloaded: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    pub id: EnvironmentId,
    pub owner: RunOwner,
    pub repo_id: RepoId,
    pub root_path: String,
    pub worktree_path: String,
    pub branch: Option<String>,
    pub base_sha: Option<String>,
    pub state: EnvironmentState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub port_start: Option<u16>,
    pub port_count: Option<u16>,
    pub recipe: Option<Value>,
    pub recipe_sha256: Option<String>,
    pub toolchains: Vec<ToolchainUse>,
    pub path_scope: Vec<String>,
    pub docs: Option<DocsCheckout>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub owner: RunOwner,
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
    pub owner: RunOwner,
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
pub struct DecisionAnswer {
    pub option: Option<u32>,
    pub note: Option<String>,
    pub answered_by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRequest {
    pub id: DecisionRequestId,
    pub owner: RunOwner,
    pub session_id: SessionId,
    pub state: DecisionState,
    pub question: String,
    pub context: Option<String>,
    pub options: Vec<DecisionOption>,
    pub answer: Option<DecisionAnswer>,
    pub asked_at: Timestamp,
    pub answered_at: Option<Timestamp>,
    pub delivered_at: Option<Timestamp>,
    pub expires_at: Timestamp,
}

impl DecisionRequest {
    pub fn is_open(&self) -> bool {
        matches!(self.state, DecisionState::Asked | DecisionState::Answered)
    }

    pub fn recommended(&self) -> &str {
        self.options.first().map_or("", |o| o.label.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub item: WorkItem,
    pub stage_run: Option<StageRun>,
    pub gates: Vec<GateResult>,
    pub blocker: Option<Blocker>,
    pub change_request: Option<ChangeRequest>,
    pub docs_change: Option<ChangeRequest>,
    pub external: Option<ExternalChange>,
    pub environment: Option<Environment>,
    pub session: Option<Session>,
    pub permissions: Vec<PermissionRequest>,
    pub decisions: Vec<DecisionRequest>,
    pub attempts: BTreeMap<Stage, u32>,
    pub retries: BTreeMap<Stage, Retries>,
    pub retired_runs: Vec<StageRun>,
    pub retired_sessions: Vec<Session>,
    pub prior_usage: Usage,
    pub usage_records: Vec<UsageRecord>,
    pub quota_observations: Vec<AccountQuota>,
    pub added_rules: Vec<PermissionRule>,
    pub hook_gates: Vec<HookGate>,
    pub reviews: Vec<Review>,
    #[serde(skip)]
    pub workflow: Workflow,
    #[serde(skip)]
    pub policy: RepoPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewPosted {
    pub event: ReviewEvent,
    pub url: Option<String>,
    pub at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub id: ReviewId,
    pub work_item_id: WorkItemId,
    pub stage_run_id: StageRunId,
    pub session_id: SessionId,
    pub head_sha: String,
    pub outcome: ReviewOutcome,
    pub basis: ReviewBasis,
    pub summary: String,
    pub findings: Vec<Finding>,
    pub questions: Vec<String>,
    pub posted: Option<ReviewPosted>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookGate {
    pub run_id: crate::ids::HookRunId,
    pub hook: String,
    pub action: crate::vocab::HookActionKind,
    pub state: crate::vocab::HookRunState,
    pub error: Option<Coded>,
}

impl Task {
    pub fn change_of(&mut self, role: ChangeRole) -> Option<&mut ChangeRequest> {
        match role {
            ChangeRole::Code => self.change_request.as_mut(),
            ChangeRole::Docs => self.docs_change.as_mut(),
        }
    }

    pub fn new(item: WorkItem) -> Task {
        Task {
            item,
            stage_run: None,
            gates: Vec::new(),
            blocker: None,
            change_request: None,
            docs_change: None,
            external: None,
            environment: None,
            session: None,
            permissions: Vec::new(),
            decisions: Vec::new(),
            attempts: BTreeMap::new(),
            retries: BTreeMap::new(),
            retired_runs: Vec::new(),
            retired_sessions: Vec::new(),
            prior_usage: Usage::default(),
            usage_records: Vec::new(),
            quota_observations: Vec::new(),
            added_rules: Vec::new(),
            hook_gates: Vec::new(),
            reviews: Vec::new(),
            workflow: Workflow::default(),
            policy: RepoPolicy::default(),
        }
    }

    pub fn has_review_stage(&self) -> bool {
        self.workflow.has_stage(Stage::Review)
    }

    pub fn approved_at(&self, head_sha: &str) -> bool {
        self.reviews
            .iter()
            .any(|r| r.head_sha == head_sha && r.outcome == ReviewOutcome::Approved)
    }

    pub fn run_parts(&mut self) -> crate::session::RunParts<'_> {
        crate::session::RunParts {
            environment: &mut self.environment,
            session: &mut self.session,
            retired_sessions: &mut self.retired_sessions,
            permissions: &mut self.permissions,
            decisions: &mut self.decisions,
            prior_usage: &mut self.prior_usage,
            usage_records: &mut self.usage_records,
            quota_observations: &mut self.quota_observations,
        }
    }

    pub fn run_owner(&self) -> Option<RunOwner> {
        self.stage_run.as_ref().map(|r| RunOwner::StageRun {
            work_item_id: self.item.id,
            stage_run_id: r.id,
        })
    }

    pub fn open_blocker(&self) -> Option<&Blocker> {
        self.blocker.as_ref().filter(|b| b.resolution.is_none())
    }

    pub fn live_session(&self) -> Option<&Session> {
        self.session.as_ref().filter(|s| s.state.is_live())
    }

    pub fn gate_for(&self, source: GateSource, head_sha: &str) -> Option<&GateResult> {
        self.gates
            .iter()
            .find(|g| g.source == source && g.head_sha == head_sha)
    }

    pub fn ready_environment(&self) -> Option<&Environment> {
        self.environment
            .as_ref()
            .filter(|e| e.state == EnvironmentState::Ready)
    }

    pub fn unposted_review(&self) -> Option<&Review> {
        let head = self.stage_run.as_ref()?.head_sha.as_deref()?;
        self.reviews.iter().rev().find(|r| {
            r.head_sha == head && r.posted.is_none() && r.outcome != ReviewOutcome::SpecUnclear
        })
    }

    pub fn stage_usage(&self, now: Timestamp) -> Usage {
        let mut usage = self.prior_usage;
        if let (Some(s), Some(run)) = (&self.session, &self.stage_run)
            && s.owner.stage_run_id() == Some(run.id)
        {
            usage.add(s.usage_at(now));
        }
        usage
    }

    pub fn is_at_stage(&self, stage: Stage) -> bool {
        self.item.state == WorkItemState::InStage && self.item.stage == Some(stage)
    }

    pub fn is_at(&self, stage: Stage, phase: StagePhase) -> bool {
        self.item.state == WorkItemState::InStage
            && self.item.stage == Some(stage)
            && self.item.stage_phase == Some(phase)
    }

    pub fn is_writing_at(&self, phase: StagePhase) -> bool {
        self.is_at(Stage::Dev, phase) || self.is_at(Stage::Fix, phase)
    }

    pub fn is_writing(&self) -> bool {
        self.is_at_stage(Stage::Dev) || self.is_at_stage(Stage::Fix)
    }

    pub fn fix_rounds_used(&self) -> u32 {
        self.retired_runs
            .iter()
            .chain(self.stage_run.iter())
            .filter_map(|r| r.fix.as_ref().map(|f| f.number))
            .max()
            .unwrap_or(0)
    }

    pub fn rejected_at(&self, head_sha: &str) -> Option<&Review> {
        self.reviews
            .iter()
            .rev()
            .find(|r| r.head_sha == head_sha && r.outcome == ReviewOutcome::ChangesRequested)
    }

    pub fn fixable(&self) -> bool {
        match &self.external {
            Some(external) => !external.from_fork,
            None => self.item.origin == WorkItemOrigin::SpecStep,
        }
    }

    pub fn fix_rounds_left(&self) -> bool {
        self.fix_rounds_used() < self.workflow.fix_rounds
    }

    pub fn is_queued(&self) -> bool {
        self.stage_run
            .as_ref()
            .is_some_and(|r| r.queued_at.is_some())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftDocument {
    pub index: u32,
    pub path: Option<String>,
    pub origin: PacketOrigin,
    pub bytes: u64,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecDraft {
    pub id: SpecDraftId,
    pub project_id: ProjectId,
    pub path: String,
    pub slug: String,
    pub idea: String,
    pub locale: DraftLocale,
    pub mode: DraftMode,
    pub target_kind: TargetKind,
    pub context_paths: Vec<String>,
    pub state: SpecDraftState,
    pub attempt: u32,
    pub current_attempt_id: Option<SpecDraftAttemptId>,
    pub retries: u32,
    pub queued_at: Timestamp,
    pub awaiting_since: Option<Timestamp>,
    pub failure: Option<Coded>,
    pub spec_id: Option<SpecId>,
    pub commit_sha: Option<String>,
    pub version: u64,
    pub created_by: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub finished_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecDraftAttempt {
    pub id: SpecDraftAttemptId,
    pub spec_draft_id: SpecDraftId,
    pub attempt: u32,
    pub trigger: Trigger,
    pub reason: Option<String>,
    pub note: Option<String>,
    pub state: DraftAttemptState,
    pub base_sha: Option<String>,
    pub budget: Budget,
    pub budget_warnings: Vec<BudgetDimension>,
    pub resumed_lost: bool,
    pub rounds: u32,
    pub packet_sha256: Option<String>,
    pub failure: Option<Coded>,
    pub started_at: Timestamp,
    pub ended_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LintRound {
    pub spec_draft_id: SpecDraftId,
    pub attempt_id: SpecDraftAttemptId,
    pub round: u32,
    pub session_id: SessionId,
    pub turn: u32,
    pub outcome: LintOutcome,
    pub bytes: Option<u64>,
    pub content: Option<Vec<u8>>,
    pub content_hash: Option<String>,
    pub parser_version: u32,
    pub issues: Vec<Value>,
    pub decisions_open: u32,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundMark {
    pub attempt_id: SpecDraftAttemptId,
    pub round: u32,
    pub session_id: SessionId,
    pub turn: u32,
    pub outcome: LintOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryRequest {
    pub reason: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub draft: SpecDraft,
    pub attempt: Option<SpecDraftAttempt>,
    pub environment: Option<Environment>,
    pub retired_environments: Vec<Environment>,
    pub session: Option<Session>,
    pub retired_sessions: Vec<Session>,
    pub permissions: Vec<PermissionRequest>,
    pub decisions: Vec<DecisionRequest>,
    pub prior_usage: Usage,
    pub usage_records: Vec<UsageRecord>,
    pub quota_observations: Vec<AccountQuota>,
    pub last_round: Option<RoundMark>,
    pub has_content: bool,
    pub retry_request: Option<RetryRequest>,
}

impl Draft {
    pub fn new(draft: SpecDraft) -> Draft {
        Draft {
            draft,
            attempt: None,
            environment: None,
            retired_environments: Vec::new(),
            session: None,
            retired_sessions: Vec::new(),
            permissions: Vec::new(),
            decisions: Vec::new(),
            prior_usage: Usage::default(),
            usage_records: Vec::new(),
            quota_observations: Vec::new(),
            last_round: None,
            has_content: false,
            retry_request: None,
        }
    }

    pub fn id(&self) -> SpecDraftId {
        self.draft.id
    }

    pub fn run_parts(&mut self) -> crate::session::RunParts<'_> {
        crate::session::RunParts {
            environment: &mut self.environment,
            session: &mut self.session,
            retired_sessions: &mut self.retired_sessions,
            permissions: &mut self.permissions,
            decisions: &mut self.decisions,
            prior_usage: &mut self.prior_usage,
            usage_records: &mut self.usage_records,
            quota_observations: &mut self.quota_observations,
        }
    }

    pub fn running_attempt(&self) -> Option<&SpecDraftAttempt> {
        self.attempt
            .as_ref()
            .filter(|a| a.state == DraftAttemptState::Running)
    }

    pub fn run_owner(&self) -> Option<RunOwner> {
        self.attempt.as_ref().map(|a| RunOwner::DraftAttempt {
            spec_draft_id: self.draft.id,
            attempt_id: a.id,
        })
    }

    pub fn live_session(&self) -> Option<&Session> {
        self.session.as_ref().filter(|s| s.state.is_live())
    }

    pub fn ready_environment(&self) -> Option<&Environment> {
        self.environment
            .as_ref()
            .filter(|e| e.state == EnvironmentState::Ready)
    }

    pub fn environment_by_id(&self, id: EnvironmentId) -> Option<&Environment> {
        self.environment
            .iter()
            .chain(self.retired_environments.iter())
            .find(|e| e.id == id)
    }

    pub fn live_environment(&self) -> Option<&Environment> {
        self.environment.as_ref().filter(|e| {
            matches!(
                e.state,
                EnvironmentState::Ready | EnvironmentState::Preparing
            )
        })
    }
}
