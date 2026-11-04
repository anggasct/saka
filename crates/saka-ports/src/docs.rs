use async_trait::async_trait;
use saka_core::time::Timestamp;

use crate::PortResult;
use crate::git::Blob;
use crate::scm::RepoRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocFile {
    pub path: String,
    pub commit: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocEntry {
    pub path: String,
    pub blob: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocTree {
    pub commit: String,
    pub files: Vec<DocEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocBlob {
    Missing,
    NotRegular,
    TooLarge { blob: String, bytes: u64 },
    Bytes { blob: String, bytes: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocOpened {
    pub commit: String,
    pub blob: DocBlob,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitIdentity {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocExpect {
    Absent,
    Exists,
    Blob(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocCommit {
    pub repo: RepoRef,
    pub branch: String,
    pub path: String,
    pub bytes: Vec<u8>,
    pub expect: DocExpect,
    pub author: CommitIdentity,
    pub at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocWrite {
    Committed {
        commit: String,
        parent: String,
        blob: String,
        created: bool,
        attempts: u32,
    },
    Unchanged {
        commit: String,
        blob: String,
    },
    Stale {
        commit: String,
        blob: Option<String>,
    },
    PathConflict {
        path: String,
    },
    NotRegular,
    Rejected,
}

#[async_trait]
pub trait DocSource: Send + Sync {
    async fn branch_tip(&self, repo: &RepoRef, branch: &str) -> PortResult<Option<String>>;

    async fn read_at_tip(
        &self,
        repo: &RepoRef,
        branch: &str,
        path: &str,
    ) -> PortResult<Option<DocFile>>;

    async fn read_at(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<Blob>;

    async fn find_always(
        &self,
        repo: &RepoRef,
        commit: &str,
        project_path: &str,
    ) -> PortResult<Vec<String>>;

    async fn tree(
        &self,
        repo: &RepoRef,
        branch: &str,
        project_path: &str,
        max_files: usize,
    ) -> PortResult<DocTree>;

    async fn tree_at(
        &self,
        repo: &RepoRef,
        commit: &str,
        project_path: &str,
    ) -> PortResult<Vec<DocEntry>>;

    async fn open(
        &self,
        repo: &RepoRef,
        branch: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<DocOpened>;

    async fn commit_file(&self, request: &DocCommit) -> PortResult<DocWrite>;
}
