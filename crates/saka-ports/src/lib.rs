pub mod agent;
pub mod docs;
pub mod env;
pub mod git;
pub mod scm;
pub mod supervisor;

use saka_core::ids::IdSource;
use saka_core::time::Timestamp;
use serde_json::{Value, json};
use uuid::Uuid;

pub use agent::{
    Agents, LaunchedSession, Link, Reattached, RunnerEvent, RunnerEventKind, SessionLaunch,
    SessionRef,
};
pub use docs::{DocFile, DocSource};
pub use env::{EnvRef, Environments, PrepareRequest, PreparedEnvironment, WorktreeState};
pub use git::{GitAccess, GitRemote, Repos};
pub use scm::{
    ChangeSnapshot, MergeOutcome, MergeRequest, OpenChangeRequest, OpenedChange, PushOutcome,
    PushRequest, RepoInfo, RepoRef, Scm,
};
pub use supervisor::{RunnerUnit, Supervisor, UnitLimits};

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
