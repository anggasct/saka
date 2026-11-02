use async_trait::async_trait;

use crate::PortResult;
use crate::git::Blob;
use crate::scm::RepoRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocFile {
    pub path: String,
    pub commit: String,
    pub bytes: Vec<u8>,
}

#[async_trait]
pub trait DocSource: Send + Sync {
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
}
