use std::fmt;

use async_trait::async_trait;
use saka_core::authorship::GitIdentity;
use saka_core::time::Timestamp;

use crate::PortResult;
use crate::scm::{PushOutcome, PushRequest, RepoRef};

#[derive(Clone, PartialEq, Eq)]
pub struct GitRemote {
    pub url: String,
    pub config: Vec<(String, String)>,
}

impl fmt::Debug for GitRemote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys: Vec<&str> = self.config.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("GitRemote")
            .field("url", &self.url)
            .field("config_keys", &keys)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blob {
    Missing,
    TooLarge { bytes: u64 },
    Bytes(Vec<u8>),
}

#[async_trait]
pub trait GitAccess: Send + Sync {
    async fn remote(&self, repo: &RepoRef) -> PortResult<GitRemote>;

    async fn refused(&self, repo: &RepoRef);
}

#[async_trait]
pub trait Repos: Send + Sync {
    async fn fetch(&self, repo: &RepoRef) -> PortResult<()>;

    async fn tip(&self, repo: &RepoRef, branch: &str) -> PortResult<Option<String>>;

    async fn read_file(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
    ) -> PortResult<Option<Vec<u8>>>;

    async fn read_bounded(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<Blob>;

    async fn grep_paths(
        &self,
        repo: &RepoRef,
        commit: &str,
        pattern: &str,
        pathspecs: &[String],
    ) -> PortResult<Vec<String>>;

    async fn push(&self, request: &PushRequest) -> PortResult<PushOutcome>;

    async fn list_tree(
        &self,
        repo: &RepoRef,
        commit: &str,
        dir: &str,
    ) -> PortResult<Vec<TreeEntry>>;

    async fn entries(
        &self,
        repo: &RepoRef,
        commit: &str,
        paths: &[String],
    ) -> PortResult<Vec<TreeEntry>>;

    async fn write_blob(&self, repo: &RepoRef, bytes: &[u8]) -> PortResult<String>;

    async fn commit_one(&self, request: &CommitOne) -> PortResult<String>;

    async fn push_commit(&self, request: &PushCommit) -> PortResult<CommitPush>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Blob,
    Tree,
    Commit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: String,
    pub mode: String,
    pub kind: EntryKind,
    pub object: String,
    pub bytes: Option<u64>,
}

impl TreeEntry {
    pub const REGULAR: &'static str = "100644";
    pub const EXECUTABLE: &'static str = "100755";

    pub fn is_regular_file(&self) -> bool {
        self.kind == EntryKind::Blob
            && (self.mode == Self::REGULAR || self.mode == Self::EXECUTABLE)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitOne {
    pub repo: RepoRef,
    pub parent: String,
    pub path: String,
    pub mode: String,
    pub blob: String,
    pub message: String,
    pub author: GitIdentity,
    pub at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushCommit {
    pub repo: RepoRef,
    pub branch: String,
    pub commit: String,
    pub expected: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitPush {
    Pushed,
    Moved { remote_sha: Option<String> },
    Rejected,
}
