use std::collections::BTreeMap;

use async_trait::async_trait;
use saka_core::authorship::GitIdentity;
use saka_core::ids::EnvironmentId;
use saka_core::policy::RequestPath;
use saka_core::secret::SecretVar;
use saka_core::vocab::Tool;

use crate::PortResult;
use crate::scm::RepoRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checkout {
    Branch(String),
    Detached,
    Change {
        head_sha: String,
        head_ref: Option<String>,
    },
}

impl Checkout {
    pub fn branch(&self) -> Option<&str> {
        match self {
            Checkout::Branch(branch) => Some(branch),
            Checkout::Detached | Checkout::Change { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeDiff {
    pub text: String,
    pub files: Vec<String>,
    pub truncated: bool,
    pub omitted_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareRequest {
    pub environment_id: EnvironmentId,
    pub repo: RepoRef,
    pub checkout: Checkout,
    pub base_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEnvironment {
    pub root_path: String,
    pub worktree_path: String,
    pub base_sha: String,
}

pub const DOCS_MAX_FILES: u64 = 20_000;
pub const DOCS_MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsRequest {
    pub environment_id: EnvironmentId,
    pub repo: RepoRef,
    pub commit: String,
    pub project_path: String,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedDocs {
    pub worktree_path: String,
    pub path: String,
    pub base_sha: String,
}

pub const CODE_DIR: &str = "code";
pub const CODE_MAX_FILES: u64 = 50_000;
pub const CODE_MAX_BYTES: u64 = 1024 * 1024 * 1024;

#[must_use]
pub fn code_path(root_path: &str, name: &str) -> String {
    format!("{}/{CODE_DIR}/{name}", root_path.trim_end_matches('/'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRequest {
    pub environment_id: EnvironmentId,
    pub repo: RepoRef,
    pub name: String,
    pub base_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedCode {
    pub path: String,
    pub base_sha: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeCheckout {
    Ready(PreparedCode),
    TooLarge {
        base_sha: String,
        files: u64,
        bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvRef {
    pub environment_id: EnvironmentId,
    pub repo: RepoRef,
    pub branch: Option<String>,
    pub root_path: String,
    pub worktree_path: String,
    pub vars: BTreeMap<String, String>,
    pub path_prefix: Vec<String>,
    pub secrets: Vec<SecretVar>,
    pub extra_dirs: Vec<ExtraDir>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraDir {
    pub path: String,
    pub writable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipePhase {
    Setup,
    Teardown,
    Gate,
    Hook,
}

impl RecipePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            RecipePhase::Setup => "setup",
            RecipePhase::Teardown => "teardown",
            RecipePhase::Gate => "gate",
            RecipePhase::Hook => "hook",
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebase {
    Current {
        head_sha: String,
        base_sha: String,
    },
    Rebased {
        head_sha: String,
        base_sha: String,
    },
    Conflict {
        base_sha: String,
        paths: Vec<String>,
    },
    Unclean,
}

pub const REVERT_PATHS_MAX: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertRequest {
    pub base_sha: String,
    pub merge_commit: String,
    pub head_sha: Option<String>,
    pub change_number: u64,
    pub message: String,
    pub author: GitIdentity,
    pub committer: GitIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reverted {
    Committed { head_sha: String },
    NotInBase,
    CommitsUnknown,
    Conflict { paths: Vec<String>, count: usize },
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseProbe {
    pub base_sha: String,
    pub in_head: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedPaths {
    pub paths: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeFile {
    Missing,
    NotRegular,
    TooLarge { bytes: u64 },
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeChange {
    pub path: String,
    pub added: bool,
    pub diff: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub lts: Option<String>,
}

#[async_trait]
pub trait ReleaseIndex: Send + Sync {
    async fn releases(&self, tool: Tool) -> PortResult<Vec<Release>>;
}

#[async_trait]
pub trait Environments: Send + Sync {
    async fn prepare(&self, request: &PrepareRequest) -> PortResult<PreparedEnvironment>;

    async fn prepare_docs(&self, request: &DocsRequest) -> PortResult<PreparedDocs>;

    async fn teardown_docs(&self, environment_id: EnvironmentId, repo: &RepoRef) -> PortResult<()>;

    async fn prepare_code(&self, request: &CodeRequest) -> PortResult<CodeCheckout>;

    async fn teardown_code(
        &self,
        environment_id: EnvironmentId,
        repo: &RepoRef,
        name: &str,
    ) -> PortResult<()>;

    async fn inspect(&self, env: &EnvRef, base_sha: &str) -> PortResult<WorktreeState>;

    async fn inspect_docs(
        &self,
        environment_id: EnvironmentId,
        base_sha: &str,
    ) -> PortResult<WorktreeState>;

    async fn contains_commit(
        &self,
        repo: &RepoRef,
        commit: &str,
        base_branch: &str,
    ) -> PortResult<Option<String>>;

    async fn base_probe(
        &self,
        repo: &RepoRef,
        base_branch: &str,
        head_sha: &str,
    ) -> PortResult<Option<BaseProbe>>;

    async fn commit_parents(&self, repo: &RepoRef, commit: &str)
    -> PortResult<Option<Vec<String>>>;

    async fn changed_paths(
        &self,
        repo: &RepoRef,
        base: &str,
        head_sha: &str,
        fetch: bool,
    ) -> PortResult<ChangedPaths>;

    async fn is_ancestor(&self, repo: &RepoRef, ancestor: &str, head_sha: &str)
    -> PortResult<bool>;

    async fn carries_patch(&self, repo: &RepoRef, commit: &str, head_sha: &str)
    -> PortResult<bool>;

    async fn change_diff(
        &self,
        repo: &RepoRef,
        base_branch: &str,
        head_sha: &str,
        max_bytes: usize,
    ) -> PortResult<ChangeDiff>;

    async fn check_out(&self, env: &EnvRef, commit: &str) -> PortResult<()>;

    async fn rebase(&self, env: &EnvRef, base_branch: &str, head_sha: &str) -> PortResult<Rebase>;

    async fn reset_branch(&self, env: &EnvRef, to_sha: &str) -> PortResult<()>;

    async fn revert(&self, env: &EnvRef, request: &RevertRequest) -> PortResult<Reverted>;

    async fn teardown(&self, env: &EnvRef) -> PortResult<()>;

    async fn read_recipe(&self, repo: &RepoRef, commit: &str) -> PortResult<Option<String>>;

    async fn branch_tip(&self, repo: &RepoRef, branch: &str) -> PortResult<Option<String>>;

    async fn list_repo_dir(
        &self,
        repo: &RepoRef,
        commit: &str,
        dir: &str,
    ) -> PortResult<Vec<String>>;

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

    async fn read_worktree_file(
        &self,
        env: &EnvRef,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<WorktreeFile>;

    async fn write_worktree_file(&self, env: &EnvRef, path: &str, bytes: &[u8]) -> PortResult<()>;

    async fn worktree_changes(
        &self,
        env: &EnvRef,
        max_diff_bytes: usize,
    ) -> PortResult<Vec<WorktreeChange>>;

    fn resolve_path(&self, env: &EnvRef, cwd: &str, raw: &str) -> RequestPath;
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
