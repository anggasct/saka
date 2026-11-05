#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use saka_api::{ApiConfig, AppState};
use saka_testkit::{DOCS_REPO, Layout, SHOP_REPO, Sim, SimConfig, WEB_REPO, repo_entry, saka_toml};
use serde_json::{Value, json};

mod support;

use support::{nullable, schema_errors};

const SAKA: &str = env!("CARGO_BIN_EXE_saka");
const DEAD_SERVER: &str = "http://127.0.0.1:9";

fn serve(sim: &Sim) -> String {
    let state = AppState::new(ApiConfig::new("0.1.0-test"));
    state.set_engine(sim.engine());
    let app = saka_api::router(state);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn text(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    config: PathBuf,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            "[toolchain]\ngo_dl_url = \"http://127.0.0.1:9/dl\"\nnode_dist_url = \"http://127.0.0.1:9/dist\"\nnpm_registry_url = \"http://127.0.0.1:9/npm\"\n",
        )
        .unwrap();
        Env {
            _dir: dir,
            root,
            home,
            config,
        }
    }

    fn repo(&self, name: &str, origin: &str, files: &[(&str, &str)]) -> Repo {
        let path = self.root.join(name);
        let bare = self.root.join(format!("{name}.git"));
        std::fs::create_dir_all(&path).unwrap();
        git(
            &self.root,
            &["init", "-q", "--bare", bare.to_str().unwrap()],
        );
        git(&path, &["init", "-q", "-b", "main"]);
        for (file, body) in files {
            let target = path.join(file);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, body).unwrap();
        }
        std::fs::write(path.join("README.md"), "hi\n").unwrap();
        git(&path, &["add", "-A"]);
        git(&path, &["commit", "-q", "-m", "init"]);
        git(&path, &["remote", "add", "origin", origin]);
        git(
            &path,
            &[
                "remote",
                "set-url",
                "--push",
                "origin",
                bare.to_str().unwrap(),
            ],
        );
        git(&path, &["push", "-q", bare.to_str().unwrap(), "main"]);
        git(&path, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(
            &path,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        Repo { path, bare }
    }

    fn init(&self, dir: &Path, server: &str, args: &[&str]) -> Output {
        self.init_with(dir, server, args, &[])
    }

    fn init_with(&self, dir: &Path, server: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(SAKA);
        cmd.current_dir(dir)
            .arg("--server")
            .arg(server)
            .arg("init")
            .args(args)
            .env("SAKA_CONFIG", &self.config)
            .env("HOME", &self.home)
            .env_remove("SAKA_SERVER")
            .env_remove("GIT_CONFIG_GLOBAL");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }
}

struct Repo {
    path: PathBuf,
    bare: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
struct GitState {
    commits: String,
    stash: String,
    index_clean: bool,
    config: Vec<u8>,
    remote_refs: String,
    head: String,
}

impl Repo {
    fn state(&self) -> GitState {
        let cached = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["diff", "--cached", "--quiet"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        GitState {
            commits: text(&git(&self.path, &["rev-list", "--all", "--count"])),
            stash: text(&git(&self.path, &["stash", "list"])),
            index_clean: cached.success(),
            config: std::fs::read(self.path.join(".git/config")).unwrap(),
            remote_refs: text(&git(&self.bare, &["for-each-ref"])),
            head: text(&git(&self.path, &["rev-parse", "HEAD"])),
        }
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path.join(rel)).unwrap()
    }

    fn files(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut pending = vec![self.path.clone()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                if path.is_dir() {
                    pending.push(path);
                } else {
                    out.push(path.strip_prefix(&self.path).unwrap().display().to_string());
                }
            }
        }
        out.sort();
        out
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn report(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|_| {
        panic!(
            "not JSON: {}\n{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn init_report_schema() -> Value {
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    json!({
        "type": "object",
        "required": ["dry_run", "repo", "docs", "config", "toolchains", "files_skipped", "recipe", "check", "registration", "pending_files", "exit_code"],
        "properties": {
            "dry_run": { "type": "boolean" },
            "repo": { "type": "object", "required": ["root", "url", "default_branch", "name"],
                "properties": { "root": { "type": "string" }, "url": { "type": "string" }, "default_branch": nullable("string"), "name": { "type": "string" } } },
            "docs": { "type": "object", "required": ["location", "root", "workspace_url", "project_path", "base_branch"],
                "properties": { "location": { "enum": ["here", "repo"] }, "root": { "type": "string" }, "workspace_url": { "type": "string" },
                    "project_path": { "type": "string" }, "base_branch": nullable("string") } },
            "config": { "type": "object", "required": ["path", "action", "written", "sha256", "repo_name", "notes"],
                "properties": { "action": { "enum": ["create", "update", "none"] }, "written": { "type": "boolean" }, "sha256": { "type": "string" },
                    "notes": { "type": "array", "items": { "type": "object", "required": ["code", "params"] } } } },
            "toolchains": { "type": "array", "items": { "type": "object",
                "required": ["tool", "status", "version", "source", "value", "provisioned_by_saka", "evidence", "drift", "proposal"],
                "properties": {
                    "tool": { "enum": ["go", "node", "pnpm", "rust", "python"] },
                    "status": { "enum": ["pinned", "unpinned", "evidence_only", "undeclared"] },
                    "provisioned_by_saka": { "type": "boolean" },
                    "evidence": { "type": "array", "items": { "type": "object", "required": ["source", "file", "line", "value"] } },
                    "proposal": { "type": "object", "required": ["action", "path", "version", "content", "command", "code", "params", "written"],
                        "properties": { "action": { "enum": ["none", "write_file", "replace_file", "manual"] }, "written": { "type": "boolean" } } } } } },
            "files_skipped": { "type": "array", "items": { "type": "object", "required": ["path", "reason"] } },
            "recipe": { "type": "object", "required": ["path", "action", "written", "commands", "issues", "notes"],
                "properties": { "action": { "enum": ["create", "none"] }, "written": { "type": "boolean" }, "commands": strings } },
            "check": { "anyOf": [{ "type": "null" }, { "type": "object", "required": ["ok", "problems", "repos", "remote", "project", "content_sha256", "workspace"] }] },
            "registration": { "type": "object", "required": ["outcome", "project_id", "reload_outcome"],
                "properties": { "outcome": { "enum": ["registered", "reloaded", "up_to_date", "pending_push", "skipped", "failed"] },
                    "project_id": nullable("string"), "reload_outcome": nullable("string") } },
            "pending_files": { "type": "array", "items": { "type": "object", "required": ["root", "paths"], "properties": { "paths": strings } } },
            "exit_code": { "type": "integer" }
        }
    })
}

fn assert_schema(value: &Value) {
    let mut errors = Vec::new();
    schema_errors(&init_report_schema(), value, "$", &mut errors);
    assert!(errors.is_empty(), "{errors:#?}\n{value:#}");
}

fn project_names(server: &str) -> Vec<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let body: Value = rt.block_on(async {
        reqwest::get(format!("{server}/api/v1/projects"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    });
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn init_converges_on_a_registered_project_without_touching_git() {
    let env = Env::new();
    let sim = Sim::new(SimConfig::default());
    let server = serve(&sim);
    let repo = env.repo("tasks", "git@github.com:acme/tasks.git", &[]);
    let before = repo.state();

    let first = env.init(
        &repo.path,
        &server,
        &[
            "--yes",
            "--json",
            "--docs-path",
            "shop",
            "--project-name",
            "shop",
        ],
    );
    assert_eq!(code(&first), 5, "{}", stderr(&first));
    let r = report(&first);
    assert_schema(&r);
    assert_eq!(r["registration"]["outcome"], "pending_push");
    assert_eq!(r["config"]["action"], "create");
    assert_eq!(r["config"]["written"], true);
    assert_eq!(r["repo"]["url"], "https://github.com/acme/tasks");
    assert_eq!(r["repo"]["default_branch"], "main");
    assert_eq!(r["check"]["remote"]["present"], false);
    assert_eq!(
        r["pending_files"],
        json!([{ "root": repo.path.canonicalize().unwrap().display().to_string(), "paths": ["shop/saka.toml"] }])
    );
    let written = repo.read("shop/saka.toml");
    assert_eq!(
        written,
        "[project]\nname = \"shop\"\n\n[[repo]]\nname = \"tasks\"\nurl = \"https://github.com/acme/tasks\"\n"
    );
    let human = env.init(&repo.path, &server, &["--yes", "--docs-path", "shop"]);
    assert_eq!(code(&human), 5);
    let lines = stdout(&human);
    let root = repo.path.canonicalize().unwrap().display().to_string();
    assert!(
        lines.contains(&format!("next  git -C {root} add shop/saka.toml && git -C {root} commit -m \"chore: set up Saka\" && git -C {root} push\nnext  then run saka init again\n")),
        "{lines}"
    );
    assert_eq!(
        repo.read("shop/saka.toml"),
        written,
        "a second run does not add the repo again"
    );

    sim.world.put_doc("shop/saka.toml", written.as_bytes());
    let second = env.init(
        &repo.path,
        &server,
        &["--yes", "--json", "--docs-path", "shop"],
    );
    assert_eq!(code(&second), 0, "{}", stderr(&second));
    let r = report(&second);
    assert_eq!(r["registration"]["outcome"], "registered");
    assert!(r["registration"]["project_id"].is_string());
    assert!(project_names(&server).contains(&"shop".to_string()));

    let third = env.init(&repo.path, &server, &["--yes", "--docs-path", "shop"]);
    assert_eq!(code(&third), 0, "{}", stderr(&third));
    assert!(
        stdout(&third).contains("\nproject  shop "),
        "{}",
        stdout(&third)
    );
    assert!(
        stdout(&third).trim_end().ends_with(" up to date"),
        "{}",
        stdout(&third)
    );

    let changed = written.replace("[project]\n", "# owned by the shop team\n[project]\n");
    std::fs::write(repo.path.join("shop/saka.toml"), &changed).unwrap();
    sim.world.put_doc("shop/saka.toml", changed.as_bytes());
    let fourth = env.init(
        &repo.path,
        &server,
        &["--yes", "--json", "--docs-path", "shop"],
    );
    assert_eq!(code(&fourth), 0, "{}", stderr(&fourth));
    let r = report(&fourth);
    assert_eq!(r["registration"]["outcome"], "reloaded");
    assert!(r["registration"]["reload_outcome"].is_string());
    assert_eq!(r["config"]["action"], "none");
    assert_eq!(repo.read("shop/saka.toml"), changed);

    let dry = env.init(
        &repo.path,
        &server,
        &["--yes", "--json", "--dry-run", "--docs-path", "other"],
    );
    assert_eq!(code(&dry), 0, "{}", stderr(&dry));
    let r = report(&dry);
    assert_eq!(r["dry_run"], true);
    assert_eq!(r["config"]["written"], false);
    assert_eq!(r["registration"]["outcome"], "skipped");
    assert!(r["check"]["ok"] == true);
    assert!(!repo.path.join("other").exists());

    let files = repo.files();
    let offline = env.init(
        &repo.path,
        DEAD_SERVER,
        &["--yes", "--json", "--no-register", "--docs-path", "offline"],
    );
    assert_eq!(code(&offline), 0, "{}", stderr(&offline));
    let r = report(&offline);
    assert_eq!(r["check"], Value::Null);
    assert_eq!(r["registration"]["outcome"], "skipped");
    assert!(repo.path.join("offline/saka.toml").exists());
    std::fs::remove_dir_all(repo.path.join("offline")).unwrap();
    assert_eq!(repo.files(), files);

    let down = env.init(
        &repo.path,
        DEAD_SERVER,
        &["--yes", "--json", "--docs-path", "down"],
    );
    assert_eq!(code(&down), 1);
    let r = report(&down);
    assert_eq!(r["error"]["code"], "server_unreachable");
    assert_eq!(r["registration"]["outcome"], "failed");
    assert!(
        repo.path.join("down/saka.toml").exists(),
        "local files stay written"
    );
    assert!(stderr(&down).contains("server_unreachable"));

    let after = repo.state();
    assert_eq!(after, before, "saka init never writes to git");
}

#[test]
fn config_problems_exit_three_and_list_every_problem() {
    let env = Env::new();
    let sim = Sim::new(SimConfig::default());
    let server = serve(&sim);
    let repo = env.repo("x", "https://gitlab.example/acme/x.git", &[]);
    let o = env.init(&repo.path, &server, &["--yes"]);
    assert_eq!(code(&o), 3, "{}", stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("check  2 problems"), "{out}");
    assert!(
        out.contains(
            "problem  scm_integration_unknown {\"host\":\"gitlab.example\",\"repo\":null}"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "problem  scm_integration_unknown {\"host\":\"gitlab.example\",\"repo\":\"x\"}"
        ),
        "{out}"
    );
    assert_eq!(
        out.matches("next  saka scm create-app --web-url https://gitlab.example\n")
            .count(),
        1,
        "{out}"
    );
    let json = env.init(&repo.path, &server, &["--yes", "--json"]);
    let report: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(report.get("next").is_none(), "{report}");
    assert!(!stdout(&json).contains("create-app"));
}

#[test]
fn discovery_refusals_exit_two_and_ignore_personal_git_config() {
    let env = Env::new();
    let plain = env.root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let o = env.init(&plain, DEAD_SERVER, &["--yes", "--json", "--no-register"]);
    assert_eq!(code(&o), 2);
    assert_eq!(report(&o)["code"], "not_a_git_repo");
    assert!(stderr(&o).contains("\"code\":\"not_a_git_repo\""));

    let repo = env.repo("shop", "https://github.com/acme/shop", &[]);
    let o = env.init(
        &repo.path,
        DEAD_SERVER,
        &["--yes", "--json", "--no-register", "--remote", "upstream"],
    );
    assert_eq!(code(&o), 2);
    assert_eq!(
        report(&o),
        json!({ "code": "remote_missing", "params": { "remote": "upstream" } })
    );

    for bad in [
        "file:///srv/git/shop.git",
        "https://github.com/acme/shop/extra",
    ] {
        let other = env.repo(&format!("bad{}", bad.len()), bad, &[]);
        let o = env.init(
            &other.path,
            DEAD_SERVER,
            &["--yes", "--json", "--no-register"],
        );
        assert_eq!(code(&o), 2, "{bad}");
        assert_eq!(report(&o)["code"], "remote_unsupported", "{bad}");
    }

    let o = env.init(
        &repo.path,
        DEAD_SERVER,
        &["--yes", "--json", "--no-register", "--docs", "repo"],
    );
    assert_eq!(code(&o), 2);
    assert_eq!(report(&o)["code"], "docs_repo_required");

    let personal = env.root.join("personal.gitconfig");
    std::fs::write(
        &personal,
        "[url \"https://mirror.example/\"]\n\tinsteadOf = https://github.com/\n",
    )
    .unwrap();
    let o = env.init_with(
        &repo.path,
        DEAD_SERVER,
        &["--yes", "--json", "--no-register", "--dry-run"],
        &[("GIT_CONFIG_GLOBAL", personal.to_str().unwrap())],
    );
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(report(&o)["repo"]["url"], "https://github.com/acme/shop");
}

fn toolchain_repo(env: &Env) -> Repo {
    env.repo(
        "tools",
        "https://github.com/acme/tools",
        &[
            (
                "package.json",
                "{\n  \"name\": \"tools\",\n  \"packageManager\": \"pnpm@9\"\n}\n",
            ),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
            ("go.mod", "module example.com/tools\n"),
            ("go.sum", ""),
            (
                "Dockerfile",
                "FROM node:22.14.0-alpine\nFROM golang:1.24.3\n",
            ),
            (
                "Cargo.toml",
                "[package]\nname = \"tools\"\nversion = \"0.1.0\"\n",
            ),
            (".tool-versions", "rust 1.82.0\npython 3.12.7\n"),
            (".python-version", "3.12\n"),
        ],
    )
}

#[test]
fn version_files_and_the_recipe_are_written_only_when_asked() {
    let env = Env::new();
    let repo = toolchain_repo(&env);
    let package = repo.read("package.json");
    let go_mod = repo.read("go.mod");

    let o = env.init(
        &repo.path,
        DEAD_SERVER,
        &["--yes", "--json", "--no-register"],
    );
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let r = report(&o);
    assert_schema(&r);
    let proposals: BTreeMap<String, (String, bool)> = r["toolchains"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["tool"].as_str().unwrap().to_string(),
                (
                    t["proposal"]["action"].as_str().unwrap().to_string(),
                    t["proposal"]["written"] == true,
                ),
            )
        })
        .collect();
    assert_eq!(proposals["node"], ("write_file".to_string(), false));
    assert_eq!(proposals["rust"], ("write_file".to_string(), false));
    assert_eq!(proposals["python"], ("replace_file".to_string(), false));
    assert_eq!(proposals["go"], ("manual".to_string(), false));
    assert_eq!(proposals["pnpm"], ("manual".to_string(), false));
    assert_eq!(r["recipe"]["action"], "create");
    assert_eq!(r["recipe"]["written"], false);
    assert!(!repo.path.join(".nvmrc").exists());
    assert!(!repo.path.join(".saka").exists());
    assert_eq!(repo.read(".python-version"), "3.12\n");
    let human = env.init(&repo.path, DEAD_SERVER, &["--yes", "--no-register"]);
    let lines = stdout(&human);
    assert!(
        lines.contains(
            "toolchain  pnpm unpinned \"9\" (package_json_package_manager) → version_unresolved"
        ),
        "{lines}"
    );
    assert!(
        lines.contains("toolchain  go evidence_only → go mod edit -go=1.24.3"),
        "{lines}"
    );
    assert!(
        lines.contains("toolchain  node evidence_only → write .nvmrc (22.14.0)"),
        "{lines}"
    );

    let o = env.init(
        &repo.path,
        DEAD_SERVER,
        &[
            "--yes",
            "--json",
            "--no-register",
            "--write-version-files",
            "--write-recipe",
        ],
    );
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let r = report(&o);
    assert_eq!(repo.read(".nvmrc"), "22.14.0\n");
    assert_eq!(
        repo.read("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.82.0\"\n"
    );
    assert_eq!(repo.read(".python-version"), "3.12.7\n");
    assert_eq!(
        repo.read(".saka/environment.toml"),
        "# Read by Saka from the default branch; each task runs these commands in its own environment.\n[setup]\ncommands = [\"pnpm install --frozen-lockfile\", \"go mod download\"]\n"
    );
    assert_eq!(r["recipe"]["written"], true);
    assert_eq!(repo.read("package.json"), package);
    assert_eq!(repo.read("go.mod"), go_mod);
    let pending = r["pending_files"][0]["paths"].as_array().unwrap();
    for path in [
        ".nvmrc",
        ".python-version",
        "rust-toolchain.toml",
        ".saka/environment.toml",
        "docs/saka.toml",
    ] {
        assert!(pending.contains(&json!(path)), "{path} in {pending:?}");
    }
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(repo.path.join(".nvmrc"))
            .unwrap()
            .permissions(),
    );
    assert_eq!(mode & 0o777, 0o644);

    let again = env.init(
        &repo.path,
        DEAD_SERVER,
        &[
            "--yes",
            "--json",
            "--no-register",
            "--write-version-files",
            "--write-recipe",
        ],
    );
    assert_eq!(code(&again), 0, "{}", stderr(&again));
    let r = report(&again);
    assert_eq!(r["recipe"]["action"], "none");
    assert_eq!(r["recipe"]["reason"], "recipe_exists");
    assert_eq!(r["config"]["action"], "none");
}

#[test]
fn a_separate_docs_repo_collects_the_code_repos_of_one_project() {
    let env = Env::new();
    let layout = Layout {
        saka_toml: saka_toml("docs", &[repo_entry("docs", DOCS_REPO, "")]),
        ..Layout::default()
    };
    let sim = Sim::multi_repo(SimConfig::default(), &layout);
    let server = serve(&sim);
    let docs = env.repo("docs", &format!("https://github.com/{DOCS_REPO}"), &[]);
    let shop = env.repo("shop", &format!("git@github.com:{SHOP_REPO}.git"), &[]);
    let web = env.repo("web", &format!("https://github.com/{WEB_REPO}.git"), &[]);
    let docs_state = docs.state();

    let docs_dir = docs.path.to_str().unwrap();
    let args = [
        "--yes",
        "--json",
        "--docs",
        "repo",
        "--docs-repo",
        docs_dir,
        "--docs-path",
        "shop",
    ];
    let o = env.init(&shop.path, &server, &args);
    assert_eq!(code(&o), 5, "{}", stderr(&o));
    let r = report(&o);
    assert_eq!(r["docs"]["location"], "repo");
    assert_eq!(
        r["docs"]["workspace_url"],
        format!("https://github.com/{DOCS_REPO}")
    );
    assert_eq!(r["check"]["workspace"]["full_name"], DOCS_REPO);
    assert_eq!(r["check"]["repos"][0]["full_name"], SHOP_REPO);
    assert_eq!(
        docs.read("shop/saka.toml"),
        format!(
            "[project]\nname = \"shop\"\n\n[[repo]]\nname = \"shop\"\nurl = \"https://github.com/{SHOP_REPO}\"\n"
        )
    );
    assert!(!shop.path.join("docs").exists());

    let o = env.init(&web.path, &server, &args);
    assert_eq!(code(&o), 5, "{}", stderr(&o));
    let r = report(&o);
    assert_eq!(r["config"]["action"], "update");
    assert_eq!(
        docs.read("shop/saka.toml"),
        format!(
            "[project]\nname = \"shop\"\n\n[[repo]]\nname = \"shop\"\nurl = \"https://github.com/{SHOP_REPO}\"\ndefault = true\n\n[[repo]]\nname = \"web\"\nurl = \"https://github.com/{WEB_REPO}\"\n"
        )
    );
    let docs_root = docs.path.canonicalize().unwrap().display().to_string();
    assert!(
        r["pending_files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["root"] == docs_root.as_str() && p["paths"] == json!(["shop/saka.toml"])),
        "{}",
        r["pending_files"]
    );
    assert_eq!(r["check"]["repos"].as_array().unwrap().len(), 2);
    assert_eq!(docs.state(), docs_state);
}

#[test]
fn a_symlinked_project_folder_cannot_move_writes_outside_the_clone() {
    let env = Env::new();
    let repo = env.repo("linked", "https://github.com/acme/linked", &[]);
    let outside = env.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, repo.path.join("docs")).unwrap();
    let o = env.init(
        &repo.path,
        DEAD_SERVER,
        &["--yes", "--json", "--no-register"],
    );
    assert_eq!(code(&o), 1, "{}", stdout(&o));
    assert_eq!(report(&o)["code"], "path_outside_repo");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    std::os::unix::fs::symlink(&outside, repo.path.join("nested")).unwrap();
    let o = env.init(
        &repo.path,
        DEAD_SERVER,
        &[
            "--yes",
            "--json",
            "--no-register",
            "--docs-path",
            "nested/deeper",
        ],
    );
    assert_eq!(code(&o), 1);
    assert_eq!(
        std::fs::read_dir(&outside).unwrap().count(),
        0,
        "no folder is created outside"
    );
}
