use async_trait::async_trait;
use saka_core::diagnostics::{
    CredentialStatus, RepoGrant, RepoHooks, ScmDiagnostics, WebhookStatus,
};
use saka_core::gate::Check;
use saka_core::ids::RepoId;
use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_core::vocab::{AuthorizationFailure, ChangeState, MergeMethod, ReviewEvent};

use crate::{PortError, PortResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub repo_id: RepoId,
    pub integration_id: String,
    pub full_name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub full_name: String,
    pub url: String,
    pub default_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRequest {
    pub repo: RepoRef,
    pub branch: String,
    pub local_sha: String,
    pub expected_remote_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    Pushed { sha: String },
    Rejected { remote_sha: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeCredential {
    App,
    User(SecretText),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenChangeRequest {
    pub repo: RepoRef,
    pub head_branch: String,
    pub base_branch: String,
    pub title: String,
    pub body: String,
    pub credential: ChangeCredential,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeOpening {
    Opened(OpenedChange),
    CredentialRejected,
    AccessDenied { status: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedChange {
    pub number: u64,
    pub url: String,
    pub head_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSnapshot {
    pub state: ChangeState,
    pub head_sha: String,
    pub merge_commit_sha: Option<String>,
    pub merged_at: Option<Timestamp>,
    pub checks: Vec<Check>,
}

pub const MAX_OPEN_CHANGES: usize = 200;
pub const MAX_CHANGE_FILES: usize = 3000;
pub const MAX_SIGNALS: usize = 100;
pub const MAX_CHANGE_TEXT_BYTES: usize = 4000;

pub fn unique_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    paths
        .into_iter()
        .filter(|p| seen.insert(*p))
        .map(String::from)
        .collect()
}

pub fn clip_text(text: &str) -> String {
    let cut = (0..=MAX_CHANGE_TEXT_BYTES.min(text.len()))
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    text[..cut].to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSummary {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub head_branch: String,
    pub base_branch: String,
    pub head_sha: String,
    pub author: String,
    pub labels: Vec<String>,
    pub draft: bool,
    pub from_fork: bool,
    pub state: ChangeState,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalKind {
    Comment { body: String },
    Label { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSignal {
    pub id: String,
    pub at: Timestamp,
    pub actor: String,
    pub kind: SignalKind,
}

pub const MAX_REVIEW_BODY_CHARS: usize = saka_core::review::MAX_REVIEW_BODY_CHARS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPost {
    pub repo: RepoRef,
    pub number: u64,
    pub head_sha: String,
    pub event: ReviewEvent,
    pub body: String,
}

impl ReviewPost {
    pub fn check(&self) -> PortResult<()> {
        let sha = self.head_sha.len() == 40 && self.head_sha.bytes().all(|b| b.is_ascii_hexdigit());
        if !sha {
            return Err(PortError::permanent(
                "git_argument_invalid",
                serde_json::json!({ "field": "head_sha" }),
            ));
        }
        if self.body.chars().count() > MAX_REVIEW_BODY_CHARS {
            return Err(PortError::permanent(
                "git_argument_invalid",
                serde_json::json!({ "field": "body" }),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostedReview {
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRequest {
    pub repo: RepoRef,
    pub number: u64,
    pub head_sha: String,
    pub method: MergeMethod,
    pub commit_title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged {
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
    },
    Rejected {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    Queued,
    Merged {
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
    },
    Rejected {
        reason: String,
    },
}

#[async_trait]
pub trait Scm: Send + Sync {
    async fn repository(&self, integration_id: &str, full_name: &str) -> PortResult<RepoInfo>;

    async fn branch_head(
        &self,
        integration_id: &str,
        full_name: &str,
        branch: &str,
    ) -> PortResult<Option<String>>;

    async fn file_at(
        &self,
        integration_id: &str,
        full_name: &str,
        commit: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<Option<Vec<u8>>>;

    async fn push_branch(&self, request: &PushRequest) -> PortResult<PushOutcome>;

    async fn open_change_request(&self, request: &OpenChangeRequest) -> PortResult<ChangeOpening>;

    async fn change_request(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSnapshot>;

    async fn commit_checks(&self, repo: &RepoRef, sha: &str) -> PortResult<Vec<Check>>;

    async fn list_open_changes(&self, repo: &RepoRef, max: usize)
    -> PortResult<Vec<ChangeSummary>>;

    async fn change_summary(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSummary>;

    async fn change_files(
        &self,
        repo: &RepoRef,
        number: u64,
        max: usize,
    ) -> PortResult<Vec<String>>;

    async fn change_signals(
        &self,
        repo: &RepoRef,
        number: u64,
        since: Timestamp,
    ) -> PortResult<Vec<ChangeSignal>>;

    async fn can_write(&self, repo: &RepoRef, login: &str) -> PortResult<bool>;

    async fn post_review(&self, post: &ReviewPost) -> PortResult<PostedReview>;

    async fn merge(&self, request: &MergeRequest) -> PortResult<MergeOutcome>;

    async fn merge_queue(&self, repo: &RepoRef, base_branch: &str) -> PortResult<bool>;

    async fn enqueue(&self, request: &MergeRequest) -> PortResult<EnqueueOutcome>;

    async fn in_merge_queue(&self, repo: &RepoRef, number: u64) -> PortResult<bool>;

    async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> PortResult<()>;

    async fn report_status(
        &self,
        repo: &RepoRef,
        sha: &str,
        report: &StatusReport,
    ) -> PortResult<()>;

    fn capabilities(&self, integration_id: &str) -> Option<ScmCapabilities>;

    async fn credential_status(&self, integration_id: &str) -> PortResult<CredentialStatus>;

    async fn repo_grant(&self, repo: &RepoRef) -> PortResult<RepoGrant>;

    async fn repo_webhooks(&self, repo: &RepoRef) -> PortResult<RepoHooks>;

    async fn webhook_status(&self, integration_id: &str) -> PortResult<WebhookStatus>;

    fn webhook_configured(&self, integration_id: &str) -> PortResult<bool>;

    fn parse_webhook(
        &self,
        integration_id: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> PortResult<WebhookDelivery>;

    fn device_flow_configured(&self, integration_id: &str) -> PortResult<bool>;

    async fn start_device_flow(&self, integration_id: &str) -> PortResult<DeviceCode>;

    async fn poll_device_flow(
        &self,
        integration_id: &str,
        device_code: &SecretText,
    ) -> PortResult<DevicePoll>;

    async fn refresh_user_token(
        &self,
        integration_id: &str,
        refresh_token: &SecretText,
    ) -> PortResult<TokenRefresh>;

    async fn user_identity(
        &self,
        integration_id: &str,
        token: &SecretText,
    ) -> PortResult<IdentityLookup>;

    async fn user_repository_access(
        &self,
        integration_id: &str,
        full_name: &str,
        token: &SecretText,
    ) -> PortResult<RepoAccess>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    pub device_code: SecretText,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in_secs: u64,
    pub interval_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserTokens {
    pub access_token: SecretText,
    pub expires_in_secs: Option<u64>,
    pub refresh_token: Option<SecretText>,
    pub refresh_expires_in_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevicePoll {
    Pending,
    SlowDown { interval_secs: Option<u64> },
    Expired,
    Denied,
    Failed { code: AuthorizationFailure },
    Token(UserTokens),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenRefresh {
    Refreshed(UserTokens),
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityLookup {
    Found {
        identity: saka_core::authorship::Identity,
        token_expires_at: Option<Timestamp>,
    },
    Rejected {
        status: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoAccess {
    Readable,
    Inaccessible { status: u16 },
}

pub const MAX_WEBHOOK_TARGETS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusState {
    Pending,
    Success,
    Failure,
}

impl StatusState {
    pub fn as_str(self) -> &'static str {
        match self {
            StatusState::Pending => "pending",
            StatusState::Success => "success",
            StatusState::Failure => "failure",
        }
    }

    pub fn parse(s: &str) -> Option<StatusState> {
        match s {
            "pending" => Some(StatusState::Pending),
            "success" => Some(StatusState::Success),
            "failure" => Some(StatusState::Failure),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    pub context: String,
    pub state: StatusState,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookSignature {
    HmacSha256,
    SharedToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScmCapabilities {
    pub provider: &'static str,
    pub merge_methods: &'static [saka_core::vocab::MergeMethod],
    pub webhook_signature: Option<WebhookSignature>,
    pub check_kinds: &'static [saka_core::vocab::CheckKind],
    pub draft_change_requests: bool,
    pub delete_branch_after_merge: bool,
    pub commit_status: bool,
    pub merge_queue: bool,
    pub user_authorship: bool,
    pub change_head_ref: &'static str,
    pub diagnostics: Option<ScmDiagnostics>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WebhookTarget {
    Change { number: u64 },
    Head { sha: String },
    Intake,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDelivery {
    pub delivery_id: String,
    pub event: String,
    pub action: Option<String>,
    pub repo_full_name: Option<String>,
    pub targets: Vec<WebhookTarget>,
}
