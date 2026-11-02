use serde_json::{Value, json};

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
    ScmIntegrationUnknown { host: String },
    #[error("project config is not valid")]
    ProjectConfigInvalid { issues: Value },
    #[error("project config lists more than one repo")]
    MultiRepoUnsupported { repos: usize },
    #[error("spec file not found on the base branch")]
    SpecFileNotFound { path: String, base_branch: String },
    #[error("work packet is not available")]
    WorkPacketUnavailable { reason: &'static str },
    #[error("secret value is too short")]
    SecretTooShort,
    #[error("secret name is not valid")]
    SecretNameInvalid,
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
            Rejection::MultiRepoUnsupported { .. } => "multi_repo_unsupported",
            Rejection::SpecFileNotFound { .. } => "spec_file_not_found",
            Rejection::WorkPacketUnavailable { .. } => "work_packet_unavailable",
            Rejection::SecretTooShort => "secret_too_short",
            Rejection::SecretNameInvalid => "secret_name_invalid",
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
            Rejection::ScmIntegrationUnknown { host } => json!({ "host": host }),
            Rejection::ProjectConfigInvalid { issues } => json!({ "issues": issues }),
            Rejection::MultiRepoUnsupported { repos } => json!({ "repos": repos }),
            Rejection::SpecFileNotFound { path, base_branch } => {
                json!({ "path": path, "base_branch": base_branch })
            }
            Rejection::WorkPacketUnavailable { reason } => json!({ "reason": reason }),
            Rejection::Invalid { field, code } => {
                json!({ "errors": [{ "field": field, "code": code }] })
            }
            _ => json!({}),
        }
    }
}
