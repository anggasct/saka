use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use saka_core::ids::{EnvironmentId, RepoId};
use saka_core::policy::RequestPath;
use saka_core::redact::Redactor;
use saka_core::vocab::SecretPurpose;
use saka_ports::BestEffort;
use saka_ports::{
    Blob, ChangeDiff, ChangedPaths, Checkout, CommandUnit, CommitFiles, CommitPush, DOCS_MAX_BYTES,
    DOCS_MAX_FILES, DocsRequest, EntryKind, EnvRef, Environments, GitAccess, GitRemote, PortError,
    PortResult, PrepareRequest, PreparedDocs, PreparedEnvironment, PushCommit, PushOutcome,
    PushRequest, Rebase, RecipeCommand, RecipeCommandOutcome, RepoRef, Repos, Supervisor,
    TreeEntry, UnitLimits, WorktreeChange, WorktreeFile, WorktreeState,
};
use serde_json::json;

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

struct FetchRef<'a> {
    refspec: &'a str,
}

#[async_trait]
impl AsyncStep<()> for FetchRef<'_> {
    async fn run(
        &mut self,
        repos: &GitRepos,
        bare: &Path,
        remote: &GitRemote,
    ) -> PortResult<Step<()>> {
        let out = repos
            .cfg
            .git
            .remote(bare, &["fetch", "--quiet", "origin", self.refspec], remote)
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

struct PushOne<'a> {
    request: &'a PushCommit,
}

#[async_trait]
impl AsyncStep<CommitPush> for PushOne<'_> {
    async fn run(
        &mut self,
        repos: &GitRepos,
        bare: &Path,
        remote: &GitRemote,
    ) -> PortResult<Step<CommitPush>> {
        let req = self.request;
        let refname = format!("refs/heads/{}", req.branch);
        let git = &repos.cfg.git;
        // The lease turns the update into compare-and-swap on the remote: it lands only while the
        // branch is still at `expected`, and `commit` is a child of it, so it is a fast-forward.
        let lease = format!("--force-with-lease={refname}:{}", req.expected);
        let spec = format!("{}:{refname}", req.commit);
        let out = git
            .remote(
                bare,
                &["push", "--porcelain", "--quiet", &lease, "origin", &spec],
                remote,
            )
            .await?;
        // Porcelain marks a refused ref with a leading `!` status line. A failure without one never
        // reached a decision on the remote, for example when the network is down.
        let refused = out
            .stdout
            .split(|b| *b == b'\n')
            .any(|line| line.starts_with(b"!\t"));
        if !out.ok() && !refused {
            return Ok(Step::Failed(out));
        }
        let pushed = if out.ok() {
            true
        } else {
            let ls = git
                .remote(bare, &["ls-remote", "origin", &refname], remote)
                .await?;
            if !ls.ok() {
                return Ok(Step::Failed(ls));
            }
            match remote_sha(&ls, &refname) {
                Some(sha) if sha == req.commit => true,
                Some(sha) if sha == req.expected => return Ok(Step::Done(CommitPush::Rejected)),
                remote_sha => return Ok(Step::Done(CommitPush::Moved { remote_sha })),
            }
        };
        if pushed {
            let tracking = format!("refs/remotes/origin/{}", req.branch);
            // A fetch may already have moved the tracking ref; the next fetch corrects it anyway.
            git.local(bare, &["update-ref", &tracking, &req.commit, &req.expected])
                .await?;
        }
        Ok(Step::Done(CommitPush::Pushed))
    }
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

#[async_trait]
impl Environments for GitRepos {
    async fn prepare(&self, request: &PrepareRequest) -> PortResult<PreparedEnvironment> {
        if let Some(branch) = request.checkout.branch() {
            arg(branch, "branch")?;
        }
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
        let bare = self.mirror_path(request.repo.repo_id);
        let base = match &request.checkout {
            Checkout::Change { head_sha, head_ref } => {
                self.ensure_change_head(&request.repo, &bare, head_sha, head_ref.as_deref())
                    .await?;
                head_sha.clone()
            }
            _ => self
                .tip_locked(&request.repo, &request.base_branch)
                .await?
                .ok_or_else(|| {
                    PortError::permanent(
                        "base_branch_missing",
                        json!({ "base_branch": request.base_branch }),
                    )
                })?,
        };
        let branch_ref = request.checkout.branch().map(|b| format!("refs/heads/{b}"));
        for (path, branch) in self.worktrees(&bare).await? {
            let stale = path == worktree || (branch.is_some() && branch == branch_ref);
            if stale && self.inside_envs(&path) {
                self.remove_worktree(&bare, &path).await?;
            }
        }
        if worktree.exists() {
            std::fs::remove_dir_all(&worktree).map_err(|e| io("remove_worktree", &e))?;
        }
        std::fs::create_dir_all(&root).map_err(|e| io("create_env_root", &e))?;
        let wt = worktree.to_string_lossy();
        let add: Vec<&str> = match &request.checkout {
            Checkout::Branch(branch) => vec![
                "worktree",
                "add",
                "--quiet",
                "--no-track",
                "-B",
                branch,
                &wt,
                &base,
            ],
            Checkout::Detached | Checkout::Change { .. } => {
                vec!["worktree", "add", "--quiet", "--detach", &wt, &base]
            }
        };
        self.git_ok(&bare, &add, "worktree_add").await?;
        // Written last: an environment without it is unfinished and is made again.
        let tmp = root.join(format!("{BASE_FILE}.tmp"));
        std::fs::write(&tmp, format!("{base}\n")).map_err(|e| io("write_base", &e))?;
        std::fs::rename(&tmp, &base_file).map_err(|e| io("write_base", &e))?;
        Ok(prepared(base))
    }

    async fn prepare_docs(&self, request: &DocsRequest) -> PortResult<PreparedDocs> {
        sha_arg(&request.commit, "commit")?;
        project_path_arg(&request.project_path)?;
        if let Some(branch) = &request.branch {
            arg(branch, "branch")?;
        }
        let lock = self.lock(request.repo.repo_id);
        let _guard = lock.lock().await;
        let root = self.env_root(request.environment_id);
        let docs = root.join(DOCS_DIR);
        let marker = root.join(DOCS_FILE);
        let path = if request.project_path == "." {
            docs.clone()
        } else {
            docs.join(&request.project_path)
        };
        let prepared = PreparedDocs {
            worktree_path: docs.to_string_lossy().into_owned(),
            path: path.to_string_lossy().into_owned(),
            base_sha: request.commit.clone(),
        };
        if let Ok(done) = std::fs::read_to_string(&marker)
            && done.trim() == request.commit
            && docs.join(".git").exists()
        {
            return Ok(prepared);
        }
        let bare = self.mirror_path(request.repo.repo_id);
        let object = format!("{}^{{commit}}", request.commit);
        let present = async || -> PortResult<bool> {
            if !bare.join("HEAD").is_file() {
                return Ok(false);
            }
            let out = self
                .cfg
                .git
                .local(&bare, &["cat-file", "-e", &object])
                .await?;
            Ok(out.ok())
        };
        if !present().await? {
            self.fetch_locked(&request.repo).await?;
            if !present().await? {
                return Err(PortError::permanent(
                    "docs_commit_missing",
                    json!({ "commit": request.commit }),
                ));
            }
        }
        let mut list = vec!["ls-tree", "-r", "-l", "-z", request.commit.as_str()];
        if request.project_path != "." {
            list.extend(["--", request.project_path.as_str()]);
        }
        let listed = self
            .cfg
            .git
            .local_with(
                &bare,
                &list,
                Extra {
                    env: &LITERAL_PATHS,
                    input: None,
                },
            )
            .await?;
        if !listed.ok() {
            return Err(failed("ls_tree", &listed));
        }
        let (files, bytes) = tree_entries(&listed)?
            .iter()
            .filter(|e| e.kind == EntryKind::Blob)
            .fold((0u64, 0u64), |(n, b), e| (n + 1, b + e.bytes.unwrap_or(0)));
        if files > DOCS_MAX_FILES || bytes > DOCS_MAX_BYTES {
            return Err(PortError::permanent(
                "docs_checkout_too_large",
                json!({
                    "files": files,
                    "bytes": bytes,
                    "max_files": DOCS_MAX_FILES,
                    "max_bytes": DOCS_MAX_BYTES,
                }),
            ));
        }
        std::fs::remove_file(&marker).best_effort("remove the docs marker");
        if docs.exists() && self.inside_envs(&docs) {
            make_dirs_writable(&docs).map_err(|e| io("make_writable", &e))?;
        }
        let branch_ref = request.branch.as_ref().map(|b| format!("refs/heads/{b}"));
        for (path, held) in self.worktrees(&bare).await? {
            let stale = path == docs || (held.is_some() && held == branch_ref);
            if stale && self.inside_envs(&path) {
                if path.exists() {
                    make_dirs_writable(&path).map_err(|e| io("make_writable", &e))?;
                }
                self.remove_worktree(&bare, &path).await?;
            }
        }
        self.remove_worktree(&bare, &docs).await?;
        std::fs::create_dir_all(&root).map_err(|e| io("create_env_root", &e))?;
        let wt = docs.to_string_lossy();
        let add: Vec<&str> = match &request.branch {
            Some(branch) => vec![
                "worktree",
                "add",
                "--quiet",
                "--no-checkout",
                "--no-track",
                "-B",
                branch,
                &wt,
                &request.commit,
            ],
            None => vec![
                "worktree",
                "add",
                "--quiet",
                "--detach",
                "--no-checkout",
                &wt,
                &request.commit,
            ],
        };
        self.git_ok(&bare, &add, "worktree_add").await?;
        if request.project_path != "." {
            self.enable_worktree_config(&bare).await?;
            self.git_ok(
                &docs,
                &["sparse-checkout", "init", "--cone"],
                "sparse_checkout",
            )
            .await?;
            let cone = format!("{}\n", request.project_path);
            let out = self
                .cfg
                .git
                .local_with(
                    &docs,
                    &["sparse-checkout", "set", "--stdin"],
                    Extra {
                        env: &[],
                        input: Some(cone.as_bytes()),
                    },
                )
                .await?;
            if !out.ok() {
                return Err(failed("sparse_checkout", &out));
            }
        }
        match &request.branch {
            Some(branch) => {
                self.git_ok(&docs, &["checkout", "--quiet", branch], "checkout")
                    .await?;
            }
            None => {
                self.git_ok(
                    &docs,
                    &["checkout", "--quiet", "--detach", &request.commit],
                    "checkout",
                )
                .await?;
                set_tree_modes(&docs, |_| 0o444, |_| 0o555)
                    .map_err(|e| io("make_read_only", &e))?;
            }
        }
        // Written last: a checkout without it is unfinished and is made again.
        let tmp = root.join(format!("{DOCS_FILE}.tmp"));
        std::fs::write(&tmp, format!("{}\n", request.commit)).map_err(|e| io("write_docs", &e))?;
        std::fs::rename(&tmp, &marker).map_err(|e| io("write_docs", &e))?;
        Ok(prepared)
    }

    async fn teardown_docs(&self, environment_id: EnvironmentId, repo: &RepoRef) -> PortResult<()> {
        let lock = self.lock(repo.repo_id);
        let _guard = lock.lock().await;
        let root = self.env_root(environment_id);
        let docs = root.join(DOCS_DIR);
        if docs.exists() && self.inside_envs(&docs) {
            set_tree_modes(&docs, |m| m | 0o200, |m| m | 0o700)
                .map_err(|e| io("make_writable", &e))?;
        }
        let bare = self.mirror_path(repo.repo_id);
        if bare.join("HEAD").is_file() {
            let held = self
                .worktrees(&bare)
                .await?
                .into_iter()
                .find(|(p, _)| *p == docs)
                .and_then(|(_, b)| b);
            self.remove_worktree(&bare, &docs).await?;
            if let Some(branch) = held.as_deref().and_then(|b| b.strip_prefix("refs/heads/")) {
                // Fails when the branch is already gone.
                self.cfg
                    .git
                    .local(&bare, &["branch", "-D", "--quiet", branch])
                    .await?;
            }
        } else if docs.exists() && self.inside_envs(&docs) {
            std::fs::remove_dir_all(&docs).map_err(|e| io("remove_docs", &e))?;
        }
        match std::fs::remove_file(root.join(DOCS_FILE)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io("remove_docs", &e)),
            _ => Ok(()),
        }
    }

    async fn inspect_docs(
        &self,
        environment_id: EnvironmentId,
        base_sha: &str,
    ) -> PortResult<WorktreeState> {
        let docs = self.env_root(environment_id).join(DOCS_DIR);
        self.worktree_state(&docs, environment_id, base_sha).await
    }

    async fn inspect(&self, env: &EnvRef, base_sha: &str) -> PortResult<WorktreeState> {
        self.worktree_state(Path::new(&env.worktree_path), env.environment_id, base_sha)
            .await
    }

    async fn contains_commit(
        &self,
        repo: &RepoRef,
        commit: &str,
        base_branch: &str,
    ) -> PortResult<Option<String>> {
        if !is_sha(commit) {
            return Ok(None);
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
            Some(0) => self.tip_locked(repo, base_branch).await,
            Some(GIT_NOT_AN_ANCESTOR | GIT_UNKNOWN_COMMIT) => Ok(None),
            _ => Err(failed("merge_base", &out)),
        }
    }

    async fn changed_paths(
        &self,
        repo: &RepoRef,
        base: &str,
        head_sha: &str,
        fetch: bool,
    ) -> PortResult<ChangedPaths> {
        sha_arg(head_sha, "head_sha")?;
        arg(base, "base")?;
        let lock = self.lock(repo.repo_id);
        let _guard = lock.lock().await;
        if fetch {
            self.fetch_locked(repo).await?;
        }
        let bare = self.mirror_path(repo.repo_id);
        let object = format!("{head_sha}^{{commit}}");
        let present = self
            .cfg
            .git
            .local(&bare, &["cat-file", "-e", &object])
            .await?;
        if !present.ok() {
            return Err(PortError::permanent(
                "commit_missing",
                json!({ "commit": head_sha }),
            ));
        }
        let merge_base = self
            .git_ok(&bare, &["merge-base", base, head_sha], "merge_base")
            .await?
            .text();
        let diff = self
            .git_ok(
                &bare,
                &[
                    "diff",
                    "--name-only",
                    "-z",
                    "--no-renames",
                    &merge_base,
                    head_sha,
                ],
                "diff",
            )
            .await?;
        let mut paths: Vec<String> = diff
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        let truncated = paths.len() > saka_core::scope::MAX_CHANGED_PATHS;
        paths.truncate(saka_core::scope::MAX_CHANGED_PATHS);
        Ok(ChangedPaths { paths, truncated })
    }

    async fn change_diff(
        &self,
        repo: &RepoRef,
        base_branch: &str,
        head_sha: &str,
        max_bytes: usize,
    ) -> PortResult<ChangeDiff> {
        sha_arg(head_sha, "head_sha")?;
        arg(base_branch, "base_branch")?;
        let lock = self.lock(repo.repo_id);
        let _guard = lock.lock().await;
        let bare = self.mirror_path(repo.repo_id);
        if !self.has_commit(&bare, head_sha).await? {
            return Err(PortError::permanent(
                "commit_missing",
                json!({ "commit": head_sha }),
            ));
        }
        let base = format!("refs/remotes/origin/{base_branch}");
        let merge_base = self
            .git_ok(&bare, &["merge-base", &base, head_sha], "merge_base")
            .await?
            .text();
        let names = self
            .git_ok(
                &bare,
                &[
                    "diff",
                    "--name-only",
                    "-z",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-renames",
                    &merge_base,
                    head_sha,
                ],
                "diff",
            )
            .await?;
        let files: Vec<String> = names
            .stdout
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .take(saka_core::scope::MAX_CHANGED_PATHS)
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        let out = self
            .cfg
            .git
            .local_capped(
                &bare,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-color",
                    "--no-renames",
                    "-U3",
                    &merge_base,
                    head_sha,
                ],
                max_bytes,
            )
            .await?;
        if !out.ok() {
            return Err(failed("diff", &out));
        }
        let truncated = out.stdout.len() > max_bytes;
        let mut text =
            String::from_utf8_lossy(&out.stdout[..out.stdout.len().min(max_bytes)]).into_owned();
        let mut omitted_files = 0;
        if truncated {
            if let Some(end) = text.rfind('\n') {
                text.truncate(end + 1);
            }
            let shown = text
                .lines()
                .filter(|l| l.starts_with("diff --git "))
                .count();
            omitted_files = files.len().saturating_sub(shown);
        }
        Ok(ChangeDiff {
            text,
            files,
            truncated,
            omitted_files,
        })
    }

    async fn check_out(&self, env: &EnvRef, commit: &str) -> PortResult<()> {
        if !is_sha(commit) {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "commit" }),
            ));
        }
        let wt = PathBuf::from(&env.worktree_path);
        if !wt.join(".git").exists() {
            return Err(PortError::permanent(
                "worktree_missing",
                json!({ "environment_id": env.environment_id }),
            ));
        }
        let lock = self.lock(env.repo.repo_id);
        let _guard = lock.lock().await;
        let bare = self.mirror_path(env.repo.repo_id);
        let object = format!("{commit}^{{commit}}");
        let present = |out: &GitOutput| out.code == Some(0);
        let mut found = present(
            &self
                .cfg
                .git
                .local(&bare, &["cat-file", "-e", &object])
                .await?,
        );
        if !found {
            self.fetch_locked(&env.repo).await?;
            found = present(
                &self
                    .cfg
                    .git
                    .local(&bare, &["cat-file", "-e", &object])
                    .await?,
            );
        }
        if !found {
            return Err(PortError::permanent(
                "commit_missing",
                json!({ "commit": commit }),
            ));
        }
        self.git_ok(
            &wt,
            &["checkout", "--detach", "--force", commit],
            "checkout",
        )
        .await?;
        self.git_ok(&wt, &["clean", "-fd"], "clean").await?;
        Ok(())
    }

    async fn rebase(&self, env: &EnvRef, base_branch: &str, head_sha: &str) -> PortResult<Rebase> {
        sha_arg(head_sha, "head_sha")?;
        arg(base_branch, "base_branch")?;
        let branch = env.branch.as_deref().ok_or_else(|| {
            PortError::permanent(
                "branch_missing",
                json!({ "environment_id": env.environment_id }),
            )
        })?;
        arg(branch, "branch")?;
        let wt = PathBuf::from(&env.worktree_path);
        if !wt.join(".git").exists() {
            return Err(PortError::permanent(
                "worktree_missing",
                json!({ "environment_id": env.environment_id }),
            ));
        }
        let lock = self.lock(env.repo.repo_id);
        let _guard = lock.lock().await;
        self.fetch_locked(&env.repo).await?;
        let bare = self.mirror_path(env.repo.repo_id);
        if !self.has_commit(&bare, head_sha).await? {
            return Err(PortError::permanent(
                "commit_missing",
                json!({ "commit": head_sha }),
            ));
        }
        let base_ref = format!("refs/remotes/origin/{base_branch}^{{commit}}");
        let base = self
            .git_ok(&wt, &["rev-parse", "--verify", &base_ref], "rev_parse")
            .await
            .map_err(|_| {
                PortError::permanent("base_branch_missing", json!({ "branch": base_branch }))
            })?
            .text();
        let status = self
            .git_ok(
                &wt,
                &["status", "--porcelain=v1", "--untracked-files=all"],
                "status",
            )
            .await?;
        let branch_ref = format!("refs/heads/{branch}");
        let local = self
            .cfg
            .git
            .local(&wt, &["rev-parse", "--verify", "--quiet", &branch_ref])
            .await?;
        let ours_only = if local.ok() {
            let (not_head, not_base) = (format!("^{head_sha}"), format!("^{base}"));
            let committers = self
                .git_ok(
                    &wt,
                    &["log", "--format=%ce", &local.text(), &not_head, &not_base],
                    "log",
                )
                .await?;
            committers
                .text()
                .lines()
                .all(|email| email == REBASE_COMMITTER_EMAIL)
        } else {
            true
        };
        if !status.stdout.iter().all(u8::is_ascii_whitespace) || !ours_only {
            return Ok(Rebase::Unclean);
        }
        self.git_ok(
            &wt,
            &["checkout", "--quiet", "-B", branch, head_sha],
            "checkout",
        )
        .await?;
        let up_to_date = self
            .cfg
            .git
            .local(&wt, &["merge-base", "--is-ancestor", &base, head_sha])
            .await?;
        if up_to_date.ok() {
            return Ok(Rebase::Current {
                head_sha: head_sha.to_string(),
                base_sha: base,
            });
        }
        let stamp = self
            .git_ok(&wt, &["show", "-s", "--format=%ct", head_sha], "show")
            .await?
            .text();
        let date = format!("{stamp} +0000");
        let committer = [
            ("GIT_COMMITTER_NAME", REBASE_COMMITTER_NAME),
            ("GIT_COMMITTER_EMAIL", REBASE_COMMITTER_EMAIL),
            ("GIT_COMMITTER_DATE", date.as_str()),
        ];
        let out = self
            .cfg
            .git
            .local_with(
                &wt,
                &["-c", "core.hooksPath=/dev/null", "rebase", &base],
                Extra {
                    env: &committer,
                    input: None,
                },
            )
            .await?;
        if out.ok() {
            let head = self
                .git_ok(&wt, &["rev-parse", "HEAD"], "rev_parse")
                .await?;
            return Ok(Rebase::Rebased {
                head_sha: head.text(),
                base_sha: base,
            });
        }
        let unmerged = self
            .git_ok(&wt, &["diff", "--name-only", "--diff-filter=U"], "diff")
            .await?;
        let paths: Vec<String> = String::from_utf8_lossy(&unmerged.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        self.git_ok(&wt, &["rebase", "--abort"], "rebase_abort")
            .await?;
        if paths.is_empty() {
            return Err(failed("rebase", &out));
        }
        Ok(Rebase::Conflict {
            base_sha: base,
            paths,
        })
    }

    async fn teardown(&self, env: &EnvRef) -> PortResult<()> {
        if let Some(branch) = &env.branch {
            arg(branch, "branch")?;
        }
        let lock = self.lock(env.repo.repo_id);
        let _guard = lock.lock().await;
        let root = self.env_root(env.environment_id);
        let bare = self.mirror_path(env.repo.repo_id);
        if root.exists() && self.inside_envs(&root) {
            make_dirs_writable(&root).map_err(|e| io("make_writable", &e))?;
        }
        if bare.join("HEAD").is_file() {
            self.remove_worktree(&bare, &root.join("worktree")).await?;
            if let Some(branch) = &env.branch {
                let branch_ref = format!("refs/heads/{branch}");
                let still_used = self
                    .worktrees(&bare)
                    .await?
                    .iter()
                    .any(|(_, b)| b.as_deref() == Some(branch_ref.as_str()));
                if !still_used {
                    // Fails when the branch is already gone.
                    self.cfg
                        .git
                        .local(&bare, &["branch", "-D", "--quiet", branch])
                        .await?;
                }
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

    async fn branch_tip(&self, repo: &RepoRef, branch: &str) -> PortResult<Option<String>> {
        arg(branch, "branch")?;
        let lock = self.lock(repo.repo_id);
        let _guard = lock.lock().await;
        self.fetch_locked(repo).await?;
        self.tip_locked(repo, branch).await
    }

    async fn list_repo_dir(
        &self,
        repo: &RepoRef,
        commit: &str,
        dir: &str,
    ) -> PortResult<Vec<String>> {
        sha_arg(commit, "commit")?;
        let bare = self.mirror_path(repo.repo_id);
        let mut args = vec!["ls-tree", "-l", "-z", "--full-tree", commit];
        let prefix;
        if !dir.is_empty() {
            project_path_arg(dir).map_err(|_| {
                PortError::permanent("git_argument_invalid", json!({ "field": "dir" }))
            })?;
            prefix = format!("{dir}/");
            args.extend(["--", &prefix]);
        }
        let out = self.cfg.git.local(&bare, &args).await?;
        if !out.ok() {
            return Err(failed("ls_tree", &out));
        }
        Ok(tree_entries(&out)?
            .into_iter()
            .filter(TreeEntry::is_regular_file)
            .map(|e| e.path)
            .collect())
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

    async fn read_worktree_file(
        &self,
        env: &EnvRef,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<WorktreeFile> {
        read_file_in(Path::new(&env.worktree_path), path, max_bytes)
    }

    async fn write_worktree_file(&self, env: &EnvRef, path: &str, bytes: &[u8]) -> PortResult<()> {
        write_file_in(Path::new(&env.worktree_path), path, bytes)
    }

    async fn worktree_changes(
        &self,
        env: &EnvRef,
        max_diff_bytes: usize,
    ) -> PortResult<Vec<WorktreeChange>> {
        let wt = Path::new(&env.worktree_path);
        let status = self
            .git_ok(
                wt,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
                "status",
            )
            .await?;
        let mut changes = Vec::new();
        for entry in status.stdout.split(|b| *b == 0).filter(|e| e.len() > 3) {
            let code = String::from_utf8_lossy(&entry[..2]).into_owned();
            let path = String::from_utf8_lossy(&entry[3..]).into_owned();
            if code.contains('D') {
                continue;
            }
            arg(&path, "path")?;
            let added = code == "??" || code.contains('A');
            let diff = if added {
                self.cfg
                    .git
                    .local(
                        wt,
                        &["diff", "--no-index", "--no-color", "--", "/dev/null", &path],
                    )
                    .await?
            } else {
                self.cfg
                    .git
                    .local(wt, &["diff", "--no-color", "HEAD", "--", &path])
                    .await?
            };
            if !matches!(diff.code, Some(0 | 1)) {
                return Err(failed("diff", &diff));
            }
            let text = String::from_utf8_lossy(&diff.stdout).into_owned();
            let truncated = text.len() > max_diff_bytes;
            let mut end = text.len().min(max_diff_bytes);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            changes.push(WorktreeChange {
                path,
                added,
                diff: text[..end].to_string(),
                truncated,
            });
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(changes)
    }

    fn resolve_path(&self, env: &EnvRef, cwd: &str, raw: &str) -> RequestPath {
        let resolved = resolve_in(Path::new(&env.worktree_path), cwd, raw);
        if resolved.inside_worktree {
            return resolved;
        }
        env.extra_dirs
            .iter()
            .filter(|d| d.writable)
            .map(|d| resolve_in(Path::new(&d.path), cwd, raw))
            .find(|r| r.inside_worktree)
            .unwrap_or(resolved)
    }
}

pub fn resolve_in(worktree: &Path, cwd: &str, raw: &str) -> RequestPath {
    if cwd.is_empty() || Path::new(raw).is_absolute() {
        return resolve(worktree, raw);
    }
    let resolved = resolve(worktree, &Path::new(cwd).join(raw).to_string_lossy());
    if resolved.inside_worktree {
        resolved
    } else {
        RequestPath {
            path: raw.to_string(),
            inside_worktree: false,
            through_symlink: resolved.through_symlink,
        }
    }
}

fn relative_parts(path: &str) -> PortResult<Vec<&std::ffi::OsStr>> {
    let invalid = || PortError::permanent("worktree_path_invalid", json!({ "path": path }));
    let parts = Path::new(path)
        .components()
        .map(|c| match c {
            Component::Normal(name) => Ok(name),
            _ => Err(invalid()),
        })
        .collect::<PortResult<Vec<_>>>()?;
    if parts.is_empty() {
        return Err(invalid());
    }
    Ok(parts)
}

fn open_no_follow(path: &Path, flags: rustix::fs::OFlags) -> std::io::Result<std::fs::File> {
    let flags = flags | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC;
    let fd = rustix::fs::open(path, flags, rustix::fs::Mode::from_raw_mode(0o644))?;
    Ok(std::fs::File::from(fd))
}

pub fn read_file_in(worktree: &Path, path: &str, max_bytes: u64) -> PortResult<WorktreeFile> {
    use std::io::Read;

    let parts = relative_parts(path)?;
    let mut current = worktree.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        current.push(part);
        let meta = match std::fs::symlink_metadata(&current) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(WorktreeFile::Missing),
            Err(e) => return Err(io("read_worktree_file", &e)),
        };
        if meta.file_type().is_symlink() {
            return Ok(WorktreeFile::NotRegular);
        }
        if i + 1 < parts.len() {
            if !meta.is_dir() {
                return Ok(WorktreeFile::Missing);
            }
            continue;
        }
        if !meta.is_file() {
            return Ok(WorktreeFile::NotRegular);
        }
        if meta.len() > max_bytes {
            return Ok(WorktreeFile::TooLarge { bytes: meta.len() });
        }
    }
    let file = match open_no_follow(&current, rustix::fs::OFlags::RDONLY) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(WorktreeFile::Missing),
        Err(e) if e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) => {
            return Ok(WorktreeFile::NotRegular);
        }
        Err(e) => return Err(io("read_worktree_file", &e)),
    };
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| io("read_worktree_file", &e))?;
    let read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if read > max_bytes {
        return Ok(WorktreeFile::TooLarge { bytes: read });
    }
    Ok(WorktreeFile::Bytes(bytes))
}

pub fn write_file_in(worktree: &Path, path: &str, bytes: &[u8]) -> PortResult<()> {
    use std::io::Write;

    let parts = relative_parts(path)?;
    let conflict = |at: &Path| {
        PortError::permanent(
            "worktree_path_conflict",
            json!({ "path": path, "at": at.strip_prefix(worktree).unwrap_or(at).to_string_lossy() }),
        )
    };
    let mut current = worktree.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        current.push(part);
        let last = i + 1 == parts.len();
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(conflict(&current)),
            Ok(meta) if last && !meta.is_file() => return Err(conflict(&current)),
            Ok(meta) if !last && !meta.is_dir() => return Err(conflict(&current)),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !last => {
                std::fs::create_dir(&current).map_err(|e| io("write_worktree_file", &e))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io("write_worktree_file", &e)),
        }
    }
    let dir = current.parent().unwrap_or(worktree);
    let name = current
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(".{}.saka-tmp", name.unwrap_or_default()));
    std::fs::remove_file(&tmp).best_effort("remove a temp file left by an earlier write");
    let write = || -> std::io::Result<()> {
        let flags =
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CREATE | rustix::fs::OFlags::EXCL;
        let mut file = open_no_follow(&tmp, flags)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &current)
    };
    write().map_err(|e| {
        std::fs::remove_file(&tmp).best_effort("remove the temp file of a failed write");
        io("write_worktree_file", &e)
    })
}

pub fn resolve(worktree: &Path, raw: &str) -> RequestPath {
    let outside = || RequestPath {
        path: raw.to_string(),
        inside_worktree: false,
        through_symlink: false,
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
        },
        Err(_) => outside(),
    }
}
