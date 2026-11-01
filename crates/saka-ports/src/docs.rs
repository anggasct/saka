//! The workspace docs source: files read byte-exact at the tip of a branch.

use async_trait::async_trait;

use crate::PortResult;
use crate::scm::RepoRef;

/// A file as read at a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocFile {
    pub path: String,
    pub commit: String,
    pub bytes: Vec<u8>,
}

#[async_trait]
pub trait DocSource: Send + Sync {
    /// Fetches, then reads `path` at the tip of `origin/<branch>`. `Ok(None)` when the
    /// branch exists but the file does not; a missing branch is the error
    /// `base_branch_missing`.
    async fn read_at_tip(
        &self,
        repo: &RepoRef,
        branch: &str,
        path: &str,
    ) -> PortResult<Option<DocFile>>;
}
