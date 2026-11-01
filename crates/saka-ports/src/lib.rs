//! Port traits and data types shared by the engine and the adapters.
//!
//! The engine talks to the outside only through these traits; adapters implement them
//! and the binary wires them together. Ports are async (the shell runs on tokio) and
//! object safe, so the engine holds them as `Arc<dyn …>`.

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

pub use agent::{Agents, LaunchedSession, RunnerEvent, RunnerEventKind, SessionLaunch, SessionRef};
pub use docs::{DocFile, DocSource};
pub use env::{EnvRef, Environments, PrepareRequest, PreparedEnvironment, WorktreeState};
pub use git::{GitAccess, GitRemote, Repos};
pub use scm::{
    ChangeSnapshot, MergeOutcome, MergeRequest, OpenChangeRequest, OpenedChange, PushOutcome,
    PushRequest, RepoInfo, RepoRef, Scm,
};
pub use supervisor::{RunnerUnit, Supervisor, UnitLimits};

/// Wall-clock time in milliseconds since the Unix epoch (UTC). Virtual in tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// New ids: UUIDv7 in production, deterministic in tests.
pub trait Ids: Send + Sync {
    fn new_id(&self) -> Uuid;
}

/// Lends an [`Ids`] port to the core, which takes ids through [`IdSource`].
pub struct IdsSource<'a>(pub &'a dyn Ids);

impl IdSource for IdsSource<'_> {
    fn next_id(&mut self) -> Uuid {
        self.0.new_id()
    }
}

/// A failed port call, as a code with parameters.
///
/// `retryable` errors (network, 5xx, 429, a runner that is not reachable yet) are tried
/// again with backoff; the rest end the job at once. Business outcomes such as a rejected
/// push are not errors: they are part of the port's result type.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{code}")]
pub struct PortError {
    pub code: String,
    pub params: Value,
    pub retryable: bool,
    /// Earliest time to try again (for example a rate limit reset), if the adapter knows.
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

    /// The error as stored in `job.last_error`.
    pub fn to_json(&self) -> Value {
        json!({ "code": self.code, "params": self.params })
    }
}

pub type PortResult<T> = Result<T, PortError>;
