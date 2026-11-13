pub mod agent;
mod best_effort;
pub mod chat;
pub mod decisions;
pub mod docs;
pub mod env;
pub mod git;
pub mod hook;
pub mod intake;
pub mod regressions;
pub mod scm;
pub mod secret;
pub mod signal;
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
pub use best_effort::BestEffort;
pub use chat::{ChatAction, ChatActionKind, ChatActions, ChatOutcome};
pub use decisions::{AskRefusal, Asked, Decisions};
pub use docs::{
    DocBlob, DocCommit, DocEntry, DocExpect, DocFile, DocFileCommitted, DocFileWrite,
    DocFilesCommit, DocFilesWrite, DocOpened, DocSource, DocStale, DocTree, DocWrite,
};
pub use env::{
    BaseProbe, CODE_DIR, CODE_MAX_BYTES, CODE_MAX_FILES, ChangeDiff, ChangedPaths, Checkout,
    CodeCheckout, CodeRequest, DOCS_MAX_BYTES, DOCS_MAX_FILES, DocsRequest, EnvRef, Environments,
    ExtraDir, OUTPUT_TAIL_BYTES, OUTPUT_TAIL_LINES, PrepareRequest, PreparedCode, PreparedDocs,
    PreparedEnvironment, REVERT_PATHS_MAX, Rebase, RecipeCommand, RecipeCommandOutcome,
    RecipePhase, Release, ReleaseIndex, RevertRequest, Reverted, WorktreeChange, WorktreeFile,
    WorktreeState, code_path, output_tail,
};
pub use git::{
    Blob, CommitFile, CommitFiles, CommitPush, EntryKind, GitAccess, GitRemote, PushCommit, Repos,
    TreeEntry,
};
pub use hook::{
    HostCommand, HostCommands, HostOutcome, Notification, Notifier, WebhookPost, WebhookReceipt,
    Webhooks,
};
pub use intake::{
    IntakeCapabilities, IntakeSource, Issue, IssueCloseOutcome, IssueListing, IssueRef,
    IssueSignal, IssueSignalKind, IssueState, IssueSummary, MAX_ISSUE_BODY_BYTES, MAX_ISSUES,
    TRUNCATION_MARKER, clip_issue_body,
};
pub use regressions::{RegressionRefusal, Regressions, Requested};
pub use scm::{
    ChangeCredential, ChangeOpening, ChangeSignal, ChangeSnapshot, ChangeSummary, DeviceCode,
    DevicePoll, EnqueueOutcome, IdentityLookup, MAX_CHANGE_FILES, MAX_CHANGE_TEXT_BYTES,
    MAX_OPEN_CHANGES, MAX_REVIEW_BODY_CHARS, MAX_SIGNALS, MAX_WEBHOOK_TARGETS, MergeOutcome,
    MergeRequest, OpenChangeRequest, OpenedChange, PostedReview, PushOutcome, PushRequest,
    RepoAccess, RepoInfo, RepoRef, RerunOutcome, ReviewPost, Scm, ScmCapabilities, SignalKind,
    StatusReport, StatusState, TokenRefresh, UpdateOutcome, UserTokens, WebhookDelivery,
    WebhookSignature, WebhookTarget, check_comment_body, check_sha, clip_text, unique_paths,
};
pub use secret::{Sealed, SecretStore};
pub use signal::{
    BODY_DEDUP_WINDOW_SECS, DecodedDelivery, IncidentSeverity, IncidentSignal,
    MAX_DELIVERY_ID_CHARS, MAX_FINGERPRINT_CHARS, MAX_LABEL_KEY_CHARS, MAX_LABEL_VALUE_CHARS,
    MAX_LABELS, MAX_SIGNALS_PER_DELIVERY, MAX_SUMMARY_BYTES, MAX_TITLE_CHARS, MAX_URL_BYTES,
    RawDelivery, SignalDecoder, SignalRequest, SignalStatus, Signals, TIMESTAMP_WINDOW_SECS,
};
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

pub trait Entropy: Send + Sync {
    fn fill(&self, buf: &mut [u8]) -> PortResult<()>;
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

    pub fn unsupported(capability: &str) -> PortError {
        PortError::permanent(
            "scm_capability_unsupported",
            json!({ "capability": capability }),
        )
    }

    pub fn to_json(&self) -> Value {
        json!({ "code": self.code, "params": self.params })
    }
}

pub type PortResult<T> = Result<T, PortError>;
