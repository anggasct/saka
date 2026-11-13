mod environments;
mod paths;
mod revert;
mod steps;

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
pub use paths::{read_file_in, resolve_in, write_file_in};
use saka_core::ids::{EnvironmentId, RepoId};
use saka_core::policy::RequestPath;
use saka_ports::{
    Blob, CommitFiles, CommitPush, EntryKind, EnvRef, GitAccess, PortError, PortResult, PushCommit,
    PushOutcome, PushRequest, RepoRef, Repos, Supervisor, TreeEntry, UnitLimits, WorktreeState,
};
use serde_json::json;
use steps::{AsyncStep, Fetch, FetchRef, Push, PushOne, Step};

use crate::git::{Extra, Git, GitOutput, auth_refused, failed};

const FETCH_REFSPEC: &str = "+refs/heads/*:refs/remotes/origin/*";
const REBASE_COMMITTER_NAME: &str = "Saka";
const REBASE_COMMITTER_EMAIL: &str = "saka@localhost";
const GIT_CONFIG_KEY_NOT_SET: i32 = 5;
const GIT_NOT_AN_ANCESTOR: i32 = 1;
const GIT_UNKNOWN_COMMIT: i32 = 128;
const BASE_FILE: &str = "base-sha";
const DOCS_DIR: &str = "docs";
const DOCS_FILE: &str = "docs-sha";
const RECIPE_PATH: &str = ".saka/environment.toml";
const RECIPE_MAX_BYTES: usize = 64 * 1024;
const REPO_FILE_MAX_BYTES: usize = 256 * 1024;

#[derive(Clone)]
pub struct ReposConfig {
    pub repos_dir: PathBuf,
    pub envs_dir: PathBuf,
    pub git: Git,
    pub recipe: RecipeRunner,
}

#[derive(Clone)]
pub struct RecipeRunner {
    pub supervisor: Arc<dyn Supervisor>,
    pub limits: UnitLimits,
    pub timeout: Duration,
    pub logs_dir: PathBuf,
    pub path: Vec<String>,
}

fn recipe_env(env: &EnvRef, path: &[String]) -> std::collections::BTreeMap<String, String> {
    let root = Path::new(&env.root_path);
    let mut vars = env.vars.clone();
    let full: Vec<&str> = env
        .path_prefix
        .iter()
        .chain(path)
        .map(String::as_str)
        .collect();
    vars.insert("PATH".into(), full.join(":"));
    vars.insert(
        "HOME".into(),
        root.join("home").to_string_lossy().into_owned(),
    );
    vars.insert(
        "TMPDIR".into(),
        root.join("tmp").to_string_lossy().into_owned(),
    );
    vars.insert("LANG".into(), "C.UTF-8".into());
    vars.insert("TERM".into(), "dumb".into());
    vars
}

fn write_log(path: &Path, text: &str, truncated: bool) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    if truncated {
        file.write_all(b"[output truncated: only the last 8 MiB is kept]\n")?;
    }
    file.write_all(text.as_bytes())
}

pub struct GitRepos {
    cfg: ReposConfig,
    access: Arc<dyn GitAccess>,
    locks: Mutex<HashMap<RepoId, Arc<tokio::sync::Mutex<()>>>>,
}

fn io(op: &str, e: &std::io::Error) -> PortError {
    PortError::retryable("env_io_failed", json!({ "op": op, "error": e.to_string() }))
}

fn make_dirs_writable(root: &Path) -> std::io::Result<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let meta = std::fs::symlink_metadata(&dir)?;
        if !meta.is_dir() {
            continue;
        }
        let mode = meta.permissions().mode();
        if mode & 0o700 != 0o700 {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode | 0o700))?;
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

// Symlinks are skipped: changing their mode would change the file they point to.
fn set_tree_modes(
    root: &Path,
    file_mode: impl Fn(u32) -> u32,
    dir_mode: impl Fn(u32) -> u32,
) -> std::io::Result<()> {
    let mut dirs = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let meta = std::fs::symlink_metadata(&dir)?;
        if !meta.is_dir() {
            continue;
        }
        let mode = meta.permissions().mode();
        if mode & 0o700 != 0o700 {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode | 0o700))?;
        }
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() && !(dir == root && entry.file_name() == ".git") {
                let mode = entry.metadata()?.permissions().mode();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(file_mode(mode)))?;
            }
        }
        dirs.push((dir, mode));
    }
    // Directories last and deepest first, so each one is still writable while it is walked.
    for (dir, mode) in dirs.into_iter().rev() {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(dir_mode(mode)))?;
    }
    Ok(())
}

fn project_path_arg(path: &str) -> PortResult<()> {
    let plain = path == "."
        || (!path.starts_with('-')
            && !path.contains(['\n', '\0', '\\'])
            && Path::new(path)
                .components()
                .all(|c| matches!(c, Component::Normal(_))));
    if plain && !path.is_empty() && !path.ends_with('/') {
        Ok(())
    } else {
        Err(PortError::permanent(
            "git_argument_invalid",
            json!({ "field": "project_path" }),
        ))
    }
}

fn code_name_arg(name: &str) -> PortResult<()> {
    let plain = !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    if plain {
        Ok(())
    } else {
        Err(PortError::permanent(
            "git_argument_invalid",
            json!({ "field": "code_name" }),
        ))
    }
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn sha_arg(value: &str, field: &str) -> PortResult<()> {
    if is_sha(value) {
        Ok(())
    } else {
        Err(PortError::permanent(
            "git_argument_invalid",
            json!({ "field": field }),
        ))
    }
}

const LITERAL_PATHS: [(&str, &str); 1] = [("GIT_LITERAL_PATHSPECS", "1")];

fn tree_entries(out: &GitOutput) -> PortResult<Vec<TreeEntry>> {
    let invalid = || PortError::retryable("git_output_invalid", json!({ "op": "ls_tree" }));
    let mut entries = Vec::new();
    for record in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(invalid)?;
        let meta = std::str::from_utf8(&record[..tab]).map_err(|_| invalid())?;
        let path = String::from_utf8(record[tab + 1..].to_vec()).map_err(|_| invalid())?;
        let mut fields = meta.split_whitespace();
        let (Some(mode), Some(kind), Some(object), Some(size)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(invalid());
        };
        let kind = match kind {
            "blob" => EntryKind::Blob,
            "tree" => EntryKind::Tree,
            "commit" => EntryKind::Commit,
            _ => return Err(invalid()),
        };
        entries.push(TreeEntry {
            path,
            mode: mode.to_string(),
            kind,
            object: object.to_string(),
            bytes: size.parse().ok(),
        });
    }
    Ok(entries)
}

fn arg(value: &str, field: &str) -> PortResult<()> {
    if value.is_empty() || value.starts_with('-') || value.contains('\0') {
        return Err(PortError::permanent(
            "git_argument_invalid",
            json!({ "field": field }),
        ));
    }
    Ok(())
}

impl GitRepos {
    pub fn new(cfg: ReposConfig, access: Arc<dyn GitAccess>) -> GitRepos {
        GitRepos {
            cfg,
            access,
            locks: Mutex::new(HashMap::new()),
        }
    }

    pub fn mirror_path(&self, repo: RepoId) -> PathBuf {
        self.cfg.repos_dir.join(format!("{repo}.git"))
    }

    async fn read_blob(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
    ) -> PortResult<Option<Vec<u8>>> {
        if !is_sha(commit) {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "commit" }),
            ));
        }
        let bare = self.mirror_path(repo.repo_id);
        let listed = self
            .git_ok(&bare, &["ls-tree", commit, "--", path], "ls_tree")
            .await?;
        if listed.stdout.is_empty() {
            return Ok(None);
        }
        let spec = format!("{commit}:{path}");
        let blob = self
            .git_ok(&bare, &["cat-file", "blob", &spec], "cat_file")
            .await?;
        Ok(Some(blob.stdout))
    }

    fn env_root(&self, id: impl std::fmt::Display) -> PathBuf {
        self.cfg.envs_dir.join(id.to_string())
    }

    fn lock(&self, repo: RepoId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.entry(repo).or_default().clone()
    }

    async fn git_ok(&self, dir: &Path, args: &[&str], op: &str) -> PortResult<GitOutput> {
        let out = self.cfg.git.local(dir, args).await?;
        if out.ok() {
            Ok(out)
        } else {
            Err(failed(op, &out))
        }
    }

    async fn ensure_mirror(&self, repo: &RepoRef, url: &str) -> PortResult<PathBuf> {
        let bare = self.mirror_path(repo.repo_id);
        if !bare.join("HEAD").is_file() {
            std::fs::create_dir_all(&self.cfg.repos_dir).map_err(|e| io("create_repos_dir", &e))?;
            let name = format!("{}.git", repo.repo_id);
            self.git_ok(
                &self.cfg.repos_dir,
                &["init", "--bare", "--quiet", &name],
                "init",
            )
            .await?;
        }
        self.git_ok(&bare, &["config", "remote.origin.url", url], "config")
            .await?;
        self.git_ok(
            &bare,
            &[
                "config",
                "--replace-all",
                "remote.origin.fetch",
                FETCH_REFSPEC,
            ],
            "config",
        )
        .await?;
        Ok(bare)
    }

    async fn with_remote<T, F>(&self, repo: &RepoRef, op: &str, mut step: F) -> PortResult<T>
    where
        F: AsyncStep<T>,
    {
        for attempt in 0..2 {
            let remote = self.access.remote(repo).await?;
            let bare = self.ensure_mirror(repo, &remote.url).await?;
            match step.run(self, &bare, &remote).await? {
                Step::Done(value) => return Ok(value),
                Step::Failed(out) if attempt == 0 && auth_refused(&out.stderr) => {
                    self.access.refused(repo).await;
                }
                Step::Failed(out) if auth_refused(&out.stderr) => {
                    return Err(PortError::permanent(
                        "git_auth_refused",
                        json!({ "op": op, "repo": repo.full_name }),
                    ));
                }
                Step::Failed(out) => return Err(failed(op, &out)),
            }
        }
        Err(PortError::permanent(
            "git_auth_refused",
            json!({ "op": op, "repo": repo.full_name }),
        ))
    }

    async fn fetch_locked(&self, repo: &RepoRef) -> PortResult<()> {
        self.with_remote(repo, "fetch", Fetch).await
    }

    async fn has_commit(&self, bare: &Path, sha: &str) -> PortResult<bool> {
        let object = format!("{sha}^{{commit}}");
        let out = self
            .cfg
            .git
            .local(bare, &["cat-file", "-e", &object])
            .await?;
        Ok(out.ok())
    }

    // A fork head is not under refs/heads, so the host's change ref is fetched by name.
    async fn ensure_change_head(
        &self,
        repo: &RepoRef,
        bare: &Path,
        head_sha: &str,
        head_ref: Option<&str>,
    ) -> PortResult<()> {
        sha_arg(head_sha, "head_sha")?;
        if self.has_commit(bare, head_sha).await? {
            return Ok(());
        }
        if let Some(head_ref) = head_ref {
            arg(head_ref, "head_ref")?;
            if !head_ref.starts_with("refs/") || head_ref.contains(['\n', ' ', ':', '\\']) {
                return Err(PortError::permanent(
                    "git_argument_invalid",
                    json!({ "field": "head_ref" }),
                ));
            }
            let refspec = format!("+{head_ref}:refs/saka/changes/{head_sha}");
            self.with_remote(repo, "fetch_change", FetchRef { refspec: &refspec })
                .await?;
        }
        if self.has_commit(bare, head_sha).await? {
            Ok(())
        } else {
            Err(PortError::permanent(
                "commit_missing",
                json!({ "commit": head_sha }),
            ))
        }
    }

    async fn tip_locked(&self, repo: &RepoRef, branch: &str) -> PortResult<Option<String>> {
        arg(branch, "branch")?;
        let bare = self.mirror_path(repo.repo_id);
        if !bare.join("HEAD").is_file() {
            return Ok(None);
        }
        let refname = format!("refs/remotes/origin/{branch}^{{commit}}");
        let out = self
            .cfg
            .git
            .local(&bare, &["rev-parse", "--verify", "--quiet", &refname])
            .await?;
        Ok(out.ok().then(|| out.text()).filter(|s| is_sha(s)))
    }

    async fn worktrees(&self, bare: &Path) -> PortResult<Vec<(PathBuf, Option<String>)>> {
        let out = self
            .git_ok(bare, &["worktree", "list", "--porcelain"], "worktree_list")
            .await?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut list = Vec::new();
        for block in text.split("\n\n") {
            let mut path = None;
            let mut branch = None;
            for line in block.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    path = Some(PathBuf::from(p));
                } else if let Some(b) = line.strip_prefix("branch ") {
                    branch = Some(b.to_string());
                }
            }
            if let Some(path) = path {
                list.push((path, branch));
            }
        }
        Ok(list)
    }

    async fn remove_worktree(&self, bare: &Path, path: &Path) -> PortResult<()> {
        let p = path.to_string_lossy();
        // Fails when git does not know the path; the directory is removed below anyway.
        self.cfg
            .git
            .local(bare, &["worktree", "remove", "--force", "--force", &p])
            .await?;
        if path.exists() {
            std::fs::remove_dir_all(path).map_err(|e| io("remove_worktree", &e))?;
        }
        self.git_ok(bare, &["worktree", "prune"], "worktree_prune")
            .await?;
        Ok(())
    }

    // The sparse patterns of one worktree need per-worktree config. Older git turns it on
    // without moving `core.bare`, which then makes every linked worktree bare; this moves it
    // first, in an order where the mirror stays usable at every step.
    async fn enable_worktree_config(&self, bare: &Path) -> PortResult<()> {
        let on = self
            .cfg
            .git
            .local(bare, &["config", "--bool", "extensions.worktreeConfig"])
            .await?;
        if on.ok() && on.text() == "true" {
            return Ok(());
        }
        let own = bare.join("config.worktree").to_string_lossy().into_owned();
        self.git_ok(
            bare,
            &["config", "--file", &own, "core.bare", "true"],
            "config",
        )
        .await?;
        let unset = self
            .cfg
            .git
            .local(bare, &["config", "--local", "--unset", "core.bare"])
            .await?;
        if !unset.ok() && unset.code != Some(GIT_CONFIG_KEY_NOT_SET) {
            return Err(failed("config", &unset));
        }
        self.git_ok(
            bare,
            &["config", "core.repositoryformatversion", "1"],
            "config",
        )
        .await?;
        self.git_ok(
            bare,
            &["config", "extensions.worktreeConfig", "true"],
            "config",
        )
        .await?;
        Ok(())
    }

    async fn worktree_state(
        &self,
        wt: &Path,
        environment_id: EnvironmentId,
        base_sha: &str,
    ) -> PortResult<WorktreeState> {
        if !is_sha(base_sha) {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "base_sha" }),
            ));
        }
        if !wt.join(".git").exists() {
            return Err(PortError::permanent(
                "worktree_missing",
                json!({ "environment_id": environment_id }),
            ));
        }
        let status = self
            .git_ok(
                wt,
                &["status", "--porcelain=v1", "--untracked-files=all"],
                "status",
            )
            .await?;
        let range = format!("{base_sha}..HEAD");
        let count = self
            .git_ok(wt, &["rev-list", "--count", &range], "rev_list")
            .await?;
        let head = self.git_ok(wt, &["rev-parse", "HEAD"], "rev_parse").await?;
        Ok(WorktreeState {
            dirty: !status.stdout.iter().all(u8::is_ascii_whitespace),
            commits_ahead: count.text().parse().unwrap_or(0),
            head_sha: head.text(),
        })
    }

    fn inside_envs(&self, path: &Path) -> bool {
        path.starts_with(&self.cfg.envs_dir) && path != self.cfg.envs_dir
    }
}

#[async_trait]
impl Repos for GitRepos {
    async fn fetch(&self, repo: &RepoRef) -> PortResult<()> {
        let lock = self.lock(repo.repo_id);
        let _guard = lock.lock().await;
        self.fetch_locked(repo).await
    }

    async fn tip(&self, repo: &RepoRef, branch: &str) -> PortResult<Option<String>> {
        let lock = self.lock(repo.repo_id);
        let _guard = lock.lock().await;
        self.tip_locked(repo, branch).await
    }

    async fn read_file(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
    ) -> PortResult<Option<Vec<u8>>> {
        if !is_sha(commit) {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "commit" }),
            ));
        }
        arg(path, "path")?;
        let bare = self.mirror_path(repo.repo_id);
        let object = format!("{commit}:{path}");
        let kind = self
            .cfg
            .git
            .local(&bare, &["cat-file", "-t", &object])
            .await?;
        if !kind.ok() || kind.text() != "blob" {
            return Ok(None);
        }
        let out = self
            .git_ok(&bare, &["cat-file", "blob", &object], "read_file")
            .await?;
        Ok(Some(out.stdout))
    }

    async fn read_bounded(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<Blob> {
        if !is_sha(commit) {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "commit" }),
            ));
        }
        arg(path, "path")?;
        let bare = self.mirror_path(repo.repo_id);
        let object = format!("{commit}:{path}");
        let kind = self
            .cfg
            .git
            .local(&bare, &["cat-file", "-t", &object])
            .await?;
        if !kind.ok() || kind.text() != "blob" {
            return Ok(Blob::Missing);
        }
        let size = self
            .git_ok(&bare, &["cat-file", "-s", &object], "read_file")
            .await?;
        let bytes: u64 = size.text().parse().map_err(|_| {
            PortError::retryable("git_output_invalid", json!({ "op": "read_file" }))
        })?;
        if bytes > max_bytes {
            return Ok(Blob::TooLarge { bytes });
        }
        let out = self
            .git_ok(&bare, &["cat-file", "blob", &object], "read_file")
            .await?;
        Ok(Blob::Bytes(out.stdout))
    }

    async fn grep_paths(
        &self,
        repo: &RepoRef,
        commit: &str,
        pattern: &str,
        pathspecs: &[String],
    ) -> PortResult<Vec<String>> {
        if !is_sha(commit) {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "commit" }),
            ));
        }
        arg(pattern, "pattern")?;
        let bare = self.mirror_path(repo.repo_id);
        let mut args = vec!["grep", "-l", "-I", "-E", "-e", pattern, commit, "--"];
        args.extend(pathspecs.iter().map(String::as_str));
        let out = self.cfg.git.local(&bare, &args).await?;
        if !out.ok() {
            if out.code == Some(1) && out.stderr.is_empty() {
                return Ok(Vec::new());
            }
            return Err(failed("grep", &out));
        }
        let prefix = format!("{commit}:");
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix(&prefix))
            .map(str::to_string)
            .collect())
    }

    async fn push(&self, request: &PushRequest) -> PortResult<PushOutcome> {
        arg(&request.branch, "branch")?;
        if !is_sha(&request.local_sha) {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "local_sha" }),
            ));
        }
        let lock = self.lock(request.repo.repo_id);
        let _guard = lock.lock().await;
        self.with_remote(&request.repo, "push", Push { request })
            .await
    }

    async fn list_tree(
        &self,
        repo: &RepoRef,
        commit: &str,
        dir: &str,
    ) -> PortResult<Vec<TreeEntry>> {
        sha_arg(commit, "commit")?;
        let bare = self.mirror_path(repo.repo_id);
        let mut args = vec!["ls-tree", "-r", "-l", "-z", "--full-tree", commit];
        match dir.trim_matches('/') {
            "" | "." => {}
            dir => {
                arg(dir, "dir")?;
                args.extend(["--", dir]);
            }
        }
        let extra = Extra {
            env: &LITERAL_PATHS,
            input: None,
        };
        let out = self.cfg.git.local_with(&bare, &args, extra).await?;
        if !out.ok() {
            return Err(failed("ls_tree", &out));
        }
        tree_entries(&out)
    }

    async fn entries(
        &self,
        repo: &RepoRef,
        commit: &str,
        paths: &[String],
    ) -> PortResult<Vec<TreeEntry>> {
        sha_arg(commit, "commit")?;
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        for path in paths {
            arg(path, "path")?;
        }
        let bare = self.mirror_path(repo.repo_id);
        let mut args = vec!["ls-tree", "-l", "-z", "--full-tree", commit, "--"];
        args.extend(paths.iter().map(String::as_str));
        let extra = Extra {
            env: &LITERAL_PATHS,
            input: None,
        };
        let out = self.cfg.git.local_with(&bare, &args, extra).await?;
        if !out.ok() {
            return Err(failed("ls_tree", &out));
        }
        tree_entries(&out)
    }

    async fn write_blob(&self, repo: &RepoRef, bytes: &[u8]) -> PortResult<String> {
        let bare = self.mirror_path(repo.repo_id);
        let extra = Extra {
            env: &[],
            input: Some(bytes),
        };
        let out = self
            .cfg
            .git
            .local_with(
                &bare,
                &["hash-object", "-w", "--no-filters", "--stdin"],
                extra,
            )
            .await?;
        if !out.ok() {
            return Err(failed("hash_object", &out));
        }
        let blob = out.text();
        sha_arg(&blob, "blob").map_err(|_| {
            PortError::retryable("git_output_invalid", json!({ "op": "hash_object" }))
        })?;
        Ok(blob)
    }

    async fn commit_files(&self, request: &CommitFiles) -> PortResult<String> {
        sha_arg(&request.parent, "parent")?;
        for file in &request.files {
            sha_arg(&file.blob, "blob")?;
            arg(&file.path, "path")?;
            if !matches!(
                file.mode.as_str(),
                TreeEntry::REGULAR | TreeEntry::EXECUTABLE
            ) {
                return Err(PortError::permanent(
                    "git_argument_invalid",
                    json!({ "field": "mode" }),
                ));
            }
        }
        let bare = self.mirror_path(request.repo.repo_id);
        let bare = bare.as_path();
        let scratch = tempfile::Builder::new()
            .prefix("index-")
            .tempdir_in(&self.cfg.repos_dir)
            .map_err(|e| io("index_dir", &e))?;
        let index = scratch.path().join("index");
        let index = index.to_string_lossy();
        let date = format!("@{} +0000", request.at.0.div_euclid(1000));
        let env = [
            ("GIT_INDEX_FILE", index.as_ref()),
            ("GIT_AUTHOR_NAME", request.author.name.as_str()),
            ("GIT_AUTHOR_EMAIL", request.author.email.as_str()),
            ("GIT_AUTHOR_DATE", date.as_str()),
            ("GIT_COMMITTER_NAME", request.author.name.as_str()),
            ("GIT_COMMITTER_EMAIL", request.author.email.as_str()),
            ("GIT_COMMITTER_DATE", date.as_str()),
        ];
        let git = &self.cfg.git;
        let extra = Extra {
            env: &env,
            input: None,
        };
        let run = |args: Vec<String>, op: &'static str| async move {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let out = git.local_with(bare, &args, extra).await?;
            if out.ok() {
                Ok(out.text())
            } else {
                Err(failed(op, &out))
            }
        };
        run(
            vec!["read-tree".into(), request.parent.clone()],
            "read_tree",
        )
        .await?;
        let mut update = vec!["update-index".to_string(), "--add".to_string()];
        for file in &request.files {
            update.push("--cacheinfo".into());
            update.push(format!("{},{},{}", file.mode, file.blob, file.path));
        }
        run(update, "update_index").await?;
        let tree = run(vec!["write-tree".into()], "write_tree").await?;
        let commit = run(
            vec![
                "commit-tree".into(),
                tree,
                "-p".into(),
                request.parent.clone(),
                "-m".into(),
                request.message.clone(),
            ],
            "commit_tree",
        )
        .await?;
        sha_arg(&commit, "commit").map_err(|_| {
            PortError::retryable("git_output_invalid", json!({ "op": "commit_tree" }))
        })?;
        Ok(commit)
    }

    async fn push_commit(&self, request: &PushCommit) -> PortResult<CommitPush> {
        arg(&request.branch, "branch")?;
        sha_arg(&request.commit, "commit")?;
        sha_arg(&request.expected, "expected")?;
        let lock = self.lock(request.repo.repo_id);
        let _guard = lock.lock().await;
        self.with_remote(&request.repo, "push", PushOne { request })
            .await
    }
}

pub fn resolve(worktree: &Path, raw: &str) -> RequestPath {
    let outside = || RequestPath {
        path: raw.to_string(),
        inside_worktree: false,
        through_symlink: false,
        in_reference: false,
    };
    let Ok(root) = worktree.canonicalize() else {
        return outside();
    };
    let start = Path::new(raw);
    let mut resolved = if start.is_absolute() {
        PathBuf::from("/")
    } else {
        root.clone()
    };
    let mut existing = true;
    let mut through_symlink = false;
    for part in start.components() {
        match part {
            Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                if existing {
                    match std::fs::symlink_metadata(&resolved) {
                        Ok(meta) => match resolved.canonicalize() {
                            Ok(real) => {
                                through_symlink |= meta.file_type().is_symlink();
                                resolved = real;
                            }
                            Err(_) => return outside(),
                        },
                        Err(_) => existing = false,
                    }
                }
            }
        }
    }
    match resolved.strip_prefix(&root) {
        Ok(rel) => RequestPath {
            path: rel.to_string_lossy().into_owned(),
            inside_worktree: true,
            through_symlink,
            in_reference: false,
        },
        Err(_) => outside(),
    }
}
