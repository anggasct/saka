//! Bare mirrors and task worktrees.
//!
//! - Mirror: `repos_dir/<repo_id>.git`, remote `origin` without credentials, branches
//!   fetched into `refs/remotes/origin/*`.
//! - Environment: `envs_dir/<environment_id>/` holds `worktree/` (on the task branch,
//!   made from `origin/<base>`), the session's `home/` and `tmp/`, and `base-sha`.
//!
//! Git work on one repo runs one command sequence at a time.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use saka_core::ids::RepoId;
use saka_core::policy::RequestPath;
use saka_ports::{
    EnvRef, Environments, GitAccess, GitRemote, PortError, PortResult, PrepareRequest,
    PreparedEnvironment, PushOutcome, PushRequest, RepoRef, Repos, WorktreeState,
};
use serde_json::json;

use crate::git::{Git, GitOutput, auth_refused, failed};

const FETCH_REFSPEC: &str = "+refs/heads/*:refs/remotes/origin/*";
const BASE_FILE: &str = "base-sha";

#[derive(Debug, Clone)]
pub struct ReposConfig {
    /// `data_dir/repos`.
    pub repos_dir: PathBuf,
    /// `data_dir/envs`.
    pub envs_dir: PathBuf,
    pub git: Git,
}

pub struct GitRepos {
    cfg: ReposConfig,
    access: Arc<dyn GitAccess>,
    locks: Mutex<HashMap<RepoId, Arc<tokio::sync::Mutex<()>>>>,
}

fn io(op: &str, e: &std::io::Error) -> PortError {
    PortError::retryable("env_io_failed", json!({ "op": op, "error": e.to_string() }))
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A branch, path, or commit given to git must not read as an option.
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

    /// Creates the mirror if needed and points `origin` at `url`.
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

    /// Runs a remote command, getting new credentials once if the remote refuses them.
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

    /// Worktrees of this mirror: `(path, branch ref)`.
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

/// One attempt of a remote command.
enum Step<T> {
    Done(T),
    /// Git ran and failed; the output decides whether to retry with new credentials.
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
    /// The engine prepares an environment only while the task has no ready one, so a
    /// worktree of an earlier, unfinished environment that still holds the task branch is
    /// removed first.
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

    fn resolve_path(&self, env: &EnvRef, raw: &str) -> RequestPath {
        resolve(Path::new(&env.worktree_path), raw)
    }
}

/// Canonicalizes `raw` (absolute, or relative to the worktree) the way the file system
/// would follow it, including `..` after a symlink. Parts that do not exist yet are taken
/// as written. A dangling symlink counts as outside.
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
