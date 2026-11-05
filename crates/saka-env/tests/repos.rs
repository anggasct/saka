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
use saka_core::ids::{EnvironmentId, RepoId};
use saka_core::secret::{SecretText, SecretVar};
use saka_core::vocab::SecretPurpose;
use saka_env::{DirectSupervisor, Git, GitRepos, RecipeRunner, ReposConfig};
use saka_ports::{
    Blob, Checkout, DocsRequest, EnvRef, Environments, ExtraDir, GitAccess, GitRemote, PortResult,
    PrepareRequest, PreparedDocs, PushOutcome, PushRequest, RecipeCommand, RecipePhase, RepoRef,
    Repos, UnitLimits, WorktreeFile,
};
use uuid::Uuid;

const SECRET: &str = "AUTHORIZATION: basic eC1hY2Nlc3MtdG9rZW46Z2hzX3Rlc3Q=";
const HEADER_KEY: &str = "http.https://scm.invalid/.extraheader";
const BRANCH: &str = "saka/two-step/s1";

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
    remote: PathBuf,
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
        let source = root.join("git-wrapper.sh");
        std::fs::write(
            &source,
            format!(
                "#!/bin/sh\n\
                 {{ printf 'argv'; for a in \"$@\"; do printf ' %s' \"$a\"; done; printf '\\n'; \
                 env | grep '^GIT_CONFIG' | sed 's/^/env /'; }} >> '{log}'\n\
                 exec '{real}' \"$@\"\n",
                log = log.display()
            ),
        )
        .unwrap();
        // A child forked by a parallel test keeps our write fd until it execs, and running the
        // wrapper then fails with ETXTBSY. `install` writes it in its own process instead.
        let installed = Command::new("install")
            .arg("-m")
            .arg("755")
            .arg(&source)
            .arg(&wrapper)
            .status()
            .unwrap();
        assert!(installed.success());

        let access = Arc::new(StaticAccess {
            url: remote.to_string_lossy().into_owned(),
            refused: AtomicUsize::new(0),
        });
        let repos = GitRepos::new(
            ReposConfig {
                repos_dir: root.join("data/repos"),
                envs_dir: root.join("data/envs"),
                git: Git::new(&wrapper, "/usr/local/bin:/usr/bin:/bin"),
                recipe: RecipeRunner {
                    supervisor: Arc::new(DirectSupervisor::new(root.join("data/units"))),
                    limits: UnitLimits {
                        memory_max: "1G".into(),
                        cpu_quota: "100%".into(),
                        tasks_max: 64,
                    },
                    timeout: std::time::Duration::from_secs(2),
                    logs_dir: root.join("data/logs/environments"),
                    path: vec!["/usr/local/bin".into(), "/usr/bin".into(), "/bin".into()],
                },
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
        self.prepare_with(n, Checkout::Branch(BRANCH.into())).await
    }

    async fn prepare_with(&self, n: u128, checkout: Checkout) -> EnvRef {
        let id = EnvironmentId(Uuid::from_u128(n));
        let branch = checkout.branch().map(String::from);
        let prepared = self
            .repos
            .prepare(&PrepareRequest {
                environment_id: id,
                repo: self.repo.clone(),
                checkout,
                base_branch: "main".into(),
            })
            .await
            .unwrap();
        EnvRef {
            environment_id: id,
            repo: self.repo.clone(),
            branch,
            root_path: prepared.root_path,
            worktree_path: prepared.worktree_path,
            vars: std::collections::BTreeMap::from([("APP_ENV".to_string(), "test".to_string())]),
            path_prefix: vec!["/opt/toolchains/node/bin".into()],
            secrets: Vec::new(),
            extra_dirs: Vec::new(),
        }
    }

    fn docs_repo(&self) -> RepoRef {
        RepoRef {
            repo_id: RepoId(Uuid::from_u128(2)),
            integration_id: "github".into(),
            full_name: "acme/docs".into(),
            url: "https://scm.invalid/acme/docs".into(),
        }
    }

    async fn prepare_docs(
        &self,
        env: &EnvRef,
        commit: &str,
        project_path: &str,
    ) -> PortResult<PreparedDocs> {
        self.repos
            .prepare_docs(&DocsRequest {
                environment_id: env.environment_id,
                repo: self.docs_repo(),
                commit: commit.into(),
                project_path: project_path.into(),
                branch: None,
            })
            .await
    }

    async fn prepare_writable_docs(&self, env: &EnvRef, commit: &str) -> PreparedDocs {
        self.repos
            .prepare_docs(&DocsRequest {
                environment_id: env.environment_id,
                repo: self.docs_repo(),
                commit: commit.into(),
                project_path: "docs".into(),
                branch: Some(format!("{BRANCH}-docs")),
            })
            .await
            .unwrap()
    }

    async fn run(
        &self,
        env: &EnvRef,
        index: usize,
        command: &str,
    ) -> saka_ports::RecipeCommandOutcome {
        self.repos
            .run_recipe_command(
                env,
                &RecipeCommand {
                    phase: RecipePhase::Setup,
                    index,
                    command: command.into(),
                    run: None,
                    timeout_ms: None,
                },
            )
            .await
            .unwrap()
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
async fn docs_are_read_with_a_size_bound_and_found_by_pattern_at_a_commit() {
    let w = World::new();
    std::fs::create_dir_all(w.other.join("docs/sub")).unwrap();
    std::fs::write(
        w.other.join("docs/always.md"),
        "---\ncontext: always\n---\n",
    )
    .unwrap();
    std::fs::write(w.other.join("docs/sub/b.markdown"), "context:  always\r\n").unwrap();
    std::fs::write(w.other.join("docs/plain.md"), "context: sometimes\n").unwrap();
    std::fs::write(w.other.join("docs/bin.md"), b"context: always\n\0\0").unwrap();
    std::fs::write(w.other.join("docs/notes.txt"), "context: always\n").unwrap();
    let tip = commit(&w.other, "README.md", "context: always\n");
    git(&w.other, &["push", "-q", "origin", "main"]);
    w.repos.fetch(&w.repo).await.unwrap();

    let specs = vec![
        ":(glob)docs/**/*.md".to_string(),
        ":(glob)docs/**/*.markdown".to_string(),
    ];
    let pattern = "^context:[[:space:]]*always[[:space:]]*$";
    let mut found = w
        .repos
        .grep_paths(&w.repo, &tip, pattern, &specs)
        .await
        .unwrap();
    found.sort();
    assert_eq!(found, vec!["docs/always.md", "docs/sub/b.markdown"]);
    let none = w
        .repos
        .grep_paths(&w.repo, &tip, "^never-here$", &specs)
        .await
        .unwrap();
    assert!(none.is_empty());

    assert_eq!(
        w.repos
            .read_bounded(&w.repo, &tip, "docs/always.md", 1024)
            .await
            .unwrap(),
        Blob::Bytes(b"---\ncontext: always\n---\n".to_vec())
    );
    assert_eq!(
        w.repos
            .read_bounded(&w.repo, &tip, "docs/always.md", 8)
            .await
            .unwrap(),
        Blob::TooLarge { bytes: 24 }
    );
    for path in ["docs", "docs/missing.md"] {
        assert_eq!(
            w.repos
                .read_bounded(&w.repo, &tip, path, 1024)
                .await
                .unwrap(),
            Blob::Missing,
            "{path}"
        );
    }
    let bad = w
        .repos
        .read_bounded(&w.repo, "HEAD", "docs/always.md", 1024)
        .await
        .unwrap_err();
    assert_eq!(bad.code, "git_argument_invalid");
}

#[tokio::test]
async fn sync_checks_the_merge_on_the_base_branch_and_teardown_cleans_up() {
    let w = World::new();
    let env = w.prepare(1).await;
    let wt = PathBuf::from(&env.worktree_path);
    let sha = commit(&wt, "a.txt", "a\n");
    w.push(&sha, None).await;
    assert_eq!(
        w.repos
            .contains_commit(&w.repo, &sha, "main")
            .await
            .unwrap(),
        None
    );

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
    assert_eq!(
        w.repos
            .contains_commit(&w.repo, &merge, "main")
            .await
            .unwrap(),
        Some(merge.clone())
    );
    assert_eq!(
        w.repos
            .contains_commit(&w.repo, &"0".repeat(40), "main")
            .await
            .unwrap(),
        None
    );

    std::fs::write(wt.join("a.txt"), "changed\n").unwrap();
    std::fs::create_dir_all(wt.join("node_modules")).unwrap();
    std::fs::write(wt.join("node_modules/kept.js"), "kept\n").unwrap();
    std::fs::write(wt.join("stray.txt"), "stray\n").unwrap();
    let exclude = wt.join(git(&wt, &["rev-parse", "--git-path", "info/exclude"]));
    std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    std::fs::write(&exclude, "node_modules/\n").unwrap();
    w.repos.check_out(&env, &merge).await.unwrap();
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), merge);
    assert_eq!(git(&wt, &["status", "--porcelain"]), "");
    assert_eq!(std::fs::read_to_string(wt.join("a.txt")).unwrap(), "a\n");
    assert!(!wt.join("stray.txt").exists());
    assert!(wt.join("node_modules/kept.js").exists());
    let missing = w.repos.check_out(&env, &"1".repeat(40)).await.unwrap_err();
    assert_eq!(missing.code, "commit_missing");

    w.repos.teardown(&env).await.unwrap();
    assert!(!Path::new(&env.root_path).exists());
    let bare = w.repos.mirror_path(w.repo.repo_id);
    assert_eq!(git(&bare, &["branch", "--list", BRANCH]), "");
    assert!(!git(&bare, &["worktree", "list"]).contains("worktree"));
    w.repos.teardown(&env).await.unwrap();
}

#[tokio::test]
async fn a_new_environment_replaces_an_unfinished_one_that_holds_the_branch() {
    let w = World::new();
    let old = w.prepare(1).await;
    std::fs::remove_file(Path::new(&old.root_path).join("base-sha")).unwrap();
    let new = w.prepare(2).await;
    assert_ne!(old.worktree_path, new.worktree_path);
    assert!(!Path::new(&old.worktree_path).exists());
    let wt = PathBuf::from(&new.worktree_path);
    assert_eq!(git(&wt, &["branch", "--show-current"]), BRANCH);
}

#[tokio::test]
async fn a_detached_worktree_sits_at_the_base_without_a_branch_and_leaves_no_branch_behind() {
    let w = World::new();
    let base = git(&w.other, &["rev-parse", "HEAD"]);
    let env = w.prepare_with(1, Checkout::Detached).await;
    let wt = PathBuf::from(&env.worktree_path);
    assert_eq!(env.branch, None);
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), base);
    assert_eq!(git(&wt, &["branch", "--show-current"]), "");
    let bare = w.repos.mirror_path(w.repo.repo_id);
    assert_eq!(git(&bare, &["branch", "--list"]), "");
    assert!(w.log().contains("worktree add --quiet --detach"));

    let again = w.prepare_with(1, Checkout::Detached).await;
    assert_eq!(again.worktree_path, env.worktree_path);

    std::fs::write(&w.log, "").unwrap();
    w.repos.teardown(&env).await.unwrap();
    assert!(!Path::new(&env.root_path).exists());
    assert!(!git(&bare, &["worktree", "list"]).contains("worktree"));
    assert!(!w.log().contains("branch -D"), "{}", w.log());
}

#[tokio::test]
async fn worktree_files_are_read_and_written_as_exact_bytes_without_following_links() {
    let w = World::new();
    let env = w.prepare_with(1, Checkout::Detached).await;
    let wt = PathBuf::from(&env.worktree_path);
    let read = |path: &'static str, max: u64| {
        let env = env.clone();
        let repos = &w.repos;
        async move { repos.read_worktree_file(&env, path, max).await.unwrap() }
    };

    assert_eq!(read("docs/specs/a.md", 64).await, WorktreeFile::Missing);
    assert_eq!(read("README.md/x.md", 64).await, WorktreeFile::Missing);
    let bytes = b"# A\r\n\r\n- [ ] one\xc3\xa9\n\tend".to_vec();
    w.repos
        .write_worktree_file(&env, "docs/specs/a.md", &bytes)
        .await
        .unwrap();
    assert_eq!(std::fs::read(wt.join("docs/specs/a.md")).unwrap(), bytes);
    assert_eq!(
        read("docs/specs/a.md", 64).await,
        WorktreeFile::Bytes(bytes.clone())
    );
    let size = u64::try_from(bytes.len()).unwrap();
    assert_eq!(
        read("docs/specs/a.md", size - 1).await,
        WorktreeFile::TooLarge { bytes: size }
    );
    assert_eq!(
        read("docs/specs/a.md", size).await,
        WorktreeFile::Bytes(bytes)
    );

    w.repos
        .write_worktree_file(&env, "docs/specs/a.md", b"second")
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(wt.join("docs/specs/a.md")).unwrap(),
        b"second"
    );
    let leftovers: Vec<_> = std::fs::read_dir(wt.join("docs/specs"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(leftovers, vec![std::ffi::OsString::from("a.md")]);

    std::fs::write(w._dir.path().join("outside.md"), "secret").unwrap();
    std::os::unix::fs::symlink(w._dir.path().join("outside.md"), wt.join("docs/link.md")).unwrap();
    std::os::unix::fs::symlink(wt.join("docs/specs"), wt.join("docs/alias")).unwrap();
    assert_eq!(read("docs/link.md", 64).await, WorktreeFile::NotRegular);
    assert_eq!(read("docs/alias/a.md", 64).await, WorktreeFile::NotRegular);
    assert_eq!(read("docs", 64).await, WorktreeFile::NotRegular);
    for path in ["docs/link.md", "docs/alias/b.md", "docs"] {
        let refused = w
            .repos
            .write_worktree_file(&env, path, b"x")
            .await
            .unwrap_err();
        assert_eq!(refused.code, "worktree_path_conflict", "{path}");
    }
    assert_eq!(
        std::fs::read_to_string(w._dir.path().join("outside.md")).unwrap(),
        "secret"
    );
    for path in ["../outside.md", "/etc/passwd", "docs/../README.md", ""] {
        let refused = w
            .repos
            .read_worktree_file(&env, path, 64)
            .await
            .unwrap_err();
        assert_eq!(refused.code, "worktree_path_invalid", "{path}");
    }
}

#[test]
fn a_relative_path_resolves_from_the_session_cwd_and_stays_relative_to_the_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path().canonicalize().unwrap().join("worktree");
    std::fs::create_dir_all(wt.join("docs/specs")).unwrap();
    let r = |cwd: &str, raw: &str| saka_env::repos::resolve_in(&wt, cwd, raw);
    let inside = |p: &str| saka_core::policy::RequestPath {
        path: p.into(),
        inside_worktree: true,
        through_symlink: false,
    };
    assert_eq!(r("docs", "specs/a.md"), inside("docs/specs/a.md"));
    assert_eq!(r("docs", "./specs/../a.md"), inside("docs/a.md"));
    assert_eq!(r("docs", "../src/main.rs"), inside("src/main.rs"));
    assert_eq!(
        r("docs", &format!("{}/docs/specs/a.md", wt.display())),
        inside("docs/specs/a.md")
    );
    assert_eq!(r("", "specs/a.md"), inside("specs/a.md"));
    let outside = r("docs", "../../etc/passwd");
    assert!(!outside.inside_worktree);
    assert_eq!(outside.path, "../../etc/passwd");
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
        through_symlink: false,
    };
    assert_eq!(r("src/main.go"), inside("src/main.go"));
    assert_eq!(
        r(&format!("{}/src/new/file.go", wt.display())),
        inside("src/new/file.go")
    );
    assert_eq!(r("./src/../README.md"), inside("README.md"));
    let linked = |p: &str| saka_core::policy::RequestPath {
        through_symlink: true,
        ..inside(p)
    };
    assert_eq!(r("in/lib.go"), linked("src/lib.go"));
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
    assert_eq!(r("out/../worktree/src"), linked("src"));
}

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn read_only_module_cache(dir: &Path) {
    let module = dir.join("mod/golang.org/toolchain@v0.0.1-go1.26.0.linux-amd64");
    let bin = module.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("go"), "binary").unwrap();
    std::fs::write(module.join("LICENSE"), "text").unwrap();
    set_mode(&bin.join("go"), 0o444);
    set_mode(&module.join("LICENSE"), 0o444);
    for d in [
        bin.clone(),
        module.clone(),
        dir.join("mod/golang.org"),
        dir.join("mod"),
    ] {
        set_mode(&d, 0o555);
    }
}

#[tokio::test]
async fn teardown_removes_read_only_trees_like_the_go_module_cache() {
    let w = World::new();
    let env = w.prepare(1).await;
    let root = PathBuf::from(&env.root_path);
    read_only_module_cache(&root.join("home/go/pkg"));
    read_only_module_cache(&PathBuf::from(&env.worktree_path).join(".cache"));

    w.repos.teardown(&env).await.unwrap();

    assert!(!root.exists());
    let bare = w.repos.mirror_path(w.repo.repo_id);
    assert_eq!(git(&bare, &["branch", "--list", BRANCH]), "");
    assert!(!git(&bare, &["worktree", "list"]).contains("worktree"));
}

#[tokio::test]
async fn teardown_never_follows_a_symlink_out_of_the_environment() {
    let w = World::new();
    let env = w.prepare(1).await;
    let root = PathBuf::from(&env.root_path);
    let locked = w._dir.path().join("outside/locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("keep.txt"), "keep").unwrap();
    set_mode(&locked, 0o555);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::os::unix::fs::symlink(&locked, root.join("home/link")).unwrap();

    w.repos.teardown(&env).await.unwrap();

    assert!(!root.exists());
    let mode = std::fs::metadata(&locked).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o555);
    assert_eq!(
        std::fs::read_to_string(locked.join("keep.txt")).unwrap(),
        "keep"
    );
    set_mode(&locked, 0o755);
}

#[tokio::test]
async fn the_recipe_comes_from_the_default_branch_commit_and_not_from_the_task_branch() {
    let w = World::new();
    std::fs::create_dir_all(w.other.join(".saka")).unwrap();
    commit(
        &w.other,
        ".saka/environment.toml",
        "[setup]\ncommands = [\"echo default > marker\"]\n",
    );
    git(&w.other, &["push", "-q", "origin", "main"]);
    git(&w.other, &["checkout", "-q", "-b", BRANCH]);
    commit(
        &w.other,
        ".saka/environment.toml",
        "[setup]\ncommands = [\"echo task > marker\"]\n",
    );
    git(&w.other, &["push", "-q", "origin", BRANCH]);

    w.repos.fetch(&w.repo).await.unwrap();
    let env = w.prepare(5).await;
    let base = git(Path::new(&env.worktree_path), &["rev-parse", "HEAD"]);
    let recipe = w.repos.read_recipe(&w.repo, &base).await.unwrap().unwrap();
    assert!(recipe.contains("echo default"), "{recipe}");
    assert!(!recipe.contains("echo task"), "{recipe}");

    let first = git(&w.other, &["rev-list", "--max-parents=0", "HEAD"]);
    assert_eq!(w.repos.read_recipe(&w.repo, &first).await.unwrap(), None);
}

#[tokio::test]
async fn a_recipe_command_runs_in_the_worktree_with_only_the_minimal_environment() {
    let w = World::new();
    w.repos.fetch(&w.repo).await.unwrap();
    let env = w.prepare(6).await;
    let outcome = w
        .run(
            &env,
            0,
            "pwd > where; env | sort > vars; echo token ghp_aB1caB1caB1caB1caB1caB1caB1caB1caB1c",
        )
        .await;
    assert!(outcome.succeeded(), "{outcome:?}");
    let wt = Path::new(&env.worktree_path);
    assert_eq!(
        std::fs::read_to_string(wt.join("where")).unwrap().trim(),
        env.worktree_path
    );
    let vars = std::fs::read_to_string(wt.join("vars")).unwrap();
    let names: Vec<&str> = vars
        .lines()
        .filter_map(|l| l.split('=').next())
        .filter(|n| !["PWD", "SHLVL", "_", "OLDPWD"].contains(n))
        .collect();
    assert_eq!(
        names,
        ["APP_ENV", "HOME", "LANG", "PATH", "TERM", "TMPDIR"],
        "{vars}"
    );
    assert!(
        vars.contains(&format!("HOME={}/home", env.root_path)),
        "{vars}"
    );
    assert!(
        vars.lines()
            .any(|l| l.starts_with("PATH=/opt/toolchains/node/bin:")),
        "toolchain bins come first on PATH: {vars}"
    );

    let log = std::fs::read_to_string(&outcome.log_path).unwrap();
    assert!(log.contains("token «secret:github_token»"), "{log}");
    assert!(!log.contains("ghp_aB1c"), "{log}");
    let mode = std::fs::metadata(&outcome.log_path)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn setup_secrets_reach_the_command_env_and_are_redacted_by_name_in_the_log() {
    let w = World::new();
    w.repos.fetch(&w.repo).await.unwrap();
    let mut env = w.prepare(8).await;
    env.secrets = vec![
        SecretVar {
            name: "NPM_TOKEN".into(),
            value: SecretText::new("npm-token-abcdef".into()),
            purposes: vec![SecretPurpose::Setup],
        },
        SecretVar {
            name: "TEST_API_KEY".into(),
            value: SecretText::new("session-only-value".into()),
            purposes: vec![SecretPurpose::Session],
        },
    ];
    let outcome = w
        .run(
            &env,
            0,
            "echo \"npm=$NPM_TOKEN api=${TEST_API_KEY-unset}\"; env | grep -c session-only-value || true",
        )
        .await;
    assert!(outcome.succeeded(), "{outcome:?}");
    let log = std::fs::read_to_string(&outcome.log_path).unwrap();
    assert!(log.contains("npm=«secret:NPM_TOKEN» api=unset"), "{log}");
    assert!(!log.contains("npm-token-abcdef"), "{log}");
    assert!(log.lines().any(|l| l.trim() == "0"), "{log}");
    assert!(
        outcome
            .output_tail
            .contains("npm=«secret:NPM_TOKEN» api=unset")
    );
    assert!(!outcome.output_tail.contains("npm-token-abcdef"));

    let gate = w
        .repos
        .run_recipe_command(
            &env,
            &RecipeCommand {
                phase: RecipePhase::Gate,
                index: 0,
                command: "echo gate $NPM_TOKEN; sleep 5".into(),
                run: Some("abcdef012345".into()),
                timeout_ms: Some(500),
            },
        )
        .await
        .unwrap();
    assert!(gate.timed_out, "{gate:?}");
    assert!(
        gate.log_path.ends_with("gate-abcdef012345-0.log"),
        "{}",
        gate.log_path
    );
    assert_eq!(gate.output_tail, "gate «secret:NPM_TOKEN»");
}

#[tokio::test]
async fn a_failing_or_slow_recipe_command_is_reported_and_killed() {
    let w = World::new();
    w.repos.fetch(&w.repo).await.unwrap();
    let env = w.prepare(7).await;
    let failed = w.run(&env, 1, "echo nope; exit 3").await;
    assert_eq!(failed.exit_code, Some(3));
    assert!(!failed.timed_out && !failed.succeeded());

    let started = std::time::Instant::now();
    let slow = w
        .run(&env, 2, "(sleep 30; touch survived) & sleep 30")
        .await;
    assert!(slow.timed_out, "{slow:?}");
    assert_eq!(slow.exit_code, None);
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!Path::new(&env.worktree_path).join("survived").exists());
}

fn commit_at(dir: &Path, file: &str, text: &str) -> String {
    std::fs::create_dir_all(dir.join(file).parent().unwrap()).unwrap();
    commit(dir, file, text)
}

#[tokio::test]
async fn changed_paths_are_the_clean_diff_from_the_merge_base_without_renames() {
    let w = World::new();
    let env = w.prepare(1).await;
    let wt = PathBuf::from(&env.worktree_path);
    let base = git(&wt, &["rev-parse", "HEAD"]);
    commit(&w.other, "NEWS.md", "moved on\n");
    git(&w.other, &["push", "-q", "origin", "main"]);

    commit_at(&wt, "services/api/a.rs", "fn a() {}\n");
    std::fs::create_dir_all(wt.join("docs")).unwrap();
    git(&wt, &["mv", "README.md", "docs/README.md"]);
    git(&wt, &["commit", "-q", "-m", "move"]);
    commit_at(&wt, "services/web/a.ts", "export {}\n");
    git(&wt, &["rm", "-q", "services/web/a.ts"]);
    git(&wt, &["commit", "-q", "-m", "revert"]);
    let head = git(&wt, &["rev-parse", "HEAD"]);

    let changed = w
        .repos
        .changed_paths(&w.repo, &base, &head, false)
        .await
        .unwrap();
    assert_eq!(
        changed.paths,
        ["README.md", "docs/README.md", "services/api/a.rs"]
    );
    assert!(!changed.truncated);
    let against_tip = w
        .repos
        .changed_paths(&w.repo, "refs/remotes/origin/main", &head, true)
        .await
        .unwrap();
    assert_eq!(
        against_tip, changed,
        "the base's own change is not the task's"
    );

    let missing = w
        .repos
        .changed_paths(&w.repo, &base, &"1".repeat(40), false)
        .await
        .unwrap_err();
    assert_eq!(
        (missing.code.as_str(), missing.retryable),
        ("commit_missing", false)
    );
    let invalid = w
        .repos
        .changed_paths(&w.repo, "-x", &head, false)
        .await
        .unwrap_err();
    assert_eq!(invalid.code, "git_argument_invalid");
}

#[tokio::test]
async fn changed_paths_fetch_a_head_pushed_from_outside_first() {
    let w = World::new();
    w.prepare(1).await;
    git(&w.other, &["checkout", "-q", "-b", "outside"]);
    let head = commit_at(&w.other, "services/web/x.ts", "x\n");
    git(&w.other, &["push", "-q", "origin", "outside"]);

    let unseen = w
        .repos
        .changed_paths(&w.repo, "refs/remotes/origin/main", &head, false)
        .await
        .unwrap_err();
    assert_eq!(unseen.code, "commit_missing");
    let changed = w
        .repos
        .changed_paths(&w.repo, "refs/remotes/origin/main", &head, true)
        .await
        .unwrap();
    assert_eq!(changed.paths, ["services/web/x.ts"]);
}

#[tokio::test]
async fn more_than_ten_thousand_changed_paths_are_cut_and_marked_truncated() {
    let w = World::new();
    let env = w.prepare(1).await;
    let wt = PathBuf::from(&env.worktree_path);
    let base = git(&wt, &["rev-parse", "HEAD"]);
    let many = wt.join("many");
    std::fs::create_dir_all(&many).unwrap();
    let total = saka_core::scope::MAX_CHANGED_PATHS + 1;
    for i in 0..total {
        std::fs::write(many.join(format!("{i:05}")), b"").unwrap();
    }
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-q", "-m", "many"]);
    let head = git(&wt, &["rev-parse", "HEAD"]);
    let changed = w
        .repos
        .changed_paths(&w.repo, &base, &head, false)
        .await
        .unwrap();
    assert!(changed.truncated);
    assert_eq!(changed.paths.len(), saka_core::scope::MAX_CHANGED_PATHS);
    assert_eq!(changed.paths[0], "many/00000");
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

fn docs_layout(w: &World) -> String {
    std::fs::create_dir_all(w.other.join("docs/specs")).unwrap();
    std::fs::create_dir_all(w.other.join("other")).unwrap();
    std::fs::write(w.other.join("other/x.md"), "other\n").unwrap();
    std::fs::write(w.other.join("docs/specs/a.md"), "pinned\n").unwrap();
    let pinned = commit(&w.other, "docs/README.md", "docs\n");
    commit(&w.other, "docs/specs/a.md", "later\n");
    git(&w.other, &["push", "-q", "origin", "main"]);
    pinned
}

#[tokio::test]
async fn the_docs_checkout_holds_only_the_project_folder_at_the_pinned_commit_and_is_read_only() {
    let w = World::new();
    let pinned = docs_layout(&w);
    let env = w.prepare(1).await;
    let docs = w.prepare_docs(&env, &pinned, "docs").await.unwrap();
    let root = PathBuf::from(&env.root_path);
    assert_eq!(PathBuf::from(&docs.worktree_path), root.join("docs"));
    assert_eq!(PathBuf::from(&docs.path), root.join("docs/docs"));
    assert_eq!(docs.base_sha, pinned);
    let dir = PathBuf::from(&docs.path);
    assert_eq!(
        std::fs::read_to_string(dir.join("specs/a.md")).unwrap(),
        "pinned\n"
    );
    assert!(!root.join("docs/other").exists());
    assert_eq!(git(&root.join("docs"), &["rev-parse", "HEAD"]), pinned);
    for file in ["specs/a.md", "README.md"] {
        assert_eq!(mode(&dir.join(file)), 0o444, "{file}");
    }
    for d in [&root.join("docs"), &dir, &dir.join("specs")] {
        assert_eq!(mode(d), 0o555, "{}", d.display());
    }
    assert!(std::fs::write(dir.join("specs/a.md"), "x").is_err());
    assert!(std::fs::write(dir.join("specs/new.md"), "x").is_err());

    std::fs::write(&w.log, "").unwrap();
    let again = w.prepare_docs(&env, &pinned, "docs").await.unwrap();
    assert_eq!(again, docs);
    assert!(!w.log().contains("worktree add"), "{}", w.log());

    let bare = w.repos.mirror_path(w.docs_repo().repo_id);
    w.repos
        .teardown_docs(env.environment_id, &w.docs_repo())
        .await
        .unwrap();
    assert!(!root.join("docs").exists());
    assert!(
        !git(&bare, &["worktree", "list"]).contains(&docs.worktree_path),
        "{}",
        git(&bare, &["worktree", "list"])
    );
    w.repos
        .teardown_docs(env.environment_id, &w.docs_repo())
        .await
        .unwrap();
    assert!(Path::new(&env.worktree_path).join(".git").exists());
    w.repos.teardown(&env).await.unwrap();
    assert!(!root.exists());
}

#[tokio::test]
async fn an_unfinished_docs_checkout_is_made_again_and_teardown_removes_it_with_the_environment() {
    let w = World::new();
    let pinned = docs_layout(&w);
    let env = w.prepare(1).await;
    let same_mirror = w
        .repos
        .prepare(&PrepareRequest {
            environment_id: EnvironmentId(Uuid::from_u128(9)),
            repo: w.docs_repo(),
            checkout: Checkout::Branch(BRANCH.into()),
            base_branch: "main".into(),
        })
        .await
        .unwrap();
    let docs = w.prepare_docs(&env, &pinned, "docs").await.unwrap();
    let code = PathBuf::from(&same_mirror.worktree_path);
    assert_eq!(git(&code, &["rev-parse", "--is-bare-repository"]), "false");
    assert_eq!(git(&code, &["status", "--porcelain"]), "");
    assert!(code.join("other/x.md").exists(), "no sparse patterns leak");
    let bare = w.repos.mirror_path(w.docs_repo().repo_id);
    assert_eq!(git(&bare, &["rev-parse", "--is-bare-repository"]), "true");
    std::fs::remove_file(Path::new(&env.root_path).join("docs-sha")).unwrap();
    let again = w.prepare_docs(&env, &pinned, "docs").await.unwrap();
    assert_eq!(again, docs);
    assert_eq!(mode(Path::new(&docs.path)), 0o555);
    w.repos
        .teardown_docs(env.environment_id, &w.docs_repo())
        .await
        .unwrap();
    w.repos.teardown(&env).await.unwrap();
    assert!(!Path::new(&env.root_path).exists());
}

#[tokio::test]
async fn a_docs_checkout_of_the_whole_repo_and_its_refusals() {
    let w = World::new();
    let pinned = docs_layout(&w);
    let env = w.prepare(1).await;
    let whole = w.prepare_docs(&env, &pinned, ".").await.unwrap();
    assert_eq!(whole.path, whole.worktree_path);
    let dir = PathBuf::from(&whole.path);
    assert_eq!(
        std::fs::read_to_string(dir.join("other/x.md")).unwrap(),
        "other\n"
    );
    assert!(dir.join("README.md").exists());
    w.repos
        .teardown_docs(env.environment_id, &w.docs_repo())
        .await
        .unwrap();

    std::fs::write(&w.log, "").unwrap();
    let missing = w
        .prepare_docs(&env, &"1".repeat(40), "docs")
        .await
        .unwrap_err();
    assert_eq!(missing.code, "docs_commit_missing");
    assert!(!missing.retryable);
    assert_eq!(missing.params["commit"], "1".repeat(40));
    assert!(w.log().contains("fetch"), "{}", w.log());

    for bad in ["", "../x", "/abs", "-x", "docs/"] {
        let refused = w.prepare_docs(&env, &pinned, bad).await.unwrap_err();
        assert_eq!(refused.code, "git_argument_invalid", "{bad:?}");
    }

    let many = w.other.join("docs/many");
    std::fs::create_dir_all(&many).unwrap();
    for i in 0..=saka_ports::DOCS_MAX_FILES {
        std::fs::write(many.join(format!("{i}.md")), "").unwrap();
    }
    let big = commit(&w.other, "docs/specs/a.md", "big\n");
    git(&w.other, &["push", "-q", "origin", "main"]);
    let too_large = w.prepare_docs(&env, &big, "docs").await.unwrap_err();
    assert_eq!(too_large.code, "docs_checkout_too_large");
    assert!(!too_large.retryable);
    assert_eq!(
        too_large.params["files"],
        saka_ports::DOCS_MAX_FILES + 1 + 2
    );
    assert_eq!(too_large.params["max_files"], 20_000);
    assert_eq!(too_large.params["max_bytes"], 268_435_456);
    assert!(!Path::new(&env.root_path).join("docs").exists());
    let other = w.prepare_docs(&env, &big, "other").await.unwrap();
    assert!(Path::new(&other.path).join("x.md").exists());
}

#[tokio::test]
async fn a_writable_docs_checkout_is_a_branch_whose_commits_are_inspected_pushed_and_resumed() {
    let w = World::new();
    let pinned = docs_layout(&w);
    let env = w.prepare(1).await;
    let docs = w.prepare_writable_docs(&env, &pinned).await;
    let wt = PathBuf::from(&docs.worktree_path);
    let branch = format!("{BRANCH}-docs");
    assert_eq!(git(&wt, &["branch", "--show-current"]), branch);
    assert_eq!(docs.base_sha, pinned);
    assert!(!wt.join("other").exists());
    assert_eq!(mode(Path::new(&docs.path)) & 0o200, 0o200);

    let state = w
        .repos
        .inspect_docs(env.environment_id, &pinned)
        .await
        .unwrap();
    assert_eq!((state.dirty, state.commits_ahead), (false, 0));
    std::fs::write(Path::new(&docs.path).join("api.md"), "api\n").unwrap();
    assert!(
        w.repos
            .inspect_docs(env.environment_id, &pinned)
            .await
            .unwrap()
            .dirty
    );
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-q", "-m", "docs: api"]);
    let head = git(&wt, &["rev-parse", "HEAD"]);
    let state = w
        .repos
        .inspect_docs(env.environment_id, &pinned)
        .await
        .unwrap();
    assert_eq!(
        (state.dirty, state.commits_ahead, state.head_sha.as_str()),
        (false, 1, head.as_str())
    );
    let changed = w
        .repos
        .changed_paths(&w.docs_repo(), &pinned, &head, false)
        .await
        .unwrap();
    assert_eq!(changed.paths, ["docs/api.md"]);

    let mut writable = env.clone();
    writable.extra_dirs = vec![ExtraDir {
        path: docs.path.clone(),
        writable: true,
    }];
    let file = format!("{}/api.md", docs.path);
    assert!(w.repos.resolve_path(&writable, "", &file).inside_worktree);
    writable.extra_dirs[0].writable = false;
    assert!(!w.repos.resolve_path(&writable, "", &file).inside_worktree);

    let pushed = w
        .repos
        .push(&PushRequest {
            repo: w.docs_repo(),
            branch: branch.clone(),
            local_sha: head.clone(),
            expected_remote_sha: None,
        })
        .await
        .unwrap();
    assert_eq!(pushed, PushOutcome::Pushed { sha: head.clone() });
    assert_eq!(w.remote_branch(&branch), Some(head.clone()));

    w.repos
        .teardown_docs(env.environment_id, &w.docs_repo())
        .await
        .unwrap();
    w.repos.teardown(&env).await.unwrap();
    let bare = w.repos.mirror_path(w.docs_repo().repo_id);
    assert_eq!(git(&bare, &["branch", "--list", &branch]), "");

    let again = w.prepare(2).await;
    let resumed = w.prepare_writable_docs(&again, &head).await;
    let wt = PathBuf::from(&resumed.worktree_path);
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&wt, &["branch", "--show-current"]), branch);
    assert!(Path::new(&resumed.path).join("api.md").exists());
}

#[tokio::test]
async fn a_branch_tip_is_fetched_first_and_a_directory_lists_only_its_own_files() {
    let w = World::new();
    assert_eq!(
        w.repos.branch_tip(&w.repo, "develop").await.unwrap(),
        None,
        "a branch the remote does not have has no tip"
    );
    std::fs::create_dir_all(w.other.join(".github/workflows/nested")).unwrap();
    std::fs::write(w.other.join(".github/workflows/ci.yml"), "on: push\n").unwrap();
    std::fs::write(w.other.join(".github/workflows/nested/x.yml"), "on: push\n").unwrap();
    std::fs::write(w.other.join(".nvmrc"), "22\n").unwrap();
    std::os::unix::fs::symlink(".nvmrc", w.other.join(".node-version")).unwrap();
    let head = commit(&w.other, "go.mod", "module x\n\ngo 1.26.1\n");
    git(&w.other, &["push", "-q", "origin", "main:develop"]);

    let tip = w.repos.branch_tip(&w.repo, "develop").await.unwrap();
    assert_eq!(
        tip.as_deref(),
        Some(head.as_str()),
        "fetched before reading"
    );
    let mut root = w.repos.list_repo_dir(&w.repo, &head, "").await.unwrap();
    root.sort();
    assert_eq!(
        root,
        [".nvmrc", "README.md", "go.mod"],
        "no folders, no links"
    );
    assert_eq!(
        w.repos
            .list_repo_dir(&w.repo, &head, ".github/workflows")
            .await
            .unwrap(),
        [".github/workflows/ci.yml"]
    );
    assert!(
        w.repos
            .list_repo_dir(&w.repo, &head, "missing")
            .await
            .unwrap()
            .is_empty()
    );
    for (commit, dir) in [
        (head.as_str(), "../x"),
        (head.as_str(), "-r"),
        (head.as_str(), "/etc"),
        ("main", ""),
    ] {
        let e = w
            .repos
            .list_repo_dir(&w.repo, commit, dir)
            .await
            .unwrap_err();
        assert_eq!(e.code, "git_argument_invalid", "{commit} {dir}");
    }
    let e = w.repos.branch_tip(&w.repo, "-x").await.unwrap_err();
    assert_eq!(e.code, "git_argument_invalid");
}

fn outside_head(w: &World, branch: &str, pull_ref: &str) -> String {
    git(&w.other, &["checkout", "-q", "-b", branch, "main"]);
    let head = commit_at(&w.other, "src/fork.rs", "fn fork() {}\n");
    git(
        &w.other,
        &["push", "-q", "origin", &format!("{branch}:{pull_ref}")],
    );
    git(&w.other, &["checkout", "-q", "main"]);
    head
}

#[tokio::test]
async fn a_change_checkout_is_detached_at_a_head_only_the_pull_ref_serves() {
    let w = World::new();
    let head = outside_head(&w, "forkish", "refs/pull/1/head");
    let request = |head_ref: Option<&str>| PrepareRequest {
        environment_id: EnvironmentId(Uuid::from_u128(1)),
        repo: w.repo.clone(),
        checkout: Checkout::Change {
            head_sha: head.clone(),
            head_ref: head_ref.map(String::from),
        },
        base_branch: "main".into(),
    };

    let unseen = w.repos.prepare(&request(None)).await.unwrap_err();
    assert_eq!(
        (unseen.code.as_str(), unseen.retryable),
        ("commit_missing", false)
    );
    for bad in ["-x", "heads/main", "refs/pull/1/head:refs/heads/x"] {
        let refused = w.repos.prepare(&request(Some(bad))).await.unwrap_err();
        assert_eq!(refused.code, "git_argument_invalid", "{bad}");
    }
    let wrong = w
        .repos
        .prepare(&request(Some("refs/pull/2/head")))
        .await
        .unwrap_err();
    assert!(wrong.code == "git_failed" || wrong.code == "commit_missing");

    let prepared = w
        .repos
        .prepare(&request(Some("refs/pull/1/head")))
        .await
        .unwrap();
    let wt = PathBuf::from(&prepared.worktree_path);
    assert_eq!(prepared.base_sha, head);
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&wt, &["branch", "--show-current"]), "");
    assert!(wt.join("src/fork.rs").is_file());
}

#[tokio::test]
async fn a_change_checkout_of_a_branch_head_needs_no_change_ref() {
    let w = World::new();
    git(&w.other, &["checkout", "-q", "-b", "feature"]);
    let head = commit_at(&w.other, "feature.rs", "fn f() {}\n");
    git(&w.other, &["push", "-q", "origin", "feature"]);
    let env = w
        .prepare_with(
            1,
            Checkout::Change {
                head_sha: head.clone(),
                head_ref: None,
            },
        )
        .await;
    assert_eq!(
        git(&PathBuf::from(&env.worktree_path), &["rev-parse", "HEAD"]),
        head
    );
}

#[tokio::test]
async fn a_change_diff_is_the_clean_diff_from_the_merge_base_and_is_cut_at_the_limit() {
    let w = World::new();
    let head = outside_head(&w, "forkish", "refs/pull/1/head");
    git(&w.other, &["checkout", "-q", "forkish"]);
    commit_at(&w.other, "src/second.rs", &"let x = 1;\n".repeat(200));
    commit_at(&w.other, "src/third.rs", &"let y = 2;\n".repeat(200));
    git(
        &w.other,
        &["push", "-q", "-f", "origin", "forkish:refs/pull/1/head"],
    );
    let head = {
        let _ = head;
        git(&w.other, &["rev-parse", "HEAD"])
    };
    git(&w.other, &["checkout", "-q", "main"]);
    commit(&w.other, "NEWS.md", "moved on\n");
    git(&w.other, &["push", "-q", "origin", "main"]);
    w.prepare_with(
        1,
        Checkout::Change {
            head_sha: head.clone(),
            head_ref: Some("refs/pull/1/head".into()),
        },
    )
    .await;

    let whole = w
        .repos
        .change_diff(&w.repo, "main", &head, 1_000_000)
        .await
        .unwrap();
    assert!(!whole.truncated);
    assert_eq!(whole.omitted_files, 0);
    assert_eq!(
        whole.files,
        ["src/fork.rs", "src/second.rs", "src/third.rs"]
    );
    assert!(
        whole
            .text
            .contains("diff --git a/src/fork.rs b/src/fork.rs")
    );
    assert!(whole.text.contains("+fn fork() {}"));
    assert!(
        !whole.text.contains("NEWS.md"),
        "the base's own change is not the change's"
    );
    assert!(w.log().contains("--no-ext-diff"));
    assert!(w.log().contains("--no-textconv"));

    let cut = w
        .repos
        .change_diff(&w.repo, "main", &head, 400)
        .await
        .unwrap();
    assert!(cut.truncated);
    assert!(cut.text.len() <= 400 && cut.text.ends_with('\n'));
    assert_eq!(cut.files.len(), 3);
    let shown = cut.text.matches("diff --git ").count();
    assert!(cut.omitted_files >= 1 && cut.omitted_files == 3 - shown);

    let missing = w
        .repos
        .change_diff(&w.repo, "main", &"1".repeat(40), 100)
        .await
        .unwrap_err();
    assert_eq!(missing.code, "commit_missing");
    let invalid = w
        .repos
        .change_diff(&w.repo, "-x", &head, 100)
        .await
        .unwrap_err();
    assert_eq!(invalid.code, "git_argument_invalid");
}
