//! Source control: Saka pushes task branches, opens change requests, reads CI at the
//! head, merges at the gate head, and deletes branches. Every call checks the current
//! state first, so repeating it after a crash has no second effect.

use async_trait::async_trait;
use saka_core::gate::Check;
use saka_core::ids::RepoId;
use saka_core::time::Timestamp;
use saka_core::vocab::{ChangeState, MergeMethod};

use crate::PortResult;

/// A repository as the adapters need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub repo_id: RepoId,
    /// Which configured SCM integration serves this repo.
    pub integration_id: String,
    /// `owner/name`.
    pub full_name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRequest {
    pub repo: RepoRef,
    pub branch: String,
    pub local_sha: String,
    /// The sha Saka pushed last, used as the lease; `None` for a new branch.
    pub expected_remote_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushOutcome {
    /// The remote branch now points at the local sha (also when it already did).
    Pushed { sha: String },
    /// The remote branch points at a sha Saka did not push.
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

/// A change request as read now, with CI normalized at its head.
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
    /// The head the gate passed at; the merge fails if the change moved since.
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
    /// `not_mergeable`, `head_changed`, or `rejected`.
    Rejected { reason: String },
}

#[async_trait]
pub trait Scm: Send + Sync {
    async fn push_branch(&self, request: &PushRequest) -> PortResult<PushOutcome>;

    /// Opens the change request, or returns the one that already exists for the branch.
    async fn open_change_request(&self, request: &OpenChangeRequest) -> PortResult<OpenedChange>;

    async fn change_request(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSnapshot>;

    /// Merges at `head_sha`. The engine reads the change first and skips the merge when it
    /// is already merged.
    async fn merge(&self, request: &MergeRequest) -> PortResult<MergeOutcome>;

    /// Deletes the remote branch; a branch that is already gone is a success.
    async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> PortResult<()>;
}
