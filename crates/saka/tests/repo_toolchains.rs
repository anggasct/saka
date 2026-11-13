#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use saka_core::diagnostics::Report;
use saka_core::ids::ProjectId;
use saka_core::policy::Policy;
use saka_core::vocab::{CheckId, Tool};
use saka_engine::{
    DiagnosticFilter, DiagnosticLimits, Engine, EngineConfig, Integration, NewBinding, NewProject,
    NewRepo, Ports,
};
use saka_env::{
    DirectSupervisor, Git, GitRepos, Platform, RecipeRunner, ReposConfig, ToolchainConfig,
    ToolchainInstaller,
};
use saka_ports::{Clock, GitAccess, GitRemote, PortError, PortResult, RepoRef, UnitLimits};
use saka_store::Store;
use saka_testkit::{AgentScript, CiScript, FakeClock, FakeIds, FakeReleases, FakeWorld, SimConfig};
use serde_json::{Value, json};

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

struct LocalRemotes(BTreeMap<String, PathBuf>);

#[async_trait]
impl GitAccess for LocalRemotes {
    async fn remote(&self, repo: &RepoRef) -> PortResult<GitRemote> {
        let path = self.0.get(&repo.full_name).ok_or_else(|| {
            PortError::permanent("scm_repo_inaccessible", json!({ "status": 404 }))
        })?;
        Ok(GitRemote {
            url: path.to_string_lossy().into_owned(),
            config: Vec::new(),
        })
    }

    async fn refused(&self, _repo: &RepoRef) {}
}

struct Remote {
    bare: PathBuf,
    work: PathBuf,
}

impl Remote {
    fn new(root: &Path, name: &str) -> Remote {
        let bare = root.join(format!("{name}.git"));
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare", "-b", "main"]);
        git(root, &["clone", "-q", bare.to_str().unwrap(), name]);
        let work = root.join(name);
        git(&work, &["checkout", "-q", "-b", "main"]);
        Remote { bare, work }
    }

    fn push(&self, branch: &str, files: &[(&str, &str)]) {
        for (path, text) in files {
            let file = self.work.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        git(&self.work, &["add", "-A"]);
        git(&self.work, &["commit", "-q", "-m", branch]);
        git(
            &self.work,
            &["push", "-q", "origin", &format!("HEAD:{branch}")],
        );
    }
}

struct World {
    _dir: tempfile::TempDir,
    engine: Engine,
    shop: ProjectId,
}

fn new_repo(full_name: &str) -> NewRepo {
    NewRepo {
        repo_id: None,
        integration_id: "github".into(),
        provider: "github".into(),
        full_name: full_name.into(),
        url: format!("https://github.com/{full_name}"),
        default_branch: "main".into(),
    }
}

fn binding(full_name: &str, name: &str, base_branch: &str, is_default: bool) -> NewBinding {
    NewBinding {
        repo: new_repo(full_name),
        name: name.into(),
        is_default,
        base_branch: base_branch.into(),
        path_scope: vec!["**".into()],
        autonomy: None,
    }
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut remotes = BTreeMap::new();

        let web = Remote::new(&root, "web");
        web.push("main", &[(".nvmrc", "24.1.0\n")]);
        web.push("develop", &[(".nvmrc", "22\n")]);
        let api = Remote::new(&root, "api");
        api.push("main", &[(".nvmrc", "22.11.0\n"), ("package.json", "{}\n")]);
        let svc = Remote::new(&root, "svc");
        svc.push(
            "main",
            &[
                ("go.mod", "module example.com/svc\n\ngo 1.26.1\n"),
                ("Dockerfile", "FROM golang:1.24 AS build\n"),
            ],
        );
        let core = Remote::new(&root, "core");
        core.push(
            "main",
            &[("rust-toolchain.toml", "[toolchain]\nchannel = \"1.80.0\"\n")],
        );
        for (name, remote) in [("web", &web), ("api", &api), ("svc", &svc), ("core", &core)] {
            remotes.insert(format!("acme/{name}"), remote.bare.clone());
        }

        let agent_bin = root.join("agent-bin");
        std::fs::create_dir_all(&agent_bin).unwrap();
        let source = root.join("rustc.sh");
        std::fs::write(
            &source,
            "#!/bin/sh\necho 'rustc 1.99.1 (4d215e2 2026-09-01)'\n",
        )
        .unwrap();
        install(&source, &agent_bin.join("rustc"));

        let real_git = String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout)
            .unwrap()
            .trim()
            .to_string();
        let repos = GitRepos::new(
            ReposConfig {
                repos_dir: root.join("data/repos"),
                envs_dir: root.join("data/envs"),
                git: Git::new(Path::new(&real_git), "/usr/local/bin:/usr/bin:/bin"),
                recipe: RecipeRunner {
                    supervisor: Arc::new(DirectSupervisor::new(root.join("data/units"))),
                    limits: UnitLimits {
                        memory_max: "1G".into(),
                        cpu_quota: "100%".into(),
                        tasks_max: 64,
                    },
                    timeout: Duration::from_secs(2),
                    logs_dir: root.join("data/logs"),
                    path: vec!["/usr/bin".into(), "/bin".into()],
                },
            },
            Arc::new(LocalRemotes(remotes)),
        );
        let toolchains = ToolchainInstaller::new(ToolchainConfig {
            root: root.join("data/toolchains"),
            caches: root.join("data/caches"),
            go_dl_url: "http://127.0.0.1:9/dl".into(),
            node_dist_url: "http://127.0.0.1:9/dist".into(),
            npm_registry_url: "http://127.0.0.1:9/npm".into(),
            timeout: Duration::from_secs(1),
            user_agent: "saka-test".into(),
            platform: Platform::current(),
            host_path: vec![agent_bin.to_string_lossy().into_owned()],
        })
        .unwrap();
        let releases = FakeReleases::new().with(Tool::Node, &["22.10.0", "22.11.0", "24.1.0"]);

        let clock = Arc::new(FakeClock::new());
        let world = Arc::new(FakeWorld::new(
            clock.clone(),
            AgentScript::default(),
            CiScript::Pass,
        ));
        let store = Store::open(&root.join("saka.db"), &root.join("backups"), clock.now()).unwrap();
        let sim = SimConfig::default();
        let engine = Engine::new(
            Arc::new(store),
            Ports {
                clock,
                ids: Arc::new(FakeIds::default()),
                entropy: Arc::new(saka_testkit::FakeEntropy::default()),
                env: Arc::new(repos),
                scm: world.clone(),
                intake: world.clone(),
                agents: world.clone(),
                docs: world.clone(),
                toolchains: Arc::new(toolchains),
                secrets: world.clone(),
                supervisor: world.clone(),
                releases: Arc::new(releases),
                webhooks: Arc::new(saka_testkit::FakeHooks::default()),
                notifier: Arc::new(saka_testkit::FakeHooks::default()),
                host: Arc::new(saka_testkit::FakeHooks::default()),
            },
            EngineConfig {
                loop_cfg: sim.loop_cfg,
                max_active_tasks: sim.max_active_tasks,
                policy: Policy::compile(&[]).unwrap(),
                worker_id: "test".into(),
                integrations: vec![Integration {
                    id: "github".into(),
                    provider: "github".into(),
                    web_url: "https://github.com".into(),
                }],
                env: sim.env,
                git_identity: saka_core::authorship::GitIdentity {
                    name: "Saka Agent".into(),
                    email: "agent@saka.invalid".into(),
                },
                draft: sim.draft,
                architect_model: None,
                config_poll_secs: 0,
                diagnostics: DiagnosticLimits::default(),
                hooks: Vec::new(),
                environments: Vec::new(),
                incident_sources: Vec::new(),
                auth_enabled: false,
                workflow_policy: saka_core::workflow::WorkflowPolicy::default(),
                public_url: None,
            },
        );
        let cause = engine.cause("human:operator:local");
        let register = |name: &str, bindings: Vec<NewBinding>| {
            engine
                .register_project(
                    &NewProject {
                        name: name.into(),
                        workspace: new_repo("acme/web"),
                        project_path: name.into(),
                        base_branch: "main".into(),
                        config_commit: "0".repeat(40),
                        config_sha256: saka_testkit::ZERO_SHA256.into(),
                        workflow: saka_core::workflow::Workflow::default(),
                        hook_skip: Vec::new(),
                        verify: Default::default(),
                        bindings,
                    },
                    &cause,
                )
                .unwrap()
                .0
        };
        let shop = register(
            "shop",
            vec![
                binding("acme/web", "web", "develop", true),
                binding("acme/api", "api", "main", false),
            ],
        );
        register(
            "infra",
            vec![
                binding("acme/svc", "svc", "main", true),
                binding("acme/core", "core", "main", false),
            ],
        );
        World {
            _dir: dir,
            engine,
            shop,
        }
    }

    async fn report(&self, project_id: Option<ProjectId>) -> Report {
        let filter = DiagnosticFilter {
            checks: Some(BTreeSet::from([CheckId::ToolchainRepo])),
            project_id,
        };
        self.engine.diagnostics(&filter, "test").await.unwrap()
    }
}

type Seen = (String, String, String, Value, Option<(String, Value)>);

fn seen(report: &Report) -> Vec<Seen> {
    report
        .checks
        .iter()
        .map(|c| {
            (
                c.subject.label.clone(),
                c.finding.status.as_str().to_string(),
                c.finding.code.clone(),
                c.finding.params.clone(),
                c.finding
                    .fix
                    .as_ref()
                    .map(|f| (f.code.clone(), f.params.clone())),
            )
        })
        .collect()
}

fn row(label: &str, status: &str, code: &str, params: Value, fix: Option<(&str, Value)>) -> Seen {
    (
        label.into(),
        status.into(),
        code.into(),
        params,
        fix.map(|(c, p)| (c.into(), p)),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn repo_toolchains_are_read_from_each_bindings_base_branch_in_a_real_repo() {
    let w = World::new();
    let all = w.report(None).await;
    assert!(
        all.checks.iter().all(|c| c.id == CheckId::ToolchainRepo),
        "{:?}",
        all.checks
    );
    assert_eq!(
        seen(&all),
        [
            row(
                "infra/core",
                "warn",
                "host_toolchain_mismatch",
                json!({ "tool": "rust", "declared": "1.80.0", "found": "1.99.1" }),
                None,
            ),
            row(
                "infra/svc",
                "warn",
                "toolchain_drift",
                json!({ "tool": "go", "pinned": "1.26.1", "drift": [
                    { "source": "dockerfile", "file": "Dockerfile", "line": 1, "value": "1.24" },
                ] }),
                None,
            ),
            row(
                "shop/api",
                "pass",
                "toolchains_pinned",
                json!({ "toolchains": [{ "tool": "node", "version": "22.11.0", "installed": false }] }),
                None,
            ),
            row(
                "shop/web",
                "fail",
                "toolchain_unpinned",
                json!({ "tool": "node", "source": "nvmrc", "value": "22" }),
                Some((
                    "write_version_file",
                    json!({ "path": ".nvmrc", "version": "22.11.0" })
                )),
            ),
        ]
    );

    let shop = w.report(Some(w.shop)).await;
    let labels: Vec<&str> = shop
        .checks
        .iter()
        .map(|c| c.subject.label.as_str())
        .collect();
    assert_eq!(labels, ["shop/api", "shop/web"]);
}
