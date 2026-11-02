use std::collections::BTreeMap;

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
    pub vars: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipePhase {
    Setup,
    Teardown,
}

impl RecipePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            RecipePhase::Setup => "setup",
            RecipePhase::Teardown => "teardown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeCommand {
    pub phase: RecipePhase,
    pub index: usize,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeCommandOutcome {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub log_path: String,
}

impl RecipeCommandOutcome {
    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
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

    async fn read_recipe(&self, repo: &RepoRef, commit: &str) -> PortResult<Option<String>>;

    async fn run_recipe_command(
        &self,
        env: &EnvRef,
        command: &RecipeCommand,
    ) -> PortResult<RecipeCommandOutcome>;

    fn resolve_path(&self, env: &EnvRef, raw: &str) -> RequestPath;
}
