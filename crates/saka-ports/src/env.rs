use async_trait::async_trait;
use saka_core::ids::{EnvironmentId, WorkItemId};
use saka_core::policy::RequestPath;

use crate::PortResult;
use crate::scm::RepoRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareRequest {
    pub environment_id: EnvironmentId,
    pub work_item_id: WorkItemId,
    pub repo: RepoRef,
    pub branch: String,
    pub base_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEnvironment {
    pub root_path: String,
    pub worktree_path: String,
    pub base_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvRef {
    pub environment_id: EnvironmentId,
    pub repo: RepoRef,
    pub branch: String,
    pub root_path: String,
    pub worktree_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeState {
    pub dirty: bool,
    pub commits_ahead: u32,
    pub head_sha: String,
}

#[async_trait]
pub trait Environments: Send + Sync {
    async fn prepare(&self, request: &PrepareRequest) -> PortResult<PreparedEnvironment>;

    async fn inspect(&self, env: &EnvRef, base_sha: &str) -> PortResult<WorktreeState>;

    async fn is_merged(&self, repo: &RepoRef, commit: &str, base_branch: &str) -> PortResult<bool>;

    async fn teardown(&self, env: &EnvRef) -> PortResult<()>;

    fn resolve_path(&self, env: &EnvRef, raw: &str) -> RequestPath;
}
