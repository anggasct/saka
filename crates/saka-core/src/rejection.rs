use serde_json::{Value, json};

use crate::ids::{SpecDraftId, SpecId};
use crate::vocab::{TargetTaken, TokenInvalidCause};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Rejection {
    #[error("invalid state: {state}")]
    InvalidState { state: String },
    #[error("not supported in this version")]
    Unsupported,
    #[error("revision is stale")]
    RevisionStale,
    #[error("version is stale")]
    VersionStale,
    #[error("spec is not ready")]
    SpecNotReady { issues: Value },
    #[error("spec is already approved")]
    SpecAlreadyApproved,
    #[error("spec was read by an older parser")]
    SpecReparseRequired {
        parser_version: u32,
        current_parser_version: u32,
    },
    #[error("revision changes steps that already started")]
    RevisionConflict { steps: Value },
    #[error("the revision changes the kind while a task of the spec has started")]
    RevisionKindChanged,
    #[error("spec has active tasks")]
    SpecHasActiveTasks { tasks: Value },
    #[error("merge is not ready")]
    MergeNotReady {
        reason: &'static str,
        gate_outcome: Option<String>,
        head_sha: Option<String>,
        review_head_sha: Option<String>,
        review_outcome: Option<&'static str>,
    },
    #[error("permission request is already answered")]
    PermissionAlreadyAnswered,
    #[error("decision request not found")]
    DecisionNotFound,
    #[error("decision request is already answered")]
    DecisionAlreadyAnswered,
    #[error("decision answer is not valid")]
    DecisionAnswerInvalid,
    #[error("the session already has a question waiting")]
    DecisionOnePerSession,
    #[error("the run asked its last question")]
    DecisionLimitReached,
    #[error("the regression check is not available: {reason}")]
    RegressionNotAvailable { reason: &'static str },
    #[error("the run used its last regression check")]
    RegressionLimitReached,
    #[error("the draft is not waiting for the operator")]
    DraftNotWaiting,
    #[error("the draft does not pass lint")]
    DraftLintFailed,
    #[error("the architect has a question waiting")]
    DraftDecisionOpen,
    #[error("the draft has no changes to commit")]
    DraftNoChanges,
    #[error("a file of the draft changed on the base branch")]
    DraftBaseMoved { paths: Vec<String> },
    #[error("file not found")]
    FileNotFound { path: String },
    #[error("not found")]
    NotFound,
    #[error("project is already registered")]
    ProjectExists { project_id: String },
    #[error("spec slug is already used")]
    SpecSlugConflict { slug: String, path: String },
    #[error("the rendered spec does not hold exactly the input")]
    QuickSpecRenderMismatch,
    #[error("the spec was committed and registered but not approved")]
    QuickSpecNotApproved { code: String, spec: Value },
    #[error("task branch is used by another project")]
    TaskBranchConflict { branches: Vec<String> },
    #[error("no SCM integration serves this host")]
    ScmIntegrationUnknown { host: String, repo: Option<String> },
    #[error("the SCM app cannot access this repo")]
    ScmRepoInaccessible { repo: Option<String> },
    #[error("project config is not valid")]
    ProjectConfigInvalid { issues: Value },
    #[error("a removed repo still has live tasks")]
    ProjectRepoInUse { repos: Value },
    #[error("spec file not found on the base branch")]
    SpecFileNotFound { path: String, base_branch: String },
    #[error("retry limit reached")]
    RetryLimitReached { stage: Option<String>, limit: u32 },
    #[error("draft target is taken")]
    SpecDraftTargetTaken { path: String, taken: TargetTakenBy },
    #[error("work packet is not available")]
    WorkPacketUnavailable { reason: &'static str },
    #[error("secret value is too short")]
    SecretTooShort,
    #[error("secret name is not valid")]
    SecretNameInvalid,
    #[error("file changed on the base branch")]
    FileStale {
        blob: Option<String>,
        commit: String,
    },
    #[error("file path is blocked by another entry")]
    FilePathConflict { path: String },
    #[error("remote refused the push")]
    FilePushRejected { branch: String },
    #[error("file cannot be edited")]
    FileUnsupported { reason: FileUnsupported },
    #[error("the integration has no linked account")]
    UserLinkNotFound,
    #[error("inbox item does not exist")]
    InboxItemNotFound,
    #[error("inbox item cannot be dismissed")]
    InboxItemNotDismissible { subject: &'static str },
    #[error("inbox item is closed")]
    InboxItemClosed { state: &'static str },
    #[error("the integration has no OAuth client id for the device flow")]
    DeviceFlowUnconfigured { integration_id: String },
    #[error("the integration cannot author changes as a user")]
    UserAuthorshipUnsupported { integration_id: String },
    #[error("projects in user mode still use the linked account")]
    UserLinkInUse { projects: Value },
    #[error("an integration of the project has no usable linked account")]
    UserLinkMissing { integrations: Value },
    #[error("the project has no user token")]
    UserTokenMissing,
    #[error("the project's user token cannot be used")]
    UserTokenInvalid { cause: TokenInvalidCause },
    #[error("the project authors changes with its user token")]
    UserTokenInUse,
    #[error("the project's repos are on more than one integration")]
    UserTokenMultiIntegration { integrations: Vec<String> },
    #[error("the user token is not a fine-grained token")]
    UserTokenNotFineGrained,
    #[error("the provider refused the user token")]
    UserTokenRejected { status: u16 },
    #[error("the user token cannot read every repo of the project")]
    UserTokenRepoInaccessible { repos: Value },
    #[error("a review was already requested at this head")]
    ReviewAlreadyRequested { head_sha: String },
    #[error("the change cannot be fixed now")]
    FixNotAvailable { reason: &'static str },
    #[error("the change request does not exist")]
    ChangeRequestNotFound { repo: String, number: u64 },
    #[error("the change request belongs to a task made from a spec step")]
    ChangeRequestOwned { work_item_id: String },
    #[error("the change request already has a task")]
    ChangeRequestTracked { work_item_id: String },
    #[error("the change request is no longer open")]
    ChangeRequestNotOpen { number: u64, state: &'static str },
    #[error("the project workflow has no {entry} entry point")]
    EntryPointDisabled { entry: &'static str },
    #[error("the project workflow has no {stage} stage")]
    StageNotInWorkflow { stage: &'static str },
    #[error("a task cannot be taken over at this point of its stage")]
    StageNotTakeable {
        stage: &'static str,
        phase: Option<&'static str>,
    },
    #[error("the task cannot be returned to the loop at {stage}")]
    ReturnTargetInvalid {
        stage: &'static str,
        reason: &'static str,
    },
    #[error("the task cannot be reverted: {reason}")]
    RevertNotAllowed { reason: &'static str },
    #[error("the task already has a revert in progress")]
    RevertInProgress { work_item_id: String },
    #[error("the task was already reverted")]
    AlreadyReverted { work_item_id: String },
    #[error("blocker is not open")]
    BlockerNotOpen { blocker_id: String },
    #[error("the primary action is not the one that was shown")]
    BlockerActionChanged {
        blocker_id: String,
        expected: &'static str,
        actual: &'static str,
    },
    #[error("the primary action needs input")]
    ActionNeedsInput { action: &'static str },
    #[error("hook run is still pending")]
    HookRunPending,
    #[error("hook is not configured")]
    HookNotConfigured { hook: String },
    #[error("hook run holds a task")]
    HookRunGated { work_item_id: String },
    #[error("an identical rule is already active")]
    RuleDuplicate { rule_id: String },
    #[error("an allow rule cannot match every command")]
    RuleTooBroad,
    #[error("the rule is not valid: {field} {reason}")]
    RuleInvalid {
        field: &'static str,
        reason: &'static str,
    },
    #[error("the pairing code is not valid")]
    PairingCodeInvalid,
    #[error("the last passkey cannot be revoked")]
    LastCredential,
    #[error("the device cannot be revoked")]
    DeviceNotRevocable,
    #[error("the issue does not exist")]
    IssueNotFound,
    #[error("the issue already has a live intake")]
    IntakeActive {
        intake_id: crate::ids::IssueIntakeId,
    },
    #[error("the intake is already approved")]
    IntakeAlreadyApproved,
    #[error("the intake is already finished")]
    IntakeClosed { state: &'static str },
    #[error("the intake has not failed")]
    IntakeNotFailed,
    #[error("the intake has no retries left")]
    IntakeRetriesExhausted,
    #[error("invalid {field}: {code}")]
    Invalid {
        field: &'static str,
        code: &'static str,
    },
}

impl Rejection {
    pub fn code(&self) -> &'static str {
        match self {
            Rejection::InvalidState { .. } => "invalid_state",
            Rejection::Unsupported => "unsupported_in_this_version",
            Rejection::RevisionStale => "revision_stale",
            Rejection::VersionStale => "version_stale",
            Rejection::SpecNotReady { .. } => "spec_not_ready",
            Rejection::SpecAlreadyApproved => "spec_already_approved",
            Rejection::SpecReparseRequired { .. } => "spec_reparse_required",
            Rejection::RevisionConflict { .. } | Rejection::RevisionKindChanged => {
                "revision_conflict"
            }
            Rejection::SpecHasActiveTasks { .. } => "spec_has_active_tasks",
            Rejection::MergeNotReady { .. } => "merge_not_ready",
            Rejection::PermissionAlreadyAnswered => "permission_already_answered",
            Rejection::DecisionNotFound => "decision_not_found",
            Rejection::DecisionAlreadyAnswered => "decision_already_answered",
            Rejection::DecisionAnswerInvalid => "decision_answer_invalid",
            Rejection::DecisionOnePerSession => "decision_one_per_session",
            Rejection::DecisionLimitReached => "decision_limit_reached",
            Rejection::RegressionNotAvailable { .. } => "regression_not_available",
            Rejection::RegressionLimitReached => "regression_limit_reached",
            Rejection::DraftNotWaiting => "draft_not_waiting",
            Rejection::DraftLintFailed => "draft_lint_failed",
            Rejection::DraftDecisionOpen => "draft_decision_open",
            Rejection::DraftNoChanges => "draft_no_changes",
            Rejection::DraftBaseMoved { .. } => "draft_base_moved",
            Rejection::FileNotFound { .. } => "file_not_found",
            Rejection::NotFound => "not_found",
            Rejection::ProjectExists { .. } => "project_exists",
            Rejection::SpecSlugConflict { .. } => "spec_slug_conflict",
            Rejection::QuickSpecNotApproved { .. } => "quick_spec_not_approved",
            Rejection::QuickSpecRenderMismatch => "quick_spec_render_mismatch",
            Rejection::TaskBranchConflict { .. } => "task_branch_conflict",
            Rejection::ScmIntegrationUnknown { .. } => "scm_integration_unknown",
            Rejection::ProjectConfigInvalid { .. } => "project_config_invalid",
            Rejection::ScmRepoInaccessible { .. } => "scm_repo_inaccessible",
            Rejection::ProjectRepoInUse { .. } => "project_repo_in_use",
            Rejection::SpecFileNotFound { .. } => "spec_file_not_found",
            Rejection::WorkPacketUnavailable { .. } => "work_packet_unavailable",
            Rejection::RetryLimitReached { .. } => "retry_limit_reached",
            Rejection::SpecDraftTargetTaken { .. } => "spec_draft_target_taken",
            Rejection::SecretTooShort => "secret_too_short",
            Rejection::SecretNameInvalid => "secret_name_invalid",
            Rejection::FileStale { .. } => "file_stale",
            Rejection::FilePathConflict { .. } => "file_path_conflict",
            Rejection::FilePushRejected { .. } => "file_push_rejected",
            Rejection::FileUnsupported { .. } => "file_unsupported",
            Rejection::UserLinkNotFound => "user_link_not_found",
            Rejection::InboxItemNotFound => "inbox_item_not_found",
            Rejection::InboxItemNotDismissible { .. } => "inbox_item_not_dismissible",
            Rejection::InboxItemClosed { .. } => "inbox_item_closed",
            Rejection::IssueNotFound => "issue_not_found",
            Rejection::IntakeActive { .. } => "intake_active",
            Rejection::IntakeAlreadyApproved => "intake_already_approved",
            Rejection::IntakeClosed { .. } => "intake_closed",
            Rejection::IntakeNotFailed => "intake_not_failed",
            Rejection::IntakeRetriesExhausted => "intake_retries_exhausted",
            Rejection::DeviceFlowUnconfigured { .. } => "device_flow_unconfigured",
            Rejection::UserAuthorshipUnsupported { .. } => "user_authorship_unsupported",
            Rejection::UserLinkInUse { .. } => "user_link_in_use",
            Rejection::UserLinkMissing { .. } => "user_link_missing",
            Rejection::UserTokenMissing => "user_token_missing",
            Rejection::UserTokenInvalid { .. } => "user_token_invalid",
            Rejection::UserTokenInUse => "user_token_in_use",
            Rejection::UserTokenMultiIntegration { .. } => "user_token_multi_integration",
            Rejection::UserTokenNotFineGrained => "user_token_not_fine_grained",
            Rejection::UserTokenRejected { .. } => "user_token_rejected",
            Rejection::UserTokenRepoInaccessible { .. } => "user_token_repo_inaccessible",
            Rejection::ReviewAlreadyRequested { .. } => "review_already_requested",
            Rejection::FixNotAvailable { .. } => "fix_not_available",
            Rejection::ChangeRequestNotOpen { .. } => "change_request_not_open",
            Rejection::ChangeRequestTracked { .. } => "change_request_tracked",
            Rejection::ChangeRequestNotFound { .. } => "change_request_not_found",
            Rejection::ChangeRequestOwned { .. } => "change_request_owned",
            Rejection::EntryPointDisabled { .. } => "entry_point_disabled",
            Rejection::StageNotInWorkflow { .. } => "stage_not_in_workflow",
            Rejection::StageNotTakeable { .. } => "stage_not_takeable",
            Rejection::ReturnTargetInvalid { .. } => "return_target_invalid",
            Rejection::RevertNotAllowed { .. } => "revert_not_allowed",
            Rejection::RevertInProgress { .. } => "revert_in_progress",
            Rejection::AlreadyReverted { .. } => "already_reverted",
            Rejection::BlockerNotOpen { .. } => "blocker_not_open",
            Rejection::BlockerActionChanged { .. } => "blocker_action_changed",
            Rejection::ActionNeedsInput { .. } => "action_needs_input",
            Rejection::HookRunPending => "hook_run_pending",
            Rejection::HookNotConfigured { .. } => "hook_not_configured",
            Rejection::HookRunGated { .. } => "hook_run_gated",
            Rejection::RuleDuplicate { .. } => "rule_duplicate",
            Rejection::RuleTooBroad => "rule_too_broad",
            Rejection::RuleInvalid { .. } => "rule_invalid",
            Rejection::PairingCodeInvalid => "pairing_code_invalid",
            Rejection::LastCredential => "last_credential",
            Rejection::DeviceNotRevocable => "device_not_revocable",
            Rejection::Invalid { .. } => "validation_failed",
        }
    }

    pub fn params(&self) -> Value {
        match self {
            Rejection::InvalidState { state } => json!({ "state": state }),
            Rejection::SpecNotReady { issues } => json!({ "issues": issues }),
            Rejection::SpecReparseRequired {
                parser_version,
                current_parser_version,
            } => json!({
                "parser_version": parser_version,
                "current_parser_version": current_parser_version,
            }),
            Rejection::RevisionConflict { steps } => json!({ "steps": steps }),
            Rejection::RevisionKindChanged => json!({ "reason": "kind_changed" }),
            Rejection::SpecHasActiveTasks { tasks } => json!({ "tasks": tasks }),
            Rejection::MergeNotReady {
                reason,
                gate_outcome,
                head_sha,
                review_head_sha,
                review_outcome,
            } => json!({
                "reason": reason,
                "gate_outcome": gate_outcome,
                "head_sha": head_sha,
                "review_head_sha": review_head_sha,
                "review_outcome": review_outcome,
            }),
            Rejection::ProjectExists { project_id } => json!({ "project_id": project_id }),
            Rejection::SpecSlugConflict { slug, path } => json!({ "slug": slug, "path": path }),
            Rejection::QuickSpecNotApproved { code, spec } => json!({ "code": code, "spec": spec }),
            Rejection::TaskBranchConflict { branches } => json!({ "branches": branches }),
            Rejection::ScmIntegrationUnknown { host, repo } => {
                json!({ "host": host, "repo": repo })
            }
            Rejection::ScmRepoInaccessible { repo } => json!({ "repo": repo }),
            Rejection::ProjectConfigInvalid { issues } => json!({ "issues": issues }),
            Rejection::ProjectRepoInUse { repos } => json!({ "repos": repos }),
            Rejection::SpecFileNotFound { path, base_branch } => {
                json!({ "path": path, "base_branch": base_branch })
            }
            Rejection::WorkPacketUnavailable { reason } => json!({ "reason": reason }),
            Rejection::RetryLimitReached {
                stage: Some(stage),
                limit,
            } => json!({ "stage": stage, "limit": limit }),
            Rejection::RetryLimitReached { stage: None, limit } => json!({ "limit": limit }),
            Rejection::SpecDraftTargetTaken { path, taken } => {
                let mut params = taken.params();
                params["path"] = json!(path);
                params
            }
            Rejection::DraftBaseMoved { paths } => json!({ "paths": paths }),
            Rejection::FileNotFound { path } => json!({ "path": path }),
            Rejection::FileStale { blob, commit } => json!({ "blob": blob, "commit": commit }),
            Rejection::RevertNotAllowed { reason } => json!({ "reason": reason }),
            Rejection::RevertInProgress { work_item_id }
            | Rejection::AlreadyReverted { work_item_id } => {
                json!({ "work_item_id": work_item_id })
            }
            Rejection::BlockerNotOpen { blocker_id } => json!({ "blocker_id": blocker_id }),
            Rejection::BlockerActionChanged {
                blocker_id,
                expected,
                actual,
            } => json!({ "blocker_id": blocker_id, "expected": expected, "actual": actual }),
            Rejection::ActionNeedsInput { action } => json!({ "action": action }),
            Rejection::InboxItemNotDismissible { subject } => json!({ "subject": subject }),
            Rejection::InboxItemClosed { state } => json!({ "state": state }),
            Rejection::IntakeClosed { state } => json!({ "state": state }),
            Rejection::IntakeActive { intake_id } => json!({ "intake_id": intake_id }),
            Rejection::HookNotConfigured { hook } => json!({ "hook": hook }),
            Rejection::HookRunGated { work_item_id } => json!({ "work_item_id": work_item_id }),
            Rejection::RuleDuplicate { rule_id } => json!({ "rule_id": rule_id }),
            Rejection::RuleInvalid { field, reason } => json!({ "field": field, "reason": reason }),
            Rejection::FilePathConflict { path } => json!({ "path": path }),
            Rejection::FilePushRejected { branch } => json!({ "branch": branch }),
            Rejection::FileUnsupported { reason } => reason.params(),
            Rejection::DeviceFlowUnconfigured { integration_id }
            | Rejection::UserAuthorshipUnsupported { integration_id } => {
                json!({ "integration_id": integration_id })
            }
            Rejection::UserLinkInUse { projects } => json!({ "projects": projects }),
            Rejection::UserLinkMissing { integrations } => json!({ "integrations": integrations }),
            Rejection::UserTokenInvalid { cause } => json!({ "cause": cause }),
            Rejection::UserTokenMultiIntegration { integrations } => {
                json!({ "integrations": integrations })
            }
            Rejection::UserTokenRejected { status } => json!({ "status": status }),
            Rejection::UserTokenRepoInaccessible { repos } => json!({ "repos": repos }),
            Rejection::ReviewAlreadyRequested { head_sha } => json!({ "head_sha": head_sha }),
            Rejection::FixNotAvailable { reason }
            | Rejection::RegressionNotAvailable { reason } => {
                json!({ "reason": reason })
            }
            Rejection::ChangeRequestNotOpen { number, state } => {
                json!({ "number": number, "state": state })
            }
            Rejection::ChangeRequestNotFound { repo, number } => {
                json!({ "repo": repo, "number": number })
            }
            Rejection::ChangeRequestOwned { work_item_id }
            | Rejection::ChangeRequestTracked { work_item_id } => {
                json!({ "work_item_id": work_item_id })
            }
            Rejection::EntryPointDisabled { entry } => json!({ "entry": entry }),
            Rejection::StageNotInWorkflow { stage } => json!({ "stage": stage }),
            Rejection::StageNotTakeable { stage, phase } => {
                json!({ "stage": stage, "phase": phase })
            }
            Rejection::ReturnTargetInvalid { stage, reason } => {
                json!({ "stage": stage, "reason": reason })
            }
            Rejection::Invalid { field, code } => {
                json!({ "errors": [{ "field": field, "code": code }] })
            }
            _ => json!({}),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetTakenBy {
    File {
        blob: Option<String>,
        commit: String,
    },
    Spec {
        spec_id: SpecId,
    },
    Slug {
        spec_id: SpecId,
    },
    Draft {
        spec_draft_id: SpecDraftId,
    },
}

impl TargetTakenBy {
    pub fn reason(&self) -> TargetTaken {
        match self {
            TargetTakenBy::File { .. } => TargetTaken::FileExists,
            TargetTakenBy::Spec { .. } => TargetTaken::SpecRegistered,
            TargetTakenBy::Slug { .. } => TargetTaken::SlugTaken,
            TargetTakenBy::Draft { .. } => TargetTaken::DraftActive,
        }
    }

    pub fn params(&self) -> Value {
        let reason = self.reason();
        match self {
            TargetTakenBy::File { blob, commit } => {
                json!({ "reason": reason, "blob": blob, "commit": commit })
            }
            TargetTakenBy::Spec { spec_id } | TargetTakenBy::Slug { spec_id } => {
                json!({ "reason": reason, "spec_id": spec_id })
            }
            TargetTakenBy::Draft { spec_draft_id } => {
                json!({ "reason": reason, "spec_draft_id": spec_draft_id })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileUnsupported {
    TooLarge { bytes: u64, max_bytes: u64 },
    NotUtf8,
    NotRegular,
}

impl FileUnsupported {
    pub fn params(self) -> Value {
        match self {
            FileUnsupported::TooLarge { bytes, max_bytes } => {
                json!({ "reason": "too_large", "bytes": bytes, "max_bytes": max_bytes })
            }
            FileUnsupported::NotUtf8 => json!({ "reason": "not_utf8" }),
            FileUnsupported::NotRegular => json!({ "reason": "not_regular" }),
        }
    }
}
