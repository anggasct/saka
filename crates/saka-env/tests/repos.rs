//! Mirrors, worktrees, and Saka's push against a local bare remote. `git` runs through a
//! wrapper that logs its argv and the `GIT_CONFIG_*` it was given, so the tests can see
//! where the credential went.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use saka_core::ids::{EnvironmentId, RepoId, WorkItemId};
use saka_env::{Git, GitRepos, ReposConfig};
use saka_ports::{
    EnvRef, Environments, GitAccess, GitRemote, PortResult, PrepareRequest, PushOutcome,
    PushRequest, RepoRef, Repos,
};
use uuid::Uuid;

const SECRET: &str = "AUTHORIZATION: basic eC1hY2Nlc3MtdG9rZW46Z2hzX3Rlc3Q=";
const HEADER_KEY: &str = "http.https://scm.invalid/.extraheader";
const BRANCH: &str = "saka/two-step/s1";

/// Plain git for test setup, isolated from the user's config.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@saka.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@saka.invalid")
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

fn commit(dir: &Path, file: &str, text: &str) -> String {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", file]);
    git(dir, &["rev-parse", "HEAD"])
}

struct StaticAccess {
    url: String,
    refused: AtomicUsize,
}

#[async_trait]
impl GitAccess for StaticAccess {
    async fn remote(&self, _repo: &RepoRef) -> PortResult<GitRemote> {
        Ok(GitRemote {
            url: self.url.clone(),
            config: vec![(HEADER_KEY.to_string(), SECRET.to_string())],
        })
    }

    async fn refused(&self, _repo: &RepoRef) {
        self.refused.fetch_add(1, Ordering::SeqCst);
    }
}

struct World {
    _dir: tempfile::TempDir,
    /// The "remote" every fetch and push goes to.
    remote: PathBuf,
    /// A clone of the remote standing in for someone else.
    other: PathBuf,
    log: PathBuf,
    repos: GitRepos,
    repo: RepoRef,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let remote = root.join("remote.git");
        let other = root.join("other");
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare", "-b", "main"]);
        git(&root, &["clone", "-q", remote.to_str().unwrap(), "other"]);
        git(&other, &["checkout", "-q", "-b", "main"]);
        commit(&other, "README.md", "hello\n");
        git(&other, &["push", "-q", "origin", "main"]);

        let log = root.join("git.log");
        let wrapper = root.join("git-wrapper");
        let real = String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_string();
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\n\
                 {{ printf 'argv'; for a in \"$@\"; do printf ' %s' \"$a\"; done; printf '\\n'; \
                 env | grep '^GIT_CONFIG' | sed 's/^/env /'; }} >> '{log}'\n\
                 exec '{real}' \"$@\"\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();

        let access = Arc::new(StaticAccess {
            url: remote.to_string_lossy().into_owned(),
            refused: AtomicUsize::new(0),
        });
        let repos = GitRepos::new(
            ReposConfig {
                repos_dir: root.join("data/repos"),
                envs_dir: root.join("data/envs"),
                git: Git::new(&wrapper, "/usr/local/bin:/usr/bin:/bin"),
            },
            access,
        );
        let repo = RepoRef {
            repo_id: RepoId(Uuid::from_u128(1)),
            integration_id: "github".into(),
            full_name: "acme/app".into(),
            url: "https://scm.invalid/acme/app".into(),
        };
        World {
            _dir: dir,
            remote,
            other,
            log,
            repos,
            repo,
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn remote_branch(&self, branch: &str) -> Option<String> {
        let out = git(
            &self.remote,
            &[
                "for-each-ref",
                "--format=%(objectname)",
                &format!("refs/heads/{branch}"),
            ],
        );
        (!out.is_empty()).then_some(out)
    }

    async fn prepare(&self, n: u128) -> EnvRef {
        let id = EnvironmentId(Uuid::from_u128(n));
        let prepared = self
            .repos
            .prepare(&PrepareRequest {
                environment_id: id,
                work_item_id: WorkItemId(Uuid::from_u128(9)),
                repo: self.repo.clone(),
                branch: BRANCH.into(),
                base_branch: "main".into(),
            })
            .await
            .unwrap();
        EnvRef {
            environment_id: id,
            repo: self.repo.clone(),
            branch: BRANCH.into(),
            root_path: prepared.root_path,
            worktree_path: prepared.worktree_path,
        }
    }

    async fn push(&self, local: &str, expected: Option<&str>) -> PushOutcome {
        self.repos
            .push(&PushRequest {
                repo: self.repo.clone(),
                branch: BRANCH.into(),
                local_sha: local.into(),
                expected_remote_sha: expected.map(String::from),
            })
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn the_token_reaches_only_the_git_child_through_git_config_variables() {
    let w = World::new();
    w.repos.fetch(&w.repo).await.unwrap();
    let env = w.prepare(1).await;
    let sha = commit(Path::new(&env.worktree_path), "a.txt", "a\n");
    assert!(matches!(
        w.push(&sha, None).await,
        PushOutcome::Pushed { .. }
    ));

    let log = w.log();
    let remote_calls: Vec<&str> = log
        .lines()
        .filter(|l| {
            l.starts_with("argv")
                && (l.contains(" fetch") || l.contains(" push") || l.contains(" ls-remote"))
        })
        .collect();
    assert!(remote_calls.len() >= 3, "{log}");
    for line in log.lines().filter(|l| l.starts_with("argv")) {
        assert!(!line.contains("eC1hY2Nlc3M"), "token in argv: {line}");
    }
    assert!(log.contains("env GIT_CONFIG_COUNT=1"), "{log}");
    assert!(
        log.contains(&format!("env GIT_CONFIG_KEY_0={HEADER_KEY}")),
        "{log}"
    );
    assert!(
        log.contains(&format!("env GIT_CONFIG_VALUE_0={SECRET}")),
        "{log}"
    );
    assert!(log.contains("env GIT_CONFIG_GLOBAL=/dev/null"), "{log}");
    assert!(log.contains("env GIT_CONFIG_NOSYSTEM=1"), "{log}");

    // Local commands get no credential at all.
    let mut current_is_local = false;
    for line in log.lines() {
        if line.starts_with("argv") {
            current_is_local = line.contains(" worktree ")
                || line.contains(" rev-parse ")
                || line.contains(" config ");
        } else if current_is_local {
            assert!(
                !line.contains("GIT_CONFIG_VALUE"),
                "credential given to a local command: {line}"
            );
        }
    }

    // Nothing persisted the credential: not the mirror config, not the worktree.
    let bare = w.repos.mirror_path(w.repo.repo_id);
    let config = std::fs::read_to_string(bare.join("config")).unwrap();
    assert!(
        !config.contains("extraheader") && !config.contains("eC1hY2Nlc3M"),
        "{config}"
    );
    assert!(
        config.contains(&format!("url = {}", w.remote.display())),
        "{config}"
    );
}

#[tokio::test]
async fn saka_pushes_with_a_lease_and_never_overwrites_someone_else() {
    let w = World::new();
    let env = w.prepare(1).await;
    let wt = PathBuf::from(&env.worktree_path);

    // A new branch: the lease says it must not exist yet.
    let first = commit(&wt, "a.txt", "a\n");
    assert_eq!(
        w.push(&first, None).await,
        PushOutcome::Pushed { sha: first.clone() }
    );
    assert_eq!(w.remote_branch(BRANCH), Some(first.clone()));
    assert!(
        w.log().contains(&format!(
            "--force-with-lease=refs/heads/{BRANCH}: origin {BRANCH}:{BRANCH}"
        )),
        "{}",
        w.log()
    );

    // The remote already at the same sha: success without a push.
    let pushes = |log: String| {
        log.lines()
            .filter(|l| l.starts_with("argv") && l.contains(" push "))
            .count()
    };
    let before = pushes(w.log());
    assert_eq!(
        w.push(&first, None).await,
        PushOutcome::Pushed { sha: first.clone() }
    );
    assert_eq!(pushes(w.log()), before);

    // An update leases against the sha Saka pushed last.
    let second = commit(&wt, "b.txt", "b\n");
    assert_eq!(
        w.push(&second, Some(&first)).await,
        PushOutcome::Pushed {
            sha: second.clone()
        }
    );
    assert!(
        w.log()
            .contains(&format!("--force-with-lease=refs/heads/{BRANCH}:{first}"))
    );
    assert_eq!(w.remote_branch(BRANCH), Some(second.clone()));

    // Someone else moves the branch: Saka refuses to overwrite it.
    git(&w.other, &["fetch", "-q", "origin"]);
    git(
        &w.other,
        &[
            "checkout",
            "-q",
            "-b",
            "theirs",
            &format!("origin/{BRANCH}"),
        ],
    );
    let theirs = commit(&w.other, "c.txt", "c\n");
    git(
        &w.other,
        &["push", "-q", "origin", &format!("theirs:{BRANCH}")],
    );
    let third = commit(&wt, "d.txt", "d\n");
    assert_eq!(
        w.push(&third, Some(&second)).await,
        PushOutcome::Rejected {
            remote_sha: theirs.clone()
        }
    );
    assert_eq!(w.remote_branch(BRANCH), Some(theirs));
}

#[tokio::test]
async fn a_worktree_starts_at_the_latest_base_and_reports_its_state() {
    let w = World::new();
    let base_before = git(&w.other, &["rev-parse", "HEAD"]);
    let newer = commit(&w.other, "NEWS.md", "news\n");
    git(&w.other, &["push", "-q", "origin", "main"]);
    assert_ne!(base_before, newer);

    let env = w.prepare(1).await;
    let wt = PathBuf::from(&env.worktree_path);
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), newer);
    assert_eq!(git(&wt, &["branch", "--show-current"]), BRANCH);
    assert!(wt.join("NEWS.md").is_file());

    // Preparing the same environment again returns it as it is.
    std::fs::write(wt.join("keep.txt"), "x").unwrap();
    let again = w.prepare(1).await;
    assert_eq!(again.worktree_path, env.worktree_path);
    assert!(wt.join("keep.txt").is_file());

    let state = w.repos.inspect(&env, &newer).await.unwrap();
    assert!(state.dirty);
    assert_eq!(state.commits_ahead, 0);
    let sha = commit(&wt, "keep.txt", "x\n");
    let state = w.repos.inspect(&env, &newer).await.unwrap();
    assert_eq!(
        (state.dirty, state.commits_ahead, state.head_sha),
        (false, 1, sha)
    );

    // Files are read byte-exact at a commit of the base branch.
    let tip = w.repos.tip(&w.repo, "main").await.unwrap().unwrap();
    assert_eq!(tip, newer);
    assert_eq!(
        w.repos.read_file(&w.repo, &tip, "NEWS.md").await.unwrap(),
        Some(b"news\n".to_vec())
    );
    assert_eq!(
        w.repos
            .read_file(&w.repo, &tip, "missing.md")
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        w.repos
            .read_file(&w.repo, &base_before, "NEWS.md")
            .await
            .unwrap(),
        None
    );
    assert_eq!(w.repos.tip(&w.repo, "nope").await.unwrap(), None);
}

#[tokio::test]
async fn sync_checks_the_merge_on_the_base_branch_and_teardown_cleans_up() {
    let w = World::new();
    let env = w.prepare(1).await;
    let wt = PathBuf::from(&env.worktree_path);
    let sha = commit(&wt, "a.txt", "a\n");
    w.push(&sha, None).await;
    assert!(!w.repos.is_merged(&w.repo, &sha, "main").await.unwrap());

    git(&w.other, &["checkout", "-q", "main"]);
    git(&w.other, &["fetch", "-q", "origin"]);
    git(
        &w.other,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "merge",
            &format!("origin/{BRANCH}"),
        ],
    );
    let merge = git(&w.other, &["rev-parse", "HEAD"]);
    git(&w.other, &["push", "-q", "origin", "main"]);
    assert!(w.repos.is_merged(&w.repo, &merge, "main").await.unwrap());
    assert!(
        !w.repos
            .is_merged(&w.repo, &"0".repeat(40), "main")
            .await
            .unwrap()
    );

    w.repos.teardown(&env).await.unwrap();
    assert!(!Path::new(&env.root_path).exists());
    let bare = w.repos.mirror_path(w.repo.repo_id);
    assert_eq!(git(&bare, &["branch", "--list", BRANCH]), "");
    assert!(!git(&bare, &["worktree", "list"]).contains("worktree"));
    // Tearing down twice is fine.
    w.repos.teardown(&env).await.unwrap();
}

#[tokio::test]
async fn a_new_environment_replaces_an_unfinished_one_that_holds_the_branch() {
    let w = World::new();
    let old = w.prepare(1).await;
    // The old environment never finished: its marker is gone.
    std::fs::remove_file(Path::new(&old.root_path).join("base-sha")).unwrap();
    let new = w.prepare(2).await;
    assert_ne!(old.worktree_path, new.worktree_path);
    assert!(!Path::new(&old.worktree_path).exists());
    let wt = PathBuf::from(&new.worktree_path);
    assert_eq!(git(&wt, &["branch", "--show-current"]), BRANCH);
}

#[test]
fn paths_resolve_against_the_worktree_through_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let wt = root.join("worktree");
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::create_dir_all(root.join("secret")).unwrap();
    std::os::unix::fs::symlink(root.join("secret"), wt.join("out")).unwrap();
    std::os::unix::fs::symlink(wt.join("src"), wt.join("in")).unwrap();
    std::os::unix::fs::symlink(root.join("nowhere"), wt.join("dangling")).unwrap();
    let r = |raw: &str| saka_env::repos::resolve(&wt, raw);

    let inside = |p: &str| saka_core::policy::RequestPath {
        path: p.into(),
        inside_worktree: true,
    };
    assert_eq!(r("src/main.go"), inside("src/main.go"));
    assert_eq!(
        r(&format!("{}/src/new/file.go", wt.display())),
        inside("src/new/file.go")
    );
    assert_eq!(r("./src/../README.md"), inside("README.md"));
    assert_eq!(r("in/lib.go"), inside("src/lib.go"));
    assert_eq!(r("new/../x"), inside("x"));
    for raw in [
        "../secret/key",
        "out/key",
        "dangling/key",
        "/etc/passwd",
        "src/../../secret",
        "in/../../secret",
    ] {
        assert!(!r(raw).inside_worktree, "{raw}");
        assert_eq!(r(raw).path, raw);
    }
    // `out/..` follows the link first, as the file system does.
    assert_eq!(r("out/../worktree/src"), inside("src"));
}
