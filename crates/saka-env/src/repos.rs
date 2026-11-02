use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use saka_core::ids::RepoId;
use saka_core::policy::RequestPath;
use saka_core::redact::Redactor;
use saka_core::vocab::SecretPurpose;
use saka_ports::{
    Blob, CommandUnit, EnvRef, Environments, GitAccess, GitRemote, PortError, PortResult,
    PrepareRequest, PreparedEnvironment, PushOutcome, PushRequest, RecipeCommand,
    RecipeCommandOutcome, RepoRef, Repos, Supervisor, UnitLimits, WorktreeState,
};
use serde_json::json;

use crate::git::{Git, GitOutput, auth_refused, failed};

const FETCH_REFSPEC: &str = "+refs/heads/*:refs/remotes/origin/*";
const BASE_FILE: &str = "base-sha";
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

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
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

    fn inside_envs(&self, path: &Path) -> bool {
        path.starts_with(&self.cfg.envs_dir) && path != self.cfg.envs_dir
    }
}

enum Step<T> {
    Done(T),
    Failed(GitOutput),
}

#[async_trait]
trait AsyncStep<T>: Send {
    async fn run(
        &mut self,
        repos: &GitRepos,
        bare: &Path,
        remote: &GitRemote,
    ) -> PortResult<Step<T>>;
}

struct Fetch;

#[async_trait]
impl AsyncStep<()> for Fetch {
    async fn run(
        &mut self,
        repos: &GitRepos,
        bare: &Path,
        remote: &GitRemote,
    ) -> PortResult<Step<()>> {
        let out = repos
            .cfg
            .git
            .remote(bare, &["fetch", "--prune", "--quiet", "origin"], remote)
            .await?;
        Ok(if out.ok() {
            Step::Done(())
        } else {
            Step::Failed(out)
        })
    }
}

struct Push<'a> {
    request: &'a PushRequest,
}

fn remote_sha(out: &GitOutput, refname: &str) -> Option<String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            let (sha, name) = line.split_once('\t')?;
            (name == refname && is_sha(sha)).then(|| sha.to_string())
        })
}

#[async_trait]
impl AsyncStep<PushOutcome> for Push<'_> {
    async fn run(
        &mut self,
        repos: &GitRepos,
        bare: &Path,
        remote: &GitRemote,
    ) -> PortResult<Step<PushOutcome>> {
        let req = self.request;
        let refname = format!("refs/heads/{}", req.branch);
        let git = &repos.cfg.git;
        let ls = git
            .remote(bare, &["ls-remote", "origin", &refname], remote)
            .await?;
        if !ls.ok() {
            return Ok(Step::Failed(ls));
        }
        let current = remote_sha(&ls, &refname);
        match &current {
            Some(sha) if *sha == req.local_sha => {
                return Ok(Step::Done(PushOutcome::Pushed { sha: sha.clone() }));
            }
            Some(sha) if Some(sha) != req.expected_remote_sha.as_ref() => {
                return Ok(Step::Done(PushOutcome::Rejected {
                    remote_sha: sha.clone(),
                }));
            }
            _ => {}
        }
        // An empty lease means "the branch must not exist yet".
        let lease = format!(
            "--force-with-lease={refname}:{}",
            current.clone().unwrap_or_default()
        );
        // The local branch must still be where the worktree was inspected.
        let local = git
            .local(bare, &["rev-parse", "--verify", "--quiet", &refname])
            .await?;
        if !local.ok() || local.text() != req.local_sha {
            return Err(PortError::permanent(
                "branch_moved_locally",
                json!({ "branch": req.branch, "expected": req.local_sha, "found": local.text() }),
            ));
        }
        let spec = format!("{0}:{0}", req.branch);
        let out = git
            .remote(bare, &["push", "--quiet", &lease, "origin", &spec], remote)
            .await?;
        if out.ok() {
            return Ok(Step::Done(PushOutcome::Pushed {
                sha: req.local_sha.clone(),
            }));
        }
        if auth_refused(&out.stderr) {
            return Ok(Step::Failed(out));
        }
        // The lease did not hold: someone else moved the branch in between.
        let again = git
            .remote(bare, &["ls-remote", "origin", &refname], remote)
            .await?;
        match remote_sha(&again, &refname) {
            Some(sha) if sha == req.local_sha => Ok(Step::Done(PushOutcome::Pushed { sha })),
            Some(sha) if Some(&sha) != current.as_ref() => {
                Ok(Step::Done(PushOutcome::Rejected { remote_sha: sha }))
            }
            _ => Err(failed("push", &out)),
        }
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
}

#[async_trait]
impl Environments for GitRepos {
    async fn prepare(&self, request: &PrepareRequest) -> PortResult<PreparedEnvironment> {
        arg(&request.branch, "branch")?;
        arg(&request.base_branch, "base_branch")?;
        let lock = self.lock(request.repo.repo_id);
        let _guard = lock.lock().await;
        let root = self.env_root(request.environment_id);
        let worktree = root.join("worktree");
        let base_file = root.join(BASE_FILE);
        let prepared = |base_sha: String| PreparedEnvironment {
            root_path: root.to_string_lossy().into_owned(),
            worktree_path: worktree.to_string_lossy().into_owned(),
            base_sha,
        };
        if let Ok(base) = std::fs::read_to_string(&base_file)
            && is_sha(base.trim())
            && worktree.join(".git").exists()
        {
            return Ok(prepared(base.trim().to_string()));
        }
        self.fetch_locked(&request.repo).await?;
        let base = self
            .tip_locked(&request.repo, &request.base_branch)
            .await?
            .ok_or_else(|| {
                PortError::permanent(
                    "base_branch_missing",
                    json!({ "base_branch": request.base_branch }),
                )
            })?;
        let bare = self.mirror_path(request.repo.repo_id);
        let branch_ref = format!("refs/heads/{}", request.branch);
        for (path, branch) in self.worktrees(&bare).await? {
            let stale = path == worktree || branch.as_deref() == Some(branch_ref.as_str());
            if stale && self.inside_envs(&path) {
                self.remove_worktree(&bare, &path).await?;
            }
        }
        if worktree.exists() {
            std::fs::remove_dir_all(&worktree).map_err(|e| io("remove_worktree", &e))?;
        }
        std::fs::create_dir_all(&root).map_err(|e| io("create_env_root", &e))?;
        let wt = worktree.to_string_lossy();
        self.git_ok(
            &bare,
            &[
                "worktree",
                "add",
                "--quiet",
                "--no-track",
                "-B",
                &request.branch,
                &wt,
                &base,
            ],
            "worktree_add",
        )
        .await?;
        // Written last: an environment without it is unfinished and is made again.
        let tmp = root.join(format!("{BASE_FILE}.tmp"));
        std::fs::write(&tmp, format!("{base}\n")).map_err(|e| io("write_base", &e))?;
        std::fs::rename(&tmp, &base_file).map_err(|e| io("write_base", &e))?;
        Ok(prepared(base))
    }

    async fn inspect(&self, env: &EnvRef, base_sha: &str) -> PortResult<WorktreeState> {
        if !is_sha(base_sha) {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "base_sha" }),
            ));
        }
        let wt = PathBuf::from(&env.worktree_path);
        if !wt.join(".git").exists() {
            return Err(PortError::permanent(
                "worktree_missing",
                json!({ "environment_id": env.environment_id }),
            ));
        }
        let status = self
            .git_ok(
                &wt,
                &["status", "--porcelain=v1", "--untracked-files=all"],
                "status",
            )
            .await?;
        let range = format!("{base_sha}..HEAD");
        let count = self
            .git_ok(&wt, &["rev-list", "--count", &range], "rev_list")
            .await?;
        let head = self
            .git_ok(&wt, &["rev-parse", "HEAD"], "rev_parse")
            .await?;
        Ok(WorktreeState {
            dirty: !status.stdout.iter().all(u8::is_ascii_whitespace),
            commits_ahead: count.text().parse().unwrap_or(0),
            head_sha: head.text(),
        })
    }

    async fn is_merged(&self, repo: &RepoRef, commit: &str, base_branch: &str) -> PortResult<bool> {
        if !is_sha(commit) {
            return Ok(false);
        }
        arg(base_branch, "base_branch")?;
        let lock = self.lock(repo.repo_id);
        let _guard = lock.lock().await;
        self.fetch_locked(repo).await?;
        let bare = self.mirror_path(repo.repo_id);
        let base = format!("refs/remotes/origin/{base_branch}");
        let out = self
            .cfg
            .git
            .local(&bare, &["merge-base", "--is-ancestor", commit, &base])
            .await?;
        match out.code {
            Some(0) => Ok(true),
            // 1: not an ancestor; 128: the commit is not in the mirror at all.
            Some(1 | 128) => Ok(false),
            _ => Err(failed("merge_base", &out)),
        }
    }

    async fn teardown(&self, env: &EnvRef) -> PortResult<()> {
        arg(&env.branch, "branch")?;
        let lock = self.lock(env.repo.repo_id);
        let _guard = lock.lock().await;
        let root = self.env_root(env.environment_id);
        let bare = self.mirror_path(env.repo.repo_id);
        if root.exists() && self.inside_envs(&root) {
            make_dirs_writable(&root).map_err(|e| io("make_writable", &e))?;
        }
        if bare.join("HEAD").is_file() {
            self.remove_worktree(&bare, &root.join("worktree")).await?;
            let branch_ref = format!("refs/heads/{}", env.branch);
            let still_used = self
                .worktrees(&bare)
                .await?
                .iter()
                .any(|(_, b)| b.as_deref() == Some(branch_ref.as_str()));
            if !still_used {
                // Fails when the branch is already gone.
                self.cfg
                    .git
                    .local(&bare, &["branch", "-D", "--quiet", &env.branch])
                    .await?;
            }
        }
        if root.exists() && self.inside_envs(&root) {
            std::fs::remove_dir_all(&root).map_err(|e| io("remove_env_root", &e))?;
        }
        Ok(())
    }

    async fn read_recipe(&self, repo: &RepoRef, commit: &str) -> PortResult<Option<String>> {
        let Some(blob) = self.read_blob(repo, commit, RECIPE_PATH).await? else {
            return Ok(None);
        };
        if blob.len() > RECIPE_MAX_BYTES {
            return Err(PortError::permanent(
                "env_recipe_too_large",
                json!({ "bytes": blob.len(), "max": RECIPE_MAX_BYTES }),
            ));
        }
        String::from_utf8(blob).map(Some).map_err(|_| {
            PortError::permanent("env_recipe_not_utf8", json!({ "path": RECIPE_PATH }))
        })
    }

    async fn read_repo_file(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
    ) -> PortResult<Option<String>> {
        let relative = Path::new(path);
        let plain = !path.is_empty()
            && !path.starts_with('-')
            && !path.contains(['\n', ':'])
            && relative
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
        if !plain {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "path" }),
            ));
        }
        let Some(blob) = self.read_blob(repo, commit, path).await? else {
            return Ok(None);
        };
        if blob.len() > REPO_FILE_MAX_BYTES {
            return Err(PortError::permanent(
                "repo_file_too_large",
                json!({ "path": path, "bytes": blob.len(), "max": REPO_FILE_MAX_BYTES }),
            ));
        }
        String::from_utf8(blob)
            .map(Some)
            .map_err(|_| PortError::permanent("repo_file_not_utf8", json!({ "path": path })))
    }

    async fn run_recipe_command(
        &self,
        env: &EnvRef,
        command: &RecipeCommand,
    ) -> PortResult<RecipeCommandOutcome> {
        let runner = &self.cfg.recipe;
        let root = self.env_root(env.environment_id);
        if !self.inside_envs(&root) || !Path::new(&env.worktree_path).is_dir() {
            return Err(PortError::permanent("worktree_missing", json!({})));
        }
        for dir in ["home", "tmp"] {
            std::fs::create_dir_all(root.join(dir)).map_err(|e| io("create_env_dir", &e))?;
        }
        let label = command.label();
        let unit = CommandUnit {
            name: format!("saka-env-{}-{label}", env.environment_id),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), command.command.clone()],
            cwd: env.worktree_path.clone(),
            env: recipe_env(env, &runner.path),
            secret_env: env
                .secrets
                .iter()
                .filter(|s| s.serves(SecretPurpose::Setup))
                .map(|s| (s.name.clone(), s.value.clone()))
                .collect(),
            limits: runner.limits.clone(),
            timeout: command
                .timeout_ms
                .map_or(runner.timeout, std::time::Duration::from_millis),
        };
        let started = Instant::now();
        let outcome = runner.supervisor.run(&unit).await?;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let text = String::from_utf8_lossy(&outcome.output);
        let named: Vec<(String, String)> = env
            .secrets
            .iter()
            .map(|s| (s.name.clone(), s.value.expose().to_string()))
            .collect();
        let redacted = Redactor::named(&[], &named).text(&text).into_owned();
        let log = runner
            .logs_dir
            .join(env.environment_id.to_string())
            .join(format!("{label}.log"));
        write_log(&log, &redacted, outcome.truncated).map_err(|e| io("write_recipe_log", &e))?;
        Ok(RecipeCommandOutcome {
            exit_code: outcome.exit_code,
            timed_out: outcome.timed_out,
            duration_ms,
            log_path: log.to_string_lossy().into_owned(),
            output_tail: saka_ports::output_tail(&redacted),
        })
    }

    fn resolve_path(&self, env: &EnvRef, raw: &str) -> RequestPath {
        resolve(Path::new(&env.worktree_path), raw)
    }
}

pub fn resolve(worktree: &Path, raw: &str) -> RequestPath {
    let outside = || RequestPath {
        path: raw.to_string(),
        inside_worktree: false,
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
                        Ok(_) => match resolved.canonicalize() {
                            Ok(real) => resolved = real,
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
        },
        Err(_) => outside(),
    }
}
