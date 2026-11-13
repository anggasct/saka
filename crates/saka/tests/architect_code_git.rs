#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use saka_core::ids::{EnvironmentId, RepoId};
use saka_env::{DirectSupervisor, Git, GitRepos, RecipeRunner, ReposConfig};
use saka_ports::{
    Checkout, CodeCheckout, CodeRequest, EnvRef, Environments, GitAccess, GitRemote, PortResult,
    PrepareRequest, RepoRef, UnitLimits,
};
use uuid::Uuid;

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

struct Remotes(BTreeMap<RepoId, String>);

#[async_trait]
impl GitAccess for Remotes {
    async fn remote(&self, repo: &RepoRef) -> PortResult<GitRemote> {
        Ok(GitRemote {
            url: self.0[&repo.repo_id].clone(),
            config: Vec::new(),
        })
    }

    async fn refused(&self, _repo: &RepoRef) {}
}

struct Project {
    work: PathBuf,
    repo: RepoRef,
}

fn project(root: &Path, id: u128, name: &str, file: &str) -> (Project, String) {
    let remote = root.join(format!("{name}-remote.git"));
    let work = root.join(format!("{name}-work"));
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    git(
        root,
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            &format!("{name}-work"),
        ],
    );
    git(&work, &["checkout", "-q", "-b", "main"]);
    std::fs::create_dir_all(work.join(file).parent().unwrap()).unwrap();
    std::fs::write(work.join(file), format!("{name}\n")).unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "seed"]);
    git(&work, &["push", "-q", "origin", "main"]);
    let repo = RepoRef {
        repo_id: RepoId(Uuid::from_u128(id)),
        integration_id: "github".into(),
        full_name: format!("acme/{name}"),
        url: format!("https://scm.invalid/acme/{name}"),
    };
    (
        Project { work, repo },
        remote.to_string_lossy().into_owned(),
    )
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

#[tokio::test]
async fn an_architect_environment_holds_the_docs_worktree_and_a_read_only_checkout_per_code_repo() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (docs, docs_url) = project(&root, 1, "docs", "docs/README.md");
    let (shop, shop_url) = project(&root, 2, "shop", "src/lib.rs");
    let (web, web_url) = project(&root, 3, "web", "app/index.ts");
    std::fs::write(shop.work.join("src/more.rs"), "more\n").unwrap();
    git(&shop.work, &["add", "-A"]);
    git(&shop.work, &["commit", "-q", "-m", "second"]);
    git(&shop.work, &["push", "-q", "origin", "main"]);

    let remotes = Remotes(BTreeMap::from([
        (docs.repo.repo_id, docs_url),
        (shop.repo.repo_id, shop_url),
        (web.repo.repo_id, web_url),
    ]));
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
        Arc::new(remotes),
    );

    let id = EnvironmentId(Uuid::from_u128(40));
    let prepared = repos
        .prepare(&PrepareRequest {
            environment_id: id,
            repo: docs.repo.clone(),
            checkout: Checkout::Detached,
            base_branch: "main".into(),
        })
        .await
        .unwrap();
    let env = EnvRef {
        environment_id: id,
        repo: docs.repo.clone(),
        branch: None,
        root_path: prepared.root_path.clone(),
        worktree_path: prepared.worktree_path.clone(),
        vars: BTreeMap::new(),
        path_prefix: Vec::new(),
        secrets: Vec::new(),
        extra_dirs: Vec::new(),
    };
    let mut paths = Vec::new();
    for (project, name) in [(&shop, "shop"), (&web, "web")] {
        let outcome = repos
            .prepare_code(&CodeRequest {
                environment_id: id,
                repo: project.repo.clone(),
                name: name.into(),
                base_branch: "main".into(),
            })
            .await
            .unwrap();
        let CodeCheckout::Ready(code) = outcome else {
            panic!("{outcome:?}");
        };
        let tip = git(&project.work, &["rev-parse", "HEAD"]);
        assert_eq!(code.base_sha, tip);
        let dir = PathBuf::from(&code.path);
        assert_eq!(dir, Path::new(&prepared.root_path).join("code").join(name));
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]), tip);
        assert_eq!(git(&dir, &["status", "--porcelain"]), "");
        assert_eq!(mode(&dir), 0o555);
        assert!(std::fs::write(dir.join("planted"), "x").is_err());
        paths.push(dir);
    }
    assert_eq!(
        std::fs::read_to_string(paths[0].join("src/lib.rs")).unwrap(),
        "shop\n"
    );
    assert!(paths[0].join("src/more.rs").exists());
    assert_eq!(
        std::fs::read_to_string(paths[1].join("app/index.ts")).unwrap(),
        "web\n"
    );
    assert!(
        Path::new(&prepared.worktree_path)
            .join("docs/README.md")
            .exists()
    );
    assert!(!Path::new(&prepared.worktree_path).join("src").exists());

    for (project, name) in [(&shop, "shop"), (&web, "web")] {
        repos.teardown_code(id, &project.repo, name).await.unwrap();
        let bare = repos.mirror_path(project.repo.repo_id);
        assert!(
            !git(&bare, &["worktree", "list"]).contains("/code/"),
            "{}",
            git(&bare, &["worktree", "list"])
        );
    }
    assert!(paths.iter().all(|p| !p.exists()));
    repos.teardown(&env).await.unwrap();
    assert!(!Path::new(&prepared.root_path).exists());
}
