use std::collections::BTreeMap;

use async_trait::async_trait;
use saka_core::ids::{EnvironmentId, WorkItemId};
use saka_core::policy::RequestPath;
use saka_core::secret::SecretVar;

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
    pub path_prefix: Vec<String>,
    pub secrets: Vec<SecretVar>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipePhase {
    Setup,
    Teardown,
    Gate,
}

impl RecipePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            RecipePhase::Setup => "setup",
            RecipePhase::Teardown => "teardown",
            RecipePhase::Gate => "gate",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeCommand {
    pub phase: RecipePhase,
    pub index: usize,
    pub command: String,
    pub run: Option<String>,
    pub timeout_ms: Option<u64>,
}

impl RecipeCommand {
    pub fn label(&self) -> String {
        match &self.run {
            Some(run) => format!("{}-{run}-{}", self.phase.as_str(), self.index),
            None => format!("{}-{}", self.phase.as_str(), self.index),
        }
    }
}

pub const OUTPUT_TAIL_LINES: usize = 60;
pub const OUTPUT_TAIL_BYTES: usize = 8 * 1024;

pub fn output_tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(OUTPUT_TAIL_LINES);
    let mut tail = lines[start..].join("\n");
    if tail.len() > OUTPUT_TAIL_BYTES {
        let mut cut = tail.len() - OUTPUT_TAIL_BYTES;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail = tail[cut..].to_string();
    }
    tail
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeCommandOutcome {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub log_path: String,
    pub output_tail: String,
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

    async fn contains_commit(
        &self,
        repo: &RepoRef,
        commit: &str,
        base_branch: &str,
    ) -> PortResult<Option<String>>;

    async fn check_out(&self, env: &EnvRef, commit: &str) -> PortResult<()>;

    async fn teardown(&self, env: &EnvRef) -> PortResult<()>;

    async fn read_recipe(&self, repo: &RepoRef, commit: &str) -> PortResult<Option<String>>;

    async fn read_repo_file(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
    ) -> PortResult<Option<String>>;

    async fn run_recipe_command(
        &self,
        env: &EnvRef,
        command: &RecipeCommand,
    ) -> PortResult<RecipeCommandOutcome>;

    fn resolve_path(&self, env: &EnvRef, raw: &str) -> RequestPath;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_output_tail_keeps_the_last_lines_within_the_byte_cap() {
        let many: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let tail = output_tail(&many);
        assert_eq!(tail.lines().count(), OUTPUT_TAIL_LINES);
        assert!(tail.starts_with("line 40"));
        let wide = format!("{}\n", "é".repeat(OUTPUT_TAIL_BYTES));
        let tail = output_tail(&wide);
        assert!(tail.len() <= OUTPUT_TAIL_BYTES);
        assert!(tail.chars().all(|c| c == 'é'));
    }
}
