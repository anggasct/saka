pub mod agent;
pub mod chat;
pub mod docs;
pub mod env;
pub mod git;
pub mod hook;
pub mod scm;
pub mod secret;
pub mod supervisor;
pub mod toolchain;

use saka_core::ids::IdSource;
use saka_core::time::Timestamp;
use serde_json::{Value, json};
use uuid::Uuid;

pub use agent::{
    Agents, LaunchProfile, LaunchedSession, Link, MAX_RESULT_TEXT_BYTES, Reattached, RunnerEvent,
    RunnerEventKind, SessionLaunch, SessionRef,
};
pub use chat::{ChatAction, ChatActionKind, ChatActions, ChatOutcome};
pub use docs::{
    DocBlob, DocCommit, DocEntry, DocExpect, DocFile, DocOpened, DocSource, DocTree, DocWrite,
};
pub use env::{
    ChangeDiff, ChangedPaths, Checkout, DOCS_MAX_BYTES, DOCS_MAX_FILES, DocsRequest, EnvRef,
    Environments, ExtraDir, OUTPUT_TAIL_BYTES, OUTPUT_TAIL_LINES, PrepareRequest, PreparedDocs,
    PreparedEnvironment, Rebase, RecipeCommand, RecipeCommandOutcome, RecipePhase, Release,
    ReleaseIndex, WorktreeFile, WorktreeState, output_tail,
};
pub use git::{
    Blob, CommitOne, CommitPush, EntryKind, GitAccess, GitRemote, PushCommit, Repos, TreeEntry,
};
pub use hook::{
    HostCommand, HostCommands, HostOutcome, Notification, Notifier, WebhookPost, WebhookReceipt,
    Webhooks,
};
pub use scm::{
    ChangeCredential, ChangeOpening, ChangeSignal, ChangeSnapshot, ChangeSummary, DeviceCode,
    DevicePoll, IdentityLookup, MAX_CHANGE_FILES, MAX_CHANGE_TEXT_BYTES, MAX_OPEN_CHANGES,
    MAX_REVIEW_BODY_CHARS, MAX_SIGNALS, MAX_WEBHOOK_TARGETS, MergeOutcome, MergeRequest,
    OpenChangeRequest, OpenedChange, PostedReview, PushOutcome, PushRequest, RepoAccess, RepoInfo,
    RepoRef, ReviewPost, Scm, ScmCapabilities, SignalKind, StatusReport, StatusState, TokenRefresh,
    UserTokens, WebhookDelivery, WebhookSignature, WebhookTarget, clip_text, unique_paths,
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
