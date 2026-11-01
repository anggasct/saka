//! Why a decision refused a command. Each rejection maps to one stable API error code.

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
    #[error("merge is not ready")]
    MergeNotReady {
        gate_outcome: Option<String>,
        head_sha: Option<String>,
    },
    #[error("permission request is already answered")]
    PermissionAlreadyAnswered,
    #[error("not found")]
    NotFound,
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
            Rejection::MergeNotReady { .. } => "merge_not_ready",
            Rejection::PermissionAlreadyAnswered => "permission_already_answered",
            Rejection::NotFound => "not_found",
        }
    }

    pub fn params(&self) -> Value {
        match self {
            Rejection::InvalidState { state } => json!({ "state": state }),
            Rejection::SpecNotReady { issues } => json!({ "issues": issues }),
            Rejection::MergeNotReady {
                gate_outcome,
                head_sha,
            } => {
                json!({ "gate_outcome": gate_outcome, "head_sha": head_sha })
            }
            _ => json!({}),
        }
    }
}
