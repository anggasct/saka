#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use saka_core::ids::RepoId;
use saka_core::time::Timestamp;
use saka_docs::GitDocs;
use saka_env::{DirectSupervisor, Git, GitRepos, RecipeRunner, ReposConfig};
use saka_ports::{
    CommitIdentity, DocBlob, DocCommit, DocExpect, DocSource, DocWrite, GitAccess, GitRemote,
    PortResult, RepoRef, UnitLimits,
};
use uuid::Uuid;

const SECRET: &str = "AUTHORIZATION: basic eC1hY2Nlc3MtdG9rZW46Z2hzX2RvY3M=";
const HEADER_KEY: &str = "http.https://scm.invalid/.extraheader";
const AT: Timestamp = Timestamp(1_790_000_000_000);

const CORPUS: [(&str, &[u8]); 10] = [
    ("lf", b"# A\nline\n"),
    ("crlf", b"# A\r\nline\r\n"),
    ("mixed", b"# A\r\nline\nlast\r\n"),
    ("lone-cr", b"# A\rline\r"),
    ("bom", b"\xef\xbb\xbf# A\nline\n"),
    ("no-newline", b"# A\nlast line"),
    ("empty", b""),
    ("unicode", "# Ünïcödé 🚀\ne\u{301} combining\n".as_bytes()),
    ("spaces", b"\tindented\ntrailing   \n\n\n"),
    ("latin1", b"caf\xe9\n"),
];

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Other")
        .env("GIT_AUTHOR_EMAIL", "other@saka.invalid")
        .env("GIT_COMMITTER_NAME", "Other")
        .env("GIT_COMMITTER_EMAIL", "other@saka.invalid")
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

fn git_bytes(dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
}

fn install(source: &Path, target: &Path) {
    let installed = Command::new("install")
        .arg("-m")
        .arg("755")
        .arg(source)
        .arg(target)
        .status()
        .unwrap();
    assert!(installed.success());
}

struct StaticAccess {
    url: String,
}

#[async_trait]
impl GitAccess for StaticAccess {
    async fn remote(&self, _repo: &RepoRef) -> PortResult<GitRemote> {
        Ok(GitRemote {
            url: self.url.clone(),
            config: vec![(HEADER_KEY.to_string(), SECRET.to_string())],
        })
    }

    async fn refused(&self, _repo: &RepoRef) {}
}

struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    remote: PathBuf,
    other: PathBuf,
    mirror: PathBuf,
    log: PathBuf,
    docs: GitDocs,
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
        std::fs::create_dir_all(other.join("docs/specs")).unwrap();
        std::fs::create_dir_all(other.join("src")).unwrap();
        for (path, bytes) in [
            ("docs/a.md", b"# A\n".as_slice()),
            ("docs/specs/b.MD", b"# B\n"),
            ("docs/x.markdown", b"# X\n"),
            ("docs/c.txt", b"text\n"),
            ("src/d.md", b"# D\n"),
            ("docs/run.md", b"# Run\n"),
        ] {
            std::fs::write(other.join(path), bytes).unwrap();
        }
        std::fs::write(other.join("docs/big.md"), vec![b'a'; 2 * 1024 * 1024]).unwrap();
        std::os::unix::fs::symlink("a.md", other.join("docs/l.md")).unwrap();
        for (name, bytes) in CORPUS {
            std::fs::write(other.join(format!("docs/{name}.md")), bytes).unwrap();
        }
        git(&other, &["add", "-A"]);
        git(&other, &["update-index", "--chmod=+x", "docs/run.md"]);
        git(&other, &["commit", "-q", "-m", "seed"]);
        git(&other, &["push", "-q", "origin", "main"]);

        let log = root.join("git.log");
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
                 case \" $* \" in *' push '*) if [ -f '{down}' ]; then \
                 echo 'fatal: unable to access remote' >&2; exit 128; fi; if [ -f '{marker}' ]; then rm '{marker}'; \
                 env -i PATH=/usr/bin:/bin GIT_CONFIG_GLOBAL=/dev/null '{real}' -C '{other}' push -q origin main; fi;; esac\n\
                 exec '{real}' \"$@\"\n",
                log = log.display(),
                marker = root.join("compete").display(),
                down = root.join("down").display(),
                other = other.display(),
            ),
        )
        .unwrap();
        let wrapper = root.join("git-wrapper");
        install(&source, &wrapper);

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
                    path: vec!["/usr/bin".into(), "/bin".into()],
                },
            },
            Arc::new(StaticAccess {
                url: remote.to_string_lossy().into_owned(),
            }),
        );
        let repo = RepoRef {
            repo_id: RepoId(Uuid::from_u128(7)),
            integration_id: "github".into(),
            full_name: "acme/app".into(),
            url: "https://scm.invalid/acme/app".into(),
        };
        let mirror = root.join(format!("data/repos/{}.git", repo.repo_id));
        World {
            _dir: dir,
            root,
            remote,
            other,
            mirror,
            log,
            docs: GitDocs::new(Arc::new(repos)),
            repo,
        }
    }

    fn tip(&self) -> String {
        git(&self.remote, &["rev-parse", "refs/heads/main"])
    }

    fn blob_at(&self, path: &str) -> String {
        git(&self.remote, &["rev-parse", &format!("main:{path}")])
    }

    fn request(&self, path: &str, bytes: &[u8], expect: DocExpect) -> DocCommit {
        DocCommit {
            repo: self.repo.clone(),
            branch: "main".into(),
            path: path.into(),
            bytes: bytes.to_vec(),
            expect,
            author: CommitIdentity {
                name: "Saka Agent".into(),
                email: "agent@saka.invalid".into(),
            },
            at: AT,
        }
    }

    async fn write(&self, path: &str, bytes: &[u8], expect: DocExpect) -> DocWrite {
        self.docs
            .commit_file(&self.request(path, bytes, expect))
            .await
            .unwrap()
    }

    fn other_commit(&self, path: &str, bytes: &[u8]) -> String {
        git(&self.other, &["pull", "-q", "--ff-only", "origin", "main"]);
        std::fs::write(self.other.join(path), bytes).unwrap();
        git(&self.other, &["add", "-A"]);
        git(&self.other, &["commit", "-q", "-m", "other"]);
        git(&self.other, &["rev-parse", "HEAD"])
    }

    fn compete_on_next_push(&self) {
        std::fs::write(self.root.join("compete"), b"").unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tree_lists_regular_markdown_files_with_the_blobs_git_has() {
    let w = World::new();
    let tree = w.docs.tree(&w.repo, "main", "docs", 5000).await.unwrap();
    assert_eq!(tree.commit, w.tip());
    assert!(!tree.truncated);
    let paths: Vec<&str> = tree.files.iter().map(|f| f.path.as_str()).collect();
    let mut expected = vec![
        "docs/a.md".to_string(),
        "docs/big.md".to_string(),
        "docs/run.md".to_string(),
        "docs/specs/b.MD".to_string(),
        "docs/x.markdown".to_string(),
    ];
    expected.extend(CORPUS.iter().map(|(name, _)| format!("docs/{name}.md")));
    expected.sort();
    assert_eq!(paths, expected);
    for file in &tree.files {
        assert_eq!(file.blob, w.blob_at(&file.path), "{}", file.path);
    }
    let big = tree.files.iter().find(|f| f.path == "docs/big.md").unwrap();
    assert_eq!(big.bytes, 2 * 1024 * 1024);

    let whole = w.docs.tree(&w.repo, "main", ".", 5000).await.unwrap();
    assert!(whole.files.iter().any(|f| f.path == "src/d.md"));
    assert!(!whole.files.iter().any(|f| f.path.ends_with(".txt")));

    let many = w.other.join("docs/many");
    std::fs::create_dir_all(&many).unwrap();
    for n in 0..5001 {
        std::fs::write(many.join(format!("{n:04}.md")), format!("{n}\n")).unwrap();
    }
    git(&w.other, &["add", "-A"]);
    git(&w.other, &["commit", "-q", "-m", "many"]);
    git(&w.other, &["push", "-q", "origin", "main"]);
    let cut = w.docs.tree(&w.repo, "main", "docs", 5000).await.unwrap();
    assert!(cut.truncated);
    assert_eq!(cut.files.len(), 5000);
}

#[tokio::test(flavor = "multi_thread")]
async fn files_open_byte_exact_and_links_and_large_files_are_told_apart() {
    let w = World::new();
    for (name, bytes) in CORPUS {
        let path = format!("docs/{name}.md");
        let opened = w
            .docs
            .open(&w.repo, "main", &path, 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(opened.commit, w.tip());
        assert_eq!(
            opened.blob,
            DocBlob::Bytes {
                blob: w.blob_at(&path),
                bytes: bytes.to_vec(),
            },
            "{name}"
        );
    }
    let open = |path: &'static str| w.docs.open(&w.repo, "main", path, 1024 * 1024);
    assert_eq!(open("docs/l.md").await.unwrap().blob, DocBlob::NotRegular);
    assert_eq!(open("docs/none.md").await.unwrap().blob, DocBlob::Missing);
    assert_eq!(
        open("docs/big.md").await.unwrap().blob,
        DocBlob::TooLarge {
            blob: w.blob_at("docs/big.md"),
            bytes: 2 * 1024 * 1024,
        }
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_save_is_one_commit_on_the_tip_with_the_exact_bytes() {
    let w = World::new();
    for (name, bytes) in CORPUS {
        let path = format!("docs/{name}.md");
        let tip = w.tip();
        let mut next = bytes.to_vec();
        next.extend_from_slice(b"edited\r\n");
        let out = w
            .write(&path, &next, DocExpect::Blob(w.blob_at(&path)))
            .await;
        let DocWrite::Committed {
            commit,
            parent,
            blob,
            created,
            attempts,
        } = out
        else {
            panic!("{name}: {out:?}");
        };
        assert_eq!(
            (parent.as_str(), created, attempts),
            (tip.as_str(), false, 1)
        );
        assert_eq!(commit, w.tip());
        assert_eq!(blob, w.blob_at(&path));
        assert_eq!(
            git_bytes(&w.remote, &["cat-file", "blob", &format!("main:{path}")]),
            next,
            "{name}"
        );
        assert_eq!(git(&w.remote, &["rev-parse", &format!("{commit}^@")]), tip);
        assert_eq!(
            git(
                &w.remote,
                &["diff-tree", "--no-commit-id", "--name-only", "-r", &commit]
            ),
            path
        );
        assert_eq!(
            git(&w.remote, &["log", "-1", "--format=%s", &commit]),
            format!("docs: update {path}")
        );
        assert_eq!(
            git(&w.mirror, &["rev-parse", "refs/remotes/origin/main"]),
            commit
        );
    }

    let tip = w.tip();
    let out = w
        .write("docs/new/page.md", b"# New\n", DocExpect::Absent)
        .await;
    let DocWrite::Committed {
        commit, created, ..
    } = out
    else {
        panic!("{out:?}");
    };
    assert!(created);
    assert_eq!(
        git(
            &w.remote,
            &[
                "log",
                "-1",
                "--format=%s|%an|%ae|%at|%cn|%ce|%ct|%P",
                &commit
            ]
        ),
        format!(
            "docs: add docs/new/page.md|Saka Agent|agent@saka.invalid|1790000000|Saka Agent|agent@saka.invalid|1790000000|{tip}"
        )
    );
    assert_eq!(
        git(&w.remote, &["ls-tree", &commit, "docs/new/page.md"])
            .split_whitespace()
            .next(),
        Some("100644")
    );

    let out = w
        .write("docs/run.md", b"# Run fast\n", DocExpect::Exists)
        .await;
    assert!(
        matches!(out, DocWrite::Committed { created: false, .. }),
        "{out:?}"
    );
    assert_eq!(
        git(&w.remote, &["ls-tree", "main", "docs/run.md"])
            .split_whitespace()
            .next(),
        Some("100755")
    );

    let log = std::fs::read_to_string(&w.log).unwrap();
    assert!(
        log.lines()
            .filter(|l| l.starts_with("argv"))
            .all(|l| !l.contains(SECRET)),
        "{log}"
    );
    assert!(
        log.lines()
            .any(|l| l.starts_with("env GIT_CONFIG_VALUE_0=") && l.contains(SECRET))
    );
    assert!(
        log.lines()
            .any(|l| l.starts_with("argv") && l.contains(" push "))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn preconditions_that_do_not_hold_change_nothing() {
    let w = World::new();
    let tip = w.tip();
    let a = w.blob_at("docs/a.md");
    let stale = w
        .write("docs/a.md", b"# Mine\n", DocExpect::Blob("0".repeat(40)))
        .await;
    assert_eq!(
        stale,
        DocWrite::Stale {
            commit: tip.clone(),
            blob: Some(a.clone()),
        }
    );
    let exists = w.write("docs/a.md", b"# Mine\n", DocExpect::Absent).await;
    assert_eq!(
        exists,
        DocWrite::Stale {
            commit: tip.clone(),
            blob: Some(a.clone()),
        }
    );
    let missing = w
        .write("docs/none.md", b"# Mine\n", DocExpect::Blob(a.clone()))
        .await;
    assert_eq!(
        missing,
        DocWrite::Stale {
            commit: tip.clone(),
            blob: None,
        }
    );
    let same = w
        .write("docs/a.md", b"# A\n", DocExpect::Blob("0".repeat(40)))
        .await;
    assert_eq!(
        same,
        DocWrite::Unchanged {
            commit: tip.clone(),
            blob: a.clone(),
        }
    );
    let conflict = w.write("docs/a.md/b.md", b"# B\n", DocExpect::Absent).await;
    assert_eq!(
        conflict,
        DocWrite::PathConflict {
            path: "docs/a.md".into(),
        }
    );
    let folder = w.write("docs/specs", b"# S\n", DocExpect::Absent).await;
    assert_eq!(
        folder,
        DocWrite::PathConflict {
            path: "docs/specs".into(),
        }
    );
    let link = w.write("docs/l.md", b"# L\n", DocExpect::Exists).await;
    assert_eq!(link, DocWrite::NotRegular);
    assert_eq!(w.tip(), tip);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_to_another_file_in_between_is_retried_on_the_new_tip() {
    let w = World::new();
    let a = w.blob_at("docs/a.md");
    let theirs = w.other_commit("docs/x.markdown", b"# X theirs\n");
    w.compete_on_next_push();
    let out = w
        .write("docs/a.md", b"# A mine\n", DocExpect::Blob(a))
        .await;
    let DocWrite::Committed {
        commit,
        parent,
        attempts,
        ..
    } = out
    else {
        panic!("{out:?}");
    };
    assert_eq!((parent.as_str(), attempts), (theirs.as_str(), 2));
    assert_eq!(w.tip(), commit);
    assert_eq!(
        git_bytes(&w.remote, &["cat-file", "blob", "main:docs/x.markdown"]),
        b"# X theirs\n"
    );
    assert_eq!(
        git_bytes(&w.remote, &["cat-file", "blob", "main:docs/a.md"]),
        b"# A mine\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_to_the_same_file_in_between_makes_the_save_stale() {
    let w = World::new();
    let a = w.blob_at("docs/a.md");
    let theirs = w.other_commit("docs/a.md", b"# A theirs\n");
    w.compete_on_next_push();
    let out = w
        .write("docs/a.md", b"# A mine\n", DocExpect::Blob(a))
        .await;
    assert_eq!(
        out,
        DocWrite::Stale {
            commit: theirs.clone(),
            blob: Some(w.blob_at("docs/a.md")),
        }
    );
    assert_eq!(w.tip(), theirs);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_that_refuses_the_push_leaves_both_refs_alone() {
    let w = World::new();
    let hook = w.root.join("pre-receive.sh");
    std::fs::write(&hook, "#!/bin/sh\necho 'protected branch' >&2\nexit 1\n").unwrap();
    install(&hook, &w.remote.join("hooks/pre-receive"));
    let tip = w.tip();
    let a = w.blob_at("docs/a.md");
    let out = w
        .write("docs/a.md", b"# A mine\n", DocExpect::Blob(a))
        .await;
    assert_eq!(out, DocWrite::Rejected);
    assert_eq!(w.tip(), tip);
    assert_eq!(
        git(&w.mirror, &["rev-parse", "refs/remotes/origin/main"]),
        tip
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_push_that_never_reaches_the_remote_is_an_error_not_a_refusal() {
    let w = World::new();
    std::fs::write(w.root.join("down"), b"").unwrap();
    let tip = w.tip();
    let a = w.blob_at("docs/a.md");
    let err = w
        .docs
        .commit_file(&w.request("docs/a.md", b"# A mine\n", DocExpect::Blob(a)))
        .await
        .unwrap_err();
    assert_eq!(err.code, "git_failed", "{err:?}");
    assert!(err.retryable);
    assert_eq!(w.tip(), tip);
}
