use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::authorship::SessionAuthor;
use crate::gate::Check;
use crate::ids::{
    BlockerId, ChangeRequestId, EnvironmentId, GateResultId, PermissionRequestId, PermissionRuleId,
    ProjectId, RepoId, SessionId, SpecDraftAttemptId, SpecDraftId, SpecId, SpecRevisionId,
    StageRunId, UsageRecordId, UserLinkAuthorizationId, UserLinkId, WorkItemId,
};
use crate::job::JobKind;
use crate::model::{
    Budget, Coded, ContextRef, DependsOn, DocsCheckout, DraftDocument, ProjectRepo, RunOwner,
    ToolchainUse,
};
use crate::time::Timestamp;
use crate::vocab::{
    AuthorizationState, AuthorshipMode, BlockerCode, BlockerResolution, BudgetDimension,
    ChangeCredentialKind, ChangeRole, DraftFailureCode, DraftLocale, GateOutcome, GateSource,
    InterruptBy, LintOutcome, MergeMethod, PermissionState, RelinkCause, ReloadTrigger,
    ResumeCause, ResumeReason, SessionState, SpecOrigin, Stage, StagePhase, SupersedeReason,
    TokenInvalidCause, Trigger,
};

fn empty_context_hash() -> String {
    EMPTY_CONTEXT_HASH.to_string()
}

pub const EMPTY_CONTEXT_HASH: &str =
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn scm_source() -> GateSource {
    GateSource::Scm
}

fn first_parser_version() -> u32 {
    1
}

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
        config_sha256: String,
        repos: Vec<ProjectRepo>,
    },
    #[serde(rename = "workspace.project.config_changed")]
    ProjectConfigChanged {
        project_id: ProjectId,
        name: String,
        config_commit: String,
        config_sha256: String,
        repos: Vec<ProjectRepo>,
        trigger: ReloadTrigger,
    },
    #[serde(rename = "workspace.project.config_rejected")]
    ProjectConfigRejected {
        project_id: ProjectId,
        commit: String,
        code: String,
        params: Value,
        trigger: ReloadTrigger,
    },
    #[serde(rename = "workspace.project.config_error_cleared")]
    ProjectConfigErrorCleared {
        project_id: ProjectId,
        commit: String,
    },
    #[serde(rename = "workspace.project.authorship_set")]
    ProjectAuthorshipSet {
        project_id: ProjectId,
        mode: AuthorshipMode,
    },
    #[serde(rename = "workspace.project.user_token_set")]
    ProjectUserTokenSet {
        project_id: ProjectId,
        integration_id: String,
        provider_user_id: u64,
        login: String,
        expires_at: Option<Timestamp>,
    },
    #[serde(rename = "workspace.project.user_token_removed")]
    ProjectUserTokenRemoved { project_id: ProjectId },
    #[serde(rename = "workspace.project.user_token_invalidated")]
    ProjectUserTokenInvalidated {
        project_id: ProjectId,
        cause: TokenInvalidCause,
    },
    #[serde(rename = "workspace.secret.set")]
    SecretSet { project_id: ProjectId, name: String },
    #[serde(rename = "workspace.secret.removed")]
    SecretRemoved { project_id: ProjectId, name: String },
    #[serde(rename = "workspace.doc.committed")]
    DocCommitted {
        project_id: ProjectId,
        path: String,
        commit: String,
        parent: String,
        blob: String,
        bytes: u64,
        created: bool,
    },
    #[serde(rename = "spec.spec.registered")]
    SpecRegistered {
        spec_id: SpecId,
        project_id: ProjectId,
        path: String,
        slug: String,
        origin: SpecOrigin,
    },
    #[serde(rename = "spec.revision.recorded")]
    SpecRevisionRecorded {
        spec_id: SpecId,
        revision_id: SpecRevisionId,
        source_commit: String,
        content_hash: String,
        #[serde(default = "empty_context_hash")]
        context_hash: String,
        #[serde(default)]
        context: Vec<ContextRef>,
        lint_ok: bool,
        issue_codes: Vec<String>,
        title: String,
        reused: bool,
        #[serde(default = "first_parser_version")]
        parser_version: u32,
        #[serde(default)]
        priority: i64,
    },
    #[serde(rename = "spec.spec.approved")]
    SpecApproved {
        spec_id: SpecId,
        revision_id: SpecRevisionId,
        reason: Option<String>,
        #[serde(default)]
        priority: i64,
        #[serde(default)]
        depends_on: Vec<DependsOn>,
        #[serde(default)]
        previous_revision_id: Option<SpecRevisionId>,
    },
    #[serde(rename = "spec.spec.withdrawn")]
    SpecWithdrawn { spec_id: SpecId, reason: String },
    #[serde(rename = "spec.spec.shipped")]
    SpecShipped {
        spec_id: SpecId,
        revision_id: SpecRevisionId,
    },

    #[serde(rename = "spec.draft.requested")]
    SpecDraftRequested {
        spec_draft_id: SpecDraftId,
        project_id: ProjectId,
        path: String,
        slug: String,
        idea: String,
        locale: DraftLocale,
        context_paths: Vec<String>,
    },
    #[serde(rename = "spec.draft.attempt_started")]
    SpecDraftAttemptStarted {
        spec_draft_id: SpecDraftId,
        attempt_id: SpecDraftAttemptId,
        attempt: u32,
        trigger: Trigger,
        reason: Option<String>,
        note: Option<String>,
        budget: Budget,
    },
    #[serde(rename = "spec.draft.packet_rendered")]
    SpecDraftPacketRendered {
        spec_draft_id: SpecDraftId,
        attempt_id: SpecDraftAttemptId,
        base_sha: String,
        packet_sha256: String,
        documents: Vec<DraftDocument>,
    },
    #[serde(rename = "spec.draft.linted")]
    SpecDraftLinted {
        spec_draft_id: SpecDraftId,
        attempt_id: SpecDraftAttemptId,
        round: u32,
        session_id: SessionId,
        turn: u32,
        outcome: LintOutcome,
        bytes: Option<u64>,
        content_hash: Option<String>,
        issue_codes: Vec<String>,
        decisions_open: u32,
    },
    #[serde(rename = "spec.draft.committing")]
    SpecDraftCommitting {
        spec_draft_id: SpecDraftId,
        attempt_id: SpecDraftAttemptId,
        round: u32,
    },
    #[serde(rename = "spec.draft.succeeded")]
    SpecDraftSucceeded {
        spec_draft_id: SpecDraftId,
        attempt_id: SpecDraftAttemptId,
        spec_id: SpecId,
        revision_id: SpecRevisionId,
        commit: String,
    },
    #[serde(rename = "spec.draft.failed")]
    SpecDraftFailed {
        spec_draft_id: SpecDraftId,
        attempt_id: SpecDraftAttemptId,
        code: DraftFailureCode,
        params: Value,
        commit: Option<String>,
    },
    #[serde(rename = "spec.draft.retried")]
    SpecDraftRetried {
        spec_draft_id: SpecDraftId,
        reason: String,
        note: Option<String>,
    },
    #[serde(rename = "spec.draft.cancelled")]
    SpecDraftCancelled {
        spec_draft_id: SpecDraftId,
        reason: String,
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
    #[serde(rename = "delivery.work_item.repinned")]
    WorkItemRepinned {
        work_item_id: WorkItemId,
        spec_revision_id: SpecRevisionId,
        step_index: u32,
        work: String,
        repo_id: RepoId,
        priority: i64,
        restored: bool,
    },
    #[serde(rename = "delivery.work_item.superseded")]
    WorkItemSuperseded {
        work_item_id: WorkItemId,
        spec_revision_id: SpecRevisionId,
        reason: SupersedeReason,
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
        retry_of: Option<Coded>,
        budget: Option<Budget>,
    },
    #[serde(rename = "delivery.stage_run.phase_changed")]
    StagePhaseChanged {
        work_item_id: WorkItemId,
        stage_run_id: StageRunId,
        phase: StagePhase,
        head_sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        docs_head_sha: Option<String>,
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
        #[serde(default = "scm_source")]
        source: GateSource,
        head_sha: String,
        outcome: GateOutcome,
        checks: Vec<Check>,
    },
    #[serde(rename = "delivery.gate.started")]
    GateStarted {
        work_item_id: WorkItemId,
        stage_run_id: StageRunId,
        gate_result_id: GateResultId,
        source: GateSource,
        head_sha: String,
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
        owner: RunOwner,
        environment_id: EnvironmentId,
        repo_id: RepoId,
        root_path: String,
        worktree_path: String,
        branch: Option<String>,
        base_sha: String,
        port_start: Option<u16>,
        port_count: Option<u16>,
        recipe: Option<Value>,
        recipe_sha256: Option<String>,
        setup: Vec<Value>,
        #[serde(default)]
        toolchains: Vec<ToolchainUse>,
        path_scope: Vec<String>,
        docs: Option<Box<DocsCheckout>>,
    },
    #[serde(rename = "runtime.environment.torn_down")]
    EnvironmentTornDown {
        owner: RunOwner,
        environment_id: EnvironmentId,
        teardown_failures: Vec<Value>,
    },
    #[serde(rename = "runtime.environment.teardown_failed")]
    EnvironmentTeardownFailed {
        owner: RunOwner,
        environment_id: EnvironmentId,
        error: Coded,
    },
    #[serde(rename = "runtime.session.started")]
    SessionStarted {
        owner: RunOwner,
        session_id: SessionId,
        environment_id: EnvironmentId,
        cli: String,
        cli_version: Option<String>,
        model: Option<String>,
        account: String,
        runner_unit: String,
        socket_path: String,
        log_path: String,
        resumed_from_session_id: Option<SessionId>,
        resume_cause: Option<ResumeCause>,
        author: SessionAuthor,
    },
    #[serde(rename = "runtime.session.reattached")]
    SessionReattached {
        owner: RunOwner,
        session_id: SessionId,
        runner_version: String,
        from_offset: u64,
    },
    #[serde(rename = "runtime.session.steered")]
    SessionSteered {
        owner: RunOwner,
        session_id: SessionId,
        text: String,
        turn: u32,
        n: u32,
    },
    #[serde(rename = "runtime.session.interrupt_requested")]
    SessionInterruptRequested {
        owner: RunOwner,
        session_id: SessionId,
        actor: InterruptBy,
        turn: u32,
    },
    #[serde(rename = "runtime.session.stop_requested")]
    SessionStopRequested {
        owner: RunOwner,
        session_id: SessionId,
        reason: String,
    },
    #[serde(rename = "runtime.session.initialized")]
    SessionInitialized {
        owner: RunOwner,
        session_id: SessionId,
        cli_session_id: String,
        model: Option<String>,
    },
    #[serde(rename = "runtime.session.state_changed")]
    SessionStateChanged {
        owner: RunOwner,
        session_id: SessionId,
        state: SessionState,
        running_ms: i64,
    },
    #[serde(rename = "runtime.session.turn_requested")]
    SessionTurnRequested {
        owner: RunOwner,
        session_id: SessionId,
        turn: u32,
        prompt: Prompt,
    },
    #[serde(rename = "runtime.session.turn_ended")]
    SessionTurnEnded {
        owner: RunOwner,
        session_id: SessionId,
        turn: u32,
        status: String,
        cost_usd_micros: Option<i64>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        running_ms: i64,
        account: String,
        usage_record_id: UsageRecordId,
    },
    #[serde(rename = "runtime.session.ended")]
    SessionEnded {
        owner: RunOwner,
        session_id: SessionId,
        end_reason: Coded,
    },
    #[serde(rename = "runtime.session.lost")]
    SessionLost {
        owner: RunOwner,
        session_id: SessionId,
        cause: String,
    },
    #[serde(rename = "runtime.permission.requested")]
    PermissionRequested {
        owner: RunOwner,
        session_id: SessionId,
        permission_request_id: PermissionRequestId,
        cli_request_id: String,
        tool: String,
        summary: String,
        input: Value,
    },
    #[serde(rename = "runtime.permission.answered")]
    PermissionAnswered {
        owner: RunOwner,
        permission_request_id: PermissionRequestId,
        state: PermissionState,
        answer_source: String,
        message: Option<String>,
    },
    #[serde(rename = "runtime.permission.delivered")]
    PermissionDelivered {
        owner: RunOwner,
        permission_request_id: PermissionRequestId,
    },
    #[serde(rename = "runtime.permission_rule.added")]
    PermissionRuleAdded {
        rule_id: PermissionRuleId,
        project_id: ProjectId,
        owner: RunOwner,
        tool: String,
        command: Option<String>,
        permission_request_id: PermissionRequestId,
        created_by: String,
    },
    #[serde(rename = "runtime.permission_rule.revoked")]
    PermissionRuleRevoked {
        rule_id: PermissionRuleId,
        project_id: ProjectId,
        reason: String,
    },
    #[serde(rename = "runtime.budget.warned")]
    BudgetWarned {
        owner: RunOwner,
        dimension: BudgetDimension,
        used: Value,
        limit: Value,
    },
    #[serde(rename = "runtime.quota.observed")]
    QuotaObserved {
        owner: RunOwner,
        session_id: SessionId,
        account: String,
        window: String,
        utilization: f64,
        status: String,
        resets_at: Option<Timestamp>,
    },

    #[serde(rename = "scm.branch.pushed")]
    BranchPushed {
        work_item_id: WorkItemId,
        target: ChangeRole,
        branch: String,
        sha: String,
    },
    #[serde(rename = "scm.change_request.opened")]
    ChangeRequestOpened {
        work_item_id: WorkItemId,
        change_request_id: ChangeRequestId,
        role: ChangeRole,
        repo_id: RepoId,
        number: u64,
        url: String,
        head_branch: String,
        base_branch: String,
        head_sha: String,
        opened_with: ChangeCredentialKind,
    },
    #[serde(rename = "scm.change_request.updated")]
    ChangeRequestUpdated {
        work_item_id: WorkItemId,
        change_request_id: ChangeRequestId,
        role: ChangeRole,
        head_sha: String,
    },
    #[serde(rename = "scm.change_request.merged")]
    ChangeRequestMerged {
        work_item_id: WorkItemId,
        change_request_id: ChangeRequestId,
        role: ChangeRole,
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
        merged_by_saka: bool,
    },
    #[serde(rename = "scm.change_request.closed")]
    ChangeRequestClosed {
        work_item_id: WorkItemId,
        change_request_id: ChangeRequestId,
        role: ChangeRole,
    },
    #[serde(rename = "delivery.docs_change.skipped")]
    DocsChangeSkipped {
        work_item_id: WorkItemId,
        change_request_id: ChangeRequestId,
        number: u64,
        reason: String,
    },
    #[serde(rename = "engine.job.exhausted")]
    JobExhausted {
        job_id: uuid::Uuid,
        kind: JobKind,
        last_error: Coded,
    },
    #[serde(rename = "scm.user_link.authorization_started")]
    UserLinkAuthorizationStarted {
        authorization_id: UserLinkAuthorizationId,
        integration_id: String,
        expires_at: Timestamp,
    },
    #[serde(rename = "scm.user_link.authorization_slowed")]
    UserLinkAuthorizationSlowed {
        authorization_id: UserLinkAuthorizationId,
        interval_secs: u32,
    },
    #[serde(rename = "scm.user_link.authorization_ended")]
    UserLinkAuthorizationEnded {
        authorization_id: UserLinkAuthorizationId,
        state: AuthorizationState,
        failure: Option<Coded>,
    },
    #[serde(rename = "scm.user_link.linked")]
    UserLinked {
        link_id: UserLinkId,
        integration_id: String,
        authorization_id: UserLinkAuthorizationId,
        provider_user_id: u64,
        login: String,
        replaced_login: Option<String>,
    },
    #[serde(rename = "scm.user_link.refreshed")]
    UserLinkRefreshed {
        link_id: UserLinkId,
        access_expires_at: Timestamp,
        refresh_expires_at: Timestamp,
    },
    #[serde(rename = "scm.user_link.relink_required")]
    UserLinkRelinkRequired {
        link_id: UserLinkId,
        cause: RelinkCause,
    },
    #[serde(rename = "scm.user_link.removed")]
    UserLinkRemoved { link_id: UserLinkId },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Prompt {
    WorkPacket {
        retry_of: Option<Coded>,
    },
    Uncommitted {
        dirty: bool,
        commits_ahead: u32,
    },
    OutOfScope {
        paths: Vec<String>,
        count: u32,
        scope: Vec<String>,
    },
    DocsUncommitted {
        path: String,
    },
    DocsOutOfScope {
        path: String,
        paths: Vec<String>,
        count: u32,
        scope: Vec<String>,
    },
    Resume {
        cause: ResumeReason,
        retry_of: Option<Coded>,
    },
    Steer {
        text: String,
    },
    DraftPacket,
    DraftLint {
        round: u32,
        max_rounds: u32,
    },
    DraftMissing {
        round: u32,
        max_rounds: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Repo(RepoId),
    Project(ProjectId),
    Spec(SpecId),
    WorkItem(WorkItemId),
    SpecDraft(SpecDraftId),
    Session(SessionId),
    PermissionRequest(PermissionRequestId),
    Job(uuid::Uuid),
    UserLinkAuthorization(UserLinkAuthorizationId),
    UserLink(UserLinkId),
}

impl Stream {
    pub fn kind(self) -> &'static str {
        match self {
            Stream::Repo(_) => "repo",
            Stream::Project(_) => "project",
            Stream::Spec(_) => "spec",
            Stream::WorkItem(_) => "work_item",
            Stream::SpecDraft(_) => "spec_draft",
            Stream::Session(_) => "session",
            Stream::PermissionRequest(_) => "permission_request",
            Stream::Job(_) => "job",
            Stream::UserLinkAuthorization(_) => "user_link_authorization",
            Stream::UserLink(_) => "user_link",
        }
    }

    pub fn id(self) -> uuid::Uuid {
        match self {
            Stream::Repo(id) => id.0,
            Stream::Project(id) => id.0,
            Stream::Spec(id) => id.0,
            Stream::WorkItem(id) => id.0,
            Stream::SpecDraft(id) => id.0,
            Stream::Session(id) => id.0,
            Stream::PermissionRequest(id) => id.0,
            Stream::Job(id) => id,
            Stream::UserLinkAuthorization(id) => id.0,
            Stream::UserLink(id) => id.0,
        }
    }
}

impl Stream {
    pub fn of_owner(owner: &RunOwner) -> Stream {
        match owner {
            RunOwner::StageRun { work_item_id, .. } => Stream::WorkItem(*work_item_id),
            RunOwner::DraftAttempt { spec_draft_id, .. } => Stream::SpecDraft(*spec_draft_id),
        }
    }
}

impl Event {
    pub const VERSION: u32 = 1;

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
            E::ProjectRegistered { project_id, .. }
            | E::ProjectConfigChanged { project_id, .. }
            | E::ProjectConfigRejected { project_id, .. }
            | E::ProjectConfigErrorCleared { project_id, .. }
            | E::ProjectAuthorshipSet { project_id, .. }
            | E::ProjectUserTokenSet { project_id, .. }
            | E::ProjectUserTokenRemoved { project_id }
            | E::ProjectUserTokenInvalidated { project_id, .. }
            | E::SecretSet { project_id, .. }
            | E::SecretRemoved { project_id, .. }
            | E::DocCommitted { project_id, .. } => Stream::Project(*project_id),
            E::SpecRegistered { spec_id, .. }
            | E::SpecRevisionRecorded { spec_id, .. }
            | E::SpecApproved { spec_id, .. }
            | E::SpecWithdrawn { spec_id, .. }
            | E::SpecShipped { spec_id, .. } => Stream::Spec(*spec_id),
            E::SpecDraftRequested { spec_draft_id, .. }
            | E::SpecDraftAttemptStarted { spec_draft_id, .. }
            | E::SpecDraftPacketRendered { spec_draft_id, .. }
            | E::SpecDraftLinted { spec_draft_id, .. }
            | E::SpecDraftCommitting { spec_draft_id, .. }
            | E::SpecDraftSucceeded { spec_draft_id, .. }
            | E::SpecDraftFailed { spec_draft_id, .. }
            | E::SpecDraftRetried { spec_draft_id, .. }
            | E::SpecDraftCancelled { spec_draft_id, .. } => Stream::SpecDraft(*spec_draft_id),
            E::SessionStarted { session_id, .. }
            | E::SessionReattached { session_id, .. }
            | E::SessionSteered { session_id, .. }
            | E::SessionInterruptRequested { session_id, .. }
            | E::SessionStopRequested { session_id, .. }
            | E::SessionInitialized { session_id, .. }
            | E::SessionStateChanged { session_id, .. }
            | E::SessionTurnRequested { session_id, .. }
            | E::SessionTurnEnded { session_id, .. }
            | E::SessionEnded { session_id, .. }
            | E::SessionLost { session_id, .. }
            | E::QuotaObserved { session_id, .. } => Stream::Session(*session_id),
            E::PermissionRuleAdded { project_id, .. }
            | E::PermissionRuleRevoked { project_id, .. } => Stream::Project(*project_id),
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
            | E::WorkItemRepinned { work_item_id, .. }
            | E::WorkItemSuperseded { work_item_id, .. }
            | E::WorkItemAdmitted { work_item_id }
            | E::StageRunCreated { work_item_id, .. }
            | E::StagePhaseChanged { work_item_id, .. }
            | E::StageRunSucceeded { work_item_id, .. }
            | E::GateObserved { work_item_id, .. }
            | E::GateStarted { work_item_id, .. }
            | E::MergeRequested { work_item_id, .. }
            | E::WorkItemBlocked { work_item_id, .. }
            | E::BlockerResolved { work_item_id, .. }
            | E::WorkItemCancelled { work_item_id, .. }
            | E::WorkItemDone { work_item_id, .. }
            | E::BranchPushed { work_item_id, .. }
            | E::ChangeRequestOpened { work_item_id, .. }
            | E::ChangeRequestUpdated { work_item_id, .. }
            | E::ChangeRequestMerged { work_item_id, .. }
            | E::ChangeRequestClosed { work_item_id, .. }
            | E::DocsChangeSkipped { work_item_id, .. } => Stream::WorkItem(*work_item_id),
            E::EnvironmentPrepared { owner, .. }
            | E::EnvironmentTornDown { owner, .. }
            | E::EnvironmentTeardownFailed { owner, .. }
            | E::BudgetWarned { owner, .. } => Stream::of_owner(owner),
            E::JobExhausted { job_id, .. } => Stream::Job(*job_id),
            E::UserLinkAuthorizationStarted {
                authorization_id, ..
            }
            | E::UserLinkAuthorizationSlowed {
                authorization_id, ..
            }
            | E::UserLinkAuthorizationEnded {
                authorization_id, ..
            } => Stream::UserLinkAuthorization(*authorization_id),
            E::UserLinked { link_id, .. }
            | E::UserLinkRefreshed { link_id, .. }
            | E::UserLinkRelinkRequired { link_id, .. }
            | E::UserLinkRemoved { link_id } => Stream::UserLink(*link_id),
        }
    }

    pub fn owner(&self) -> Option<&RunOwner> {
        use Event as E;
        match self {
            E::EnvironmentPrepared { owner, .. }
            | E::EnvironmentTornDown { owner, .. }
            | E::EnvironmentTeardownFailed { owner, .. }
            | E::SessionStarted { owner, .. }
            | E::SessionReattached { owner, .. }
            | E::SessionSteered { owner, .. }
            | E::SessionInterruptRequested { owner, .. }
            | E::SessionStopRequested { owner, .. }
            | E::SessionInitialized { owner, .. }
            | E::SessionStateChanged { owner, .. }
            | E::SessionTurnRequested { owner, .. }
            | E::SessionTurnEnded { owner, .. }
            | E::SessionEnded { owner, .. }
            | E::SessionLost { owner, .. }
            | E::PermissionRequested { owner, .. }
            | E::PermissionAnswered { owner, .. }
            | E::PermissionDelivered { owner, .. }
            | E::PermissionRuleAdded { owner, .. }
            | E::BudgetWarned { owner, .. }
            | E::QuotaObserved { owner, .. } => Some(owner),
            _ => None,
        }
    }

    pub fn work_item_id(&self) -> Option<WorkItemId> {
        if let Some(owner) = self.owner() {
            return owner.work_item_id();
        }
        match self.stream() {
            Stream::WorkItem(id) => Some(id),
            _ => None,
        }
    }

    pub fn spec_draft_id(&self) -> Option<SpecDraftId> {
        if let Some(owner) = self.owner() {
            return owner.spec_draft_id();
        }
        match self.stream() {
            Stream::SpecDraft(id) => Some(id),
            _ => None,
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
