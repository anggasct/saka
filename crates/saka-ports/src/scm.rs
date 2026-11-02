use async_trait::async_trait;
use saka_core::gate::Check;
use saka_core::ids::RepoId;
use saka_core::time::Timestamp;
use saka_core::vocab::{ChangeState, MergeMethod};

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
pub struct OpenChangeRequest {
    pub repo: RepoRef,
    pub head_branch: String,
    pub base_branch: String,
    pub title: String,
    pub body: String,
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

    async fn open_change_request(&self, request: &OpenChangeRequest) -> PortResult<OpenedChange>;

    async fn change_request(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSnapshot>;

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
