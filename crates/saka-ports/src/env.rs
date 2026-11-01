//! Task environments: a worktree per task, made from the latest base branch.

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
    /// The task branch to create in the worktree.
    pub branch: String,
    /// The worktree starts at `origin/<base_branch>` as fetched now.
    pub base_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEnvironment {
    pub root_path: String,
    pub worktree_path: String,
    pub base_sha: String,
}

/// An environment that was prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvRef {
    pub environment_id: EnvironmentId,
    pub repo: RepoRef,
    pub branch: String,
    pub root_path: String,
    pub worktree_path: String,
}

/// What the worktree holds after a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeState {
    /// Uncommitted or untracked changes exist.
    pub dirty: bool,
    pub commits_ahead: u32,
    pub head_sha: String,
}

#[async_trait]
pub trait Environments: Send + Sync {
    /// Fetches the repo and creates the worktree. Calling it again for the same
    /// environment id returns the environment that already exists.
    async fn prepare(&self, request: &PrepareRequest) -> PortResult<PreparedEnvironment>;

    /// Reads the worktree state relative to `base_sha`.
    async fn inspect(&self, env: &EnvRef, base_sha: &str) -> PortResult<WorktreeState>;

    /// Fetches and checks that `commit` is an ancestor of `origin/<base_branch>`.
    async fn is_merged(&self, repo: &RepoRef, commit: &str, base_branch: &str) -> PortResult<bool>;

    /// Removes the worktree and its local branch. Succeeds when they are already gone.
    async fn teardown(&self, env: &EnvRef) -> PortResult<()>;

    /// Canonicalizes a path a tool wants to touch (including `..` and symlinks) against
    /// the worktree.
    fn resolve_path(&self, env: &EnvRef, raw: &str) -> RequestPath;
}
