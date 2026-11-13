#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use saka_core::authorship::GitIdentity;
use saka_core::ids::{EnvironmentId, RepoId};
use saka_env::{DirectSupervisor, Git, GitRepos, RecipeRunner, ReposConfig};
use saka_ports::{
    Checkout, EnvRef, Environments, GitAccess, GitRemote, PortResult, PrepareRequest, RepoRef,
    RevertRequest, Reverted, UnitLimits,
};
use uuid::Uuid;

const BRANCH: &str = "saka/revert/app-s1-r1";
const ABSENT: &str = "1111111111111111111111111111111111111111";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Dev")
        .env("GIT_AUTHOR_EMAIL", "dev@saka.invalid")
        .env("GIT_COMMITTER_NAME", "Dev")
        .env("GIT_COMMITTER_EMAIL", "dev@saka.invalid")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct LocalAccess {
    url: String,
}

#[async_trait]
impl GitAccess for LocalAccess {
    async fn remote(&self, _repo: &RepoRef) -> PortResult<GitRemote> {
        Ok(GitRemote {
            url: self.url.clone(),
            config: Vec::new(),
        })
    }

    async fn refused(&self, _repo: &RepoRef) {}
}

struct World {
    _dir: tempfile::TempDir,
    remote: PathBuf,
    work: PathBuf,
    repos: GitRepos,
    repo: RepoRef,
    next_env: u128,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let remote = root.join("remote.git");
        let work = root.join("work");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare", "-b", "main"]);
        git(&root, &["clone", "-q", remote.to_str().unwrap(), "work"]);
        git(&work, &["checkout", "-q", "-b", "main"]);
        std::fs::write(work.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(work.join("b.txt"), "base\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "seed"]);
        git(&work, &["push", "-q", "origin", "main"]);
        let repos = GitRepos::new(
            ReposConfig {
                repos_dir: root.join("data/repos"),
                envs_dir: root.join("data/envs"),
                git: Git::new("git", "/usr/local/bin:/usr/bin:/bin"),
                recipe: RecipeRunner {
                    supervisor: Arc::new(DirectSupervisor::new(root.join("data/units"))),
                    limits: UnitLimits {
                        memory_max: "1G".into(),
                        cpu_quota: "100%".into(),
                        tasks_max: 64,
                    },
                    timeout: std::time::Duration::from_secs(2),
                    logs_dir: root.join("data/logs/environments"),
                    path: vec!["/usr/bin".into(), "/bin".into()],
                },
            },
            Arc::new(LocalAccess {
                url: remote.to_string_lossy().into_owned(),
            }),
        );
        World {
            _dir: dir,
            remote,
            work,
            repos,
            repo: RepoRef {
                repo_id: RepoId(Uuid::from_u128(7)),
                integration_id: "github".into(),
                full_name: "acme/app".into(),
                url: "https://scm.invalid/acme/app".into(),
            },
            next_env: 1,
        }
    }

    fn write(&self, file: &str, text: &str, message: &str) -> String {
        std::fs::write(self.work.join(file), text).unwrap();
        git(&self.work, &["add", "-A"]);
        git(&self.work, &["commit", "-q", "-m", message]);
        git(&self.work, &["rev-parse", "HEAD"])
    }

    fn push_main(&self) -> String {
        git(&self.work, &["push", "-q", "origin", "main"]);
        git(&self.work, &["rev-parse", "HEAD"])
    }

    fn tip(&self) -> String {
        git(&self.remote, &["rev-parse", "refs/heads/main"])
    }

    async fn env(&mut self) -> EnvRef {
        let id = EnvironmentId(Uuid::from_u128(self.next_env));
        self.next_env += 1;
        let prepared = self
            .repos
            .prepare(&PrepareRequest {
                environment_id: id,
                repo: self.repo.clone(),
                checkout: Checkout::Branch(BRANCH.into()),
                base_branch: "main".into(),
            })
            .await
            .unwrap();
        EnvRef {
            environment_id: id,
            repo: self.repo.clone(),
            branch: Some(BRANCH.into()),
            root_path: prepared.root_path,
            worktree_path: prepared.worktree_path,
            vars: std::collections::BTreeMap::new(),
            path_prefix: Vec::new(),
            secrets: Vec::new(),
            extra_dirs: Vec::new(),
        }
    }

    async fn revert(
        &mut self,
        merged: &str,
        head: Option<&str>,
        number: u64,
    ) -> (EnvRef, Reverted) {
        let env = self.env().await;
        let request = RevertRequest {
            base_sha: git(&wt(&env), &["rev-parse", "HEAD"]),
            merge_commit: merged.into(),
            head_sha: head.map(String::from),
            change_number: number,
            message: "Revert \"Add the thing\"\n\nThis reverts the commit from https://scm.invalid/acme/app/pull/7.\n\nReason: it broke the build\n".into(),
            author: GitIdentity {
                name: "Octo Cat".into(),
                email: "1+octo@users.noreply.github.com".into(),
            },
            committer: GitIdentity {
                name: "Saka Agent".into(),
                email: "agent@saka.invalid".into(),
            },
        };
        let outcome = self.repos.revert(&env, &request).await.unwrap();
        (env, outcome)
    }
}

fn wt(env: &EnvRef) -> PathBuf {
    PathBuf::from(&env.worktree_path)
}

fn read(env: &EnvRef, file: &str) -> Option<String> {
    std::fs::read_to_string(wt(env).join(file)).ok()
}

fn committed(outcome: &Reverted) -> String {
    match outcome {
        Reverted::Committed { head_sha } => head_sha.clone(),
        other => panic!("{other:?}"),
    }
}

fn assert_single_revert_commit(w: &World, env: &EnvRef, head: &str, base: &str) {
    assert_eq!(git(&wt(env), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(
            &wt(env),
            &["rev-list", "--count", &format!("{base}..{head}")]
        ),
        "1"
    );
    let parents = git(&wt(env), &["rev-list", "--parents", "-n", "1", head]);
    assert_eq!(
        parents.split_whitespace().count(),
        2,
        "a plain commit: {parents}"
    );
    assert_eq!(
        git(&wt(env), &["log", "-1", "--format=%an|%ae|%cn|%ce", head]),
        "Octo Cat|1+octo@users.noreply.github.com|Saka Agent|agent@saka.invalid"
    );
    let message = git(&wt(env), &["log", "-1", "--format=%B", head]);
    assert!(
        message.starts_with("Revert \"Add the thing\"\n\nThis reverts "),
        "{message}"
    );
    assert!(message.ends_with("Reason: it broke the build"), "{message}");
    assert_eq!(w.tip(), base, "nothing is pushed by the revert itself");
    assert_eq!(git(&wt(env), &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn a_merge_commit_is_reverted_against_its_first_parent_as_one_commit() {
    let mut w = World::new();
    git(&w.work, &["checkout", "-q", "-b", "feature"]);
    let head = {
        w.write("a.txt", "one\nTWO\nthree\n", "edit a");
        w.write("c.txt", "new\n", "add c")
    };
    git(&w.work, &["checkout", "-q", "main"]);
    git(
        &w.work,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge pull request #7",
            "feature",
        ],
    );
    let merged = w.push_main();
    let base = w.write("b.txt", "later\n", "unrelated");
    w.push_main();

    let (env, outcome) = w.revert(&merged, Some(&head), 7).await;
    let revert = committed(&outcome);
    assert_single_revert_commit(&w, &env, &revert, &base);
    assert_eq!(read(&env, "a.txt").as_deref(), Some("one\ntwo\nthree\n"));
    assert_eq!(read(&env, "c.txt"), None);
    assert_eq!(read(&env, "b.txt").as_deref(), Some("later\n"));
}

#[tokio::test]
async fn a_squash_commit_is_reverted_as_a_single_commit_even_when_the_pull_request_had_several() {
    let mut w = World::new();
    git(&w.work, &["checkout", "-q", "-b", "feature"]);
    w.write("a.txt", "one\nTWO\nthree\n", "edit a");
    let head = w.write("c.txt", "new\n", "add c");
    git(&w.work, &["push", "-q", "origin", "feature"]);
    git(&w.work, &["checkout", "-q", "main"]);
    git(&w.work, &["merge", "-q", "--squash", "feature"]);
    git(&w.work, &["commit", "-q", "-m", "Add the thing (#7)"]);
    let merged = w.push_main();
    w.write("b.txt", "later\n", "unrelated");
    let base = w.push_main();

    let (env, outcome) = w.revert(&merged, Some(&head), 7).await;
    assert_single_revert_commit(&w, &env, &committed(&outcome), &base);
    assert_eq!(read(&env, "a.txt").as_deref(), Some("one\ntwo\nthree\n"));
    assert_eq!(read(&env, "c.txt"), None);
    assert_eq!(read(&env, "b.txt").as_deref(), Some("later\n"));
}

#[tokio::test]
async fn a_rebased_range_is_reverted_newest_first_and_collapsed_into_one_commit() {
    let mut w = World::new();
    git(&w.work, &["checkout", "-q", "-b", "feature"]);
    w.write("a.txt", "one\nTWO\nthree\n", "edit a");
    w.write("c.txt", "new\n", "add c");
    let head = w.write("c.txt", "newer\n", "edit c");
    git(&w.work, &["push", "-q", "origin", "feature"]);
    git(&w.work, &["checkout", "-q", "main"]);
    w.write("b.txt", "moved on\n", "main moves");
    w.push_main();
    git(&w.work, &["checkout", "-q", "feature"]);
    git(&w.work, &["rebase", "-q", "main"]);
    git(&w.work, &["checkout", "-q", "main"]);
    git(&w.work, &["merge", "-q", "--ff-only", "feature"]);
    let merged = w.push_main();
    assert_ne!(merged, head, "the rebase rewrote the commits");
    w.write("b.txt", "later\n", "unrelated");
    let base = w.push_main();

    let (env, outcome) = w.revert(&merged, Some(&head), 7).await;
    assert_single_revert_commit(&w, &env, &committed(&outcome), &base);
    assert_eq!(read(&env, "a.txt").as_deref(), Some("one\ntwo\nthree\n"));
    assert_eq!(read(&env, "c.txt"), None);
    assert_eq!(read(&env, "b.txt").as_deref(), Some("later\n"));
}

#[tokio::test]
async fn a_later_change_to_the_same_lines_is_a_conflict_that_leaves_the_worktree_clean() {
    let mut w = World::new();
    let merged = w.write("a.txt", "one\nTWO\nthree\n", "Add the thing (#7)");
    w.push_main();
    let base = w.write("a.txt", "one\nTWO!\nthree\n", "touch the same line");
    w.push_main();

    let (env, outcome) = w.revert(&merged, None, 7).await;
    assert_eq!(
        outcome,
        Reverted::Conflict {
            paths: vec!["a.txt".into()],
            count: 1
        }
    );
    assert_eq!(git(&wt(&env), &["rev-parse", "HEAD"]), base);
    assert_eq!(git(&wt(&env), &["status", "--porcelain"]), "");
    assert_eq!(read(&env, "a.txt").as_deref(), Some("one\nTWO!\nthree\n"));
}

#[tokio::test]
async fn a_change_that_a_later_commit_already_pulled_back_reverts_to_nothing() {
    let mut w = World::new();
    let merged = w.write("c.txt", "new\n", "Add the thing (#7)");
    w.push_main();
    git(&w.work, &["rm", "-q", "c.txt"]);
    git(&w.work, &["commit", "-q", "-m", "drop c by hand"]);
    let base = w.push_main();

    let (env, outcome) = w.revert(&merged, None, 7).await;
    assert_eq!(outcome, Reverted::Empty);
    assert_eq!(git(&wt(&env), &["rev-parse", "HEAD"]), base);
    assert_eq!(git(&wt(&env), &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn a_commit_that_is_not_in_the_base_branch_cannot_be_reverted() {
    let mut w = World::new();
    git(&w.work, &["checkout", "-q", "-b", "side"]);
    let stray = w.write("c.txt", "new\n", "never merged");
    git(&w.work, &["push", "-q", "origin", "side"]);
    git(&w.work, &["checkout", "-q", "main"]);

    let (_, outcome) = w.revert(&stray, None, 7).await;
    assert_eq!(outcome, Reverted::NotInBase);
    let (_, outcome) = w.revert(ABSENT, None, 7).await;
    assert_eq!(outcome, Reverted::NotInBase);
}

#[tokio::test]
async fn without_the_original_head_a_squash_is_found_by_the_only_commit_naming_the_pull_request() {
    let mut w = World::new();
    let merged = w.write("a.txt", "one\nTWO\nthree\n", "Add the thing (#7)");
    w.push_main();
    let base = w.write("b.txt", "later\n", "unrelated");
    w.push_main();

    let (env, outcome) = w.revert(&merged, Some(ABSENT), 7).await;
    assert_single_revert_commit(&w, &env, &committed(&outcome), &base);
    assert_eq!(read(&env, "a.txt").as_deref(), Some("one\ntwo\nthree\n"));
}

#[tokio::test]
async fn without_the_original_head_and_with_no_single_mention_the_commits_are_unknown() {
    let mut w = World::new();
    w.write("b.txt", "step one (#7)\n", "first part (#7)");
    let merged = w.write("a.txt", "one\nTWO\nthree\n", "second part (#7)");
    w.push_main();

    let (env, outcome) = w.revert(&merged, Some(ABSENT), 7).await;
    assert_eq!(outcome, Reverted::CommitsUnknown);
    assert_eq!(git(&wt(&env), &["status", "--porcelain"]), "");

    let mut w = World::new();
    let plain = w.write("a.txt", "one\nTWO\nthree\n", "rebased without a mention");
    w.push_main();
    let (_, outcome) = w.revert(&plain, None, 7).await;
    assert_eq!(outcome, Reverted::CommitsUnknown);
}
