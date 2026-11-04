use async_trait::async_trait;
use saka_core::gate::Check;
use saka_core::ids::RepoId;
use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_core::vocab::{AuthorizationFailure, ChangeState, MergeMethod};

use crate::PortResult;

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

#[async_trait]
pub trait Scm: Send + Sync {
    async fn repository(&self, integration_id: &str, full_name: &str) -> PortResult<RepoInfo>;

    async fn push_branch(&self, request: &PushRequest) -> PortResult<PushOutcome>;

    async fn open_change_request(&self, request: &OpenChangeRequest) -> PortResult<ChangeOpening>;

    async fn change_request(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSnapshot>;

    async fn commit_checks(&self, repo: &RepoRef, sha: &str) -> PortResult<Vec<Check>>;

    async fn merge(&self, request: &MergeRequest) -> PortResult<MergeOutcome>;

    async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> PortResult<()>;

    async fn report_status(
        &self,
        repo: &RepoRef,
        sha: &str,
        report: &StatusReport,
    ) -> PortResult<()>;

    fn capabilities(&self, integration_id: &str) -> Option<ScmCapabilities>;

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
    Found(saka_core::authorship::Identity),
    Rejected { status: u16 },
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
    pub user_authorship: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WebhookTarget {
    Change { number: u64 },
    Head { sha: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDelivery {
    pub delivery_id: String,
    pub event: String,
    pub action: Option<String>,
    pub repo_full_name: Option<String>,
    pub targets: Vec<WebhookTarget>,
}
