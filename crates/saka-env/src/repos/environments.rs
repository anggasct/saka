use std::path::{Path, PathBuf};
use std::time::Instant;

use async_trait::async_trait;
use saka_core::ids::EnvironmentId;
use saka_core::policy::RequestPath;
use saka_core::redact::Redactor;
use saka_core::vocab::SecretPurpose;
use saka_ports::{
    BaseProbe, BestEffort, ChangeDiff, ChangedPaths, Checkout, CommandUnit, DOCS_MAX_BYTES,
    DOCS_MAX_FILES, DocsRequest, EntryKind, EnvRef, Environments, PortError, PortResult,
    PrepareRequest, PreparedDocs, PreparedEnvironment, Rebase, RecipeCommand, RecipeCommandOutcome,
    RepoRef, TreeEntry, WorktreeChange, WorktreeFile, WorktreeState,
};
use serde_json::json;

use super::paths::{read_file_in, resolve_in, write_file_in};
use super::{
    BASE_FILE, DOCS_DIR, DOCS_FILE, GIT_NOT_AN_ANCESTOR, GIT_UNKNOWN_COMMIT, GitRepos,
    LITERAL_PATHS, REBASE_COMMITTER_EMAIL, REBASE_COMMITTER_NAME, RECIPE_MAX_BYTES, RECIPE_PATH,
    REPO_FILE_MAX_BYTES, arg, io, is_sha, make_dirs_writable, project_path_arg, recipe_env,
    set_tree_modes, sha_arg, tree_entries, write_log,
};
use crate::git::{Extra, GitOutput, failed};

const PATCH_SCAN_COMMITS: u32 = 200;
const PATCH_SCAN_BYTES: usize = 32 * 1024 * 1024;

impl GitRepos {
    async fn patch_ids(&self, bare: &Path, args: &[&str]) -> PortResult<Vec<(String, String)>> {
        let log = self
            .cfg
            .git
            .local_capped(bare, args, PATCH_SCAN_BYTES)
            .await?;
        if !log.ok() {
            return Err(failed("log", &log));
        }
        let extra = Extra {
            env: &[],
            input: Some(&log.stdout),
        };
        let ids = self
            .cfg
            .git
            .local_with(bare, &["patch-id", "--stable"], extra)
            .await?;
        if !ids.ok() {
            return Err(failed("patch_id", &ids));
        }
        Ok(ids
            .text()
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(id, commit)| (id.to_string(), commit.to_string()))
            .collect())
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

    async fn base_probe(
        &self,
        repo: &RepoRef,
        base_branch: &str,
        head_sha: &str,
    ) -> PortResult<Option<BaseProbe>> {
        sha_arg(head_sha, "head_sha")?;
        arg(base_branch, "base_branch")?;
        let lock = self.lock(repo.repo_id);
        let _guard = lock.lock().await;
        self.fetch_locked(repo).await?;
        let Some(base_sha) = self.tip_locked(repo, base_branch).await? else {
            return Ok(None);
        };
        let bare = self.mirror_path(repo.repo_id);
        let out = self
            .cfg
            .git
            .local(&bare, &["merge-base", "--is-ancestor", &base_sha, head_sha])
            .await?;
        match out.code {
            Some(0) => Ok(Some(BaseProbe {
                base_sha,
                in_head: true,
            })),
            Some(GIT_NOT_AN_ANCESTOR) => Ok(Some(BaseProbe {
                base_sha,
                in_head: false,
            })),
            Some(GIT_UNKNOWN_COMMIT) => Err(PortError::permanent(
                "commit_missing",
                json!({ "commit": head_sha }),
            )),
            _ => Err(failed("merge_base", &out)),
        }
    }

    async fn commit_parents(
        &self,
        repo: &RepoRef,
        commit: &str,
    ) -> PortResult<Option<Vec<String>>> {
        sha_arg(commit, "commit")?;
        let lock = self.lock(repo.repo_id);
        let _guard = lock.lock().await;
        self.fetch_locked(repo).await?;
        let bare = self.mirror_path(repo.repo_id);
        let object = format!("{commit}^{{commit}}");
        let present = self
            .cfg
            .git
            .local(&bare, &["cat-file", "-e", &object])
            .await?;
        if !present.ok() {
            return Ok(None);
        }
        let out = self
            .git_ok(
                &bare,
                &["rev-list", "--parents", "-n", "1", commit],
                "rev_list",
            )
            .await?
            .text();
        Ok(Some(
            out.split_whitespace().skip(1).map(String::from).collect(),
        ))
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

    async fn is_ancestor(
        &self,
        repo: &RepoRef,
        ancestor: &str,
        head_sha: &str,
    ) -> PortResult<bool> {
        sha_arg(ancestor, "ancestor")?;
        sha_arg(head_sha, "head_sha")?;
        let lock = self.lock(repo.repo_id);
        let _guard = lock.lock().await;
        let bare = self.mirror_path(repo.repo_id);
        let out = self
            .cfg
            .git
            .local(&bare, &["merge-base", "--is-ancestor", ancestor, head_sha])
            .await?;
        match out.code {
            Some(0) => Ok(true),
            Some(GIT_NOT_AN_ANCESTOR | GIT_UNKNOWN_COMMIT) => Ok(false),
            _ => Err(failed("merge_base", &out)),
        }
    }

    async fn carries_patch(
        &self,
        repo: &RepoRef,
        commit: &str,
        head_sha: &str,
    ) -> PortResult<bool> {
        sha_arg(commit, "commit")?;
        sha_arg(head_sha, "head_sha")?;
        let lock = self.lock(repo.repo_id);
        let _guard = lock.lock().await;
        let bare = self.mirror_path(repo.repo_id);
        if !self.has_commit(&bare, commit).await? || !self.has_commit(&bare, head_sha).await? {
            return Ok(false);
        }
        let wanted = self
            .patch_ids(
                &bare,
                &[
                    "show",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--format=commit %H",
                    commit,
                ],
            )
            .await?;
        let Some((wanted, _)) = wanted.first() else {
            return Ok(false);
        };
        let window = format!("-{PATCH_SCAN_COMMITS}");
        let scanned = self
            .patch_ids(
                &bare,
                &[
                    "log",
                    "-p",
                    "--no-merges",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--format=commit %H",
                    &window,
                    head_sha,
                ],
            )
            .await?;
        Ok(scanned.iter().any(|(id, _)| id == wanted))
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

    async fn reset_branch(&self, env: &EnvRef, to_sha: &str) -> PortResult<()> {
        sha_arg(to_sha, "to_sha")?;
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
        if !self.has_commit(&bare, to_sha).await? {
            return Err(PortError::permanent(
                "commit_missing",
                json!({ "commit": to_sha }),
            ));
        }
        self.git_ok(&wt, &["reset", "--hard", "--quiet", to_sha], "reset")
            .await?;
        self.git_ok(&wt, &["clean", "-fdq"], "clean").await?;
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
