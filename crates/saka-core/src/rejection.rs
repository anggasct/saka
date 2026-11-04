use serde_json::{Value, json};

use crate::ids::{SpecDraftId, SpecId};
use crate::vocab::TargetTaken;

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
    #[error("spec has active tasks")]
    SpecHasActiveTasks { tasks: Value },
    #[error("merge is not ready")]
    MergeNotReady {
        gate_outcome: Option<String>,
        head_sha: Option<String>,
    },
    #[error("permission request is already answered")]
    PermissionAlreadyAnswered,
    #[error("not found")]
    NotFound,
    #[error("project is already registered")]
    ProjectExists { project_id: String },
    #[error("spec slug is already used")]
    SpecSlugConflict { slug: String, path: String },
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
            Rejection::RevisionConflict { .. } => "revision_conflict",
            Rejection::SpecHasActiveTasks { .. } => "spec_has_active_tasks",
            Rejection::MergeNotReady { .. } => "merge_not_ready",
            Rejection::PermissionAlreadyAnswered => "permission_already_answered",
            Rejection::NotFound => "not_found",
            Rejection::ProjectExists { .. } => "project_exists",
            Rejection::SpecSlugConflict { .. } => "spec_slug_conflict",
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
            Rejection::SpecHasActiveTasks { tasks } => json!({ "tasks": tasks }),
            Rejection::MergeNotReady {
                gate_outcome,
                head_sha,
            } => {
                json!({ "gate_outcome": gate_outcome, "head_sha": head_sha })
            }
            Rejection::ProjectExists { project_id } => json!({ "project_id": project_id }),
            Rejection::SpecSlugConflict { slug, path } => json!({ "slug": slug, "path": path }),
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
            Rejection::FileStale { blob, commit } => json!({ "blob": blob, "commit": commit }),
            Rejection::FilePathConflict { path } => json!({ "path": path }),
            Rejection::FilePushRejected { branch } => json!({ "branch": branch }),
            Rejection::FileUnsupported { reason } => reason.params(),
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
