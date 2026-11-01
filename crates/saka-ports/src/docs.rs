use async_trait::async_trait;

use crate::PortResult;
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
}
