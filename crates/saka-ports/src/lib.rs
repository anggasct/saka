pub mod agent;
pub mod docs;
pub mod env;
pub mod git;
pub mod scm;
pub mod secret;
pub mod supervisor;
pub mod toolchain;

use saka_core::ids::IdSource;
use saka_core::time::Timestamp;
use serde_json::{Value, json};
use uuid::Uuid;

pub use agent::{
    Agents, LaunchedSession, Link, Reattached, RunnerEvent, RunnerEventKind, SessionLaunch,
    SessionRef,
};
pub use docs::{DocFile, DocSource};
pub use env::{
    EnvRef, Environments, PrepareRequest, PreparedEnvironment, RecipeCommand, RecipeCommandOutcome,
    RecipePhase, WorktreeState,
};
pub use git::{Blob, GitAccess, GitRemote, Repos};
pub use scm::{
    ChangeSnapshot, MAX_WEBHOOK_TARGETS, MergeOutcome, MergeRequest, OpenChangeRequest,
    OpenedChange, PushOutcome, PushRequest, RepoInfo, RepoRef, Scm, ScmCapabilities,
    WebhookDelivery, WebhookSignature, WebhookTarget,
};
pub use secret::{Sealed, SecretStore};
pub use supervisor::{
    COMMAND_OUTPUT_MAX, CommandOutcome, CommandUnit, RunnerUnit, Supervisor, UnitLimits,
};
pub use toolchain::Toolchains;

pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

pub trait Ids: Send + Sync {
    fn new_id(&self) -> Uuid;
}

pub struct IdsSource<'a>(pub &'a dyn Ids);

impl IdSource for IdsSource<'_> {
    fn next_id(&mut self) -> Uuid {
        self.0.new_id()
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{code}")]
pub struct PortError {
    pub code: String,
    pub params: Value,
    pub retryable: bool,
    pub retry_at: Option<Timestamp>,
}

impl PortError {
    pub fn retryable(code: impl Into<String>, params: Value) -> PortError {
        PortError {
            code: code.into(),
            params,
            retryable: true,
            retry_at: None,
        }
    }

    pub fn permanent(code: impl Into<String>, params: Value) -> PortError {
        PortError {
            code: code.into(),
            params,
            retryable: false,
            retry_at: None,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({ "code": self.code, "params": self.params })
    }
}

pub type PortResult<T> = Result<T, PortError>;
