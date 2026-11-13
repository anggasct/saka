#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use saka_api::{ApiConfig, AppState};
use saka_core::diagnostics::AgentLogin;
use saka_core::vocab::Tool;
use saka_testkit::{Sim, SimConfig};
use serde_json::Value;

const SAKA: &str = env!("CARGO_BIN_EXE_saka");

fn serve_running(sim: &Sim) -> String {
    let state = AppState::new(ApiConfig::new("0.1.0-test"));
    let engine = sim.engine();
    state.set_engine(engine.clone());
    let app = saka_api::router(state);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (_stop, stopped) = tokio::sync::watch::channel(false);
            tokio::spawn(engine.run(1, stopped));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

struct Host {
    dir: tempfile::TempDir,
}

impl Host {
    fn new() -> Host {
        let host = Host {
            dir: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(host.path("bin")).unwrap();
        std::fs::create_dir_all(host.path("home")).unwrap();
        std::fs::create_dir_all(host.path("xdg")).unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../saka-agents/tests/fixtures/fake-claude");
        std::fs::copy(fixture, host.path("bin/claude")).unwrap();
        host
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn account(&self) -> PathBuf {
        self.path("data/agents/claude/default")
    }

    fn agent_path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.path("bin").display())
    }

    fn write_config(&self, extra: &str) {
        let dir = self.path("xdg/saka");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[server]\nlisten = \"127.0.0.1:1\"\ndata_dir = \"{}\"\n\n[agent]\npath = [\"{}\", \"/usr/bin\", \"/bin\"]\n{extra}",
                self.path("data").display(),
                self.path("bin").display(),
            ),
        )
        .unwrap();
    }

    fn fake(&self, file: &str, value: &str) {
        std::fs::write(self.path(&format!("bin/{file}")), value).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(SAKA);
        c.args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.path("home"))
            .env("XDG_CONFIG_HOME", self.path("xdg"))
            .env("XDG_DATA_HOME", self.path("xdg-data"))
            .env("TERM", "xterm-test")
            .env("ANTHROPIC_API_KEY", "sk-ant-test-must-not-leak")
            .env("CLAUDE_CONFIG_DIR", self.path("operator-claude"))
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn calls_text(&self) -> String {
        let path = self.path("bin/calls");
        match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => panic!("reading {}: {e}", path.display()),
        }
    }

    fn bin_listing(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.path("bin"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn calls(&self) -> Vec<(String, BTreeMap<String, String>)> {
        let text = self.calls_text();
        let mut calls = Vec::new();
        let mut argv = String::new();
        let mut env = BTreeMap::new();
        for line in text.lines() {
            if let Some(a) = line.strip_prefix("argv ") {
                argv = a.to_string();
            } else if let Some(kv) = line.strip_prefix("env ") {
                let (k, v) = kv.split_once('=').unwrap();
                env.insert(k.to_string(), v.to_string());
            } else if line == "end" {
                calls.push((std::mem::take(&mut argv), std::mem::take(&mut env)));
            }
        }
        calls
    }

    fn logins(&self) -> Vec<(String, BTreeMap<String, String>)> {
        self.calls()
            .into_iter()
            .filter(|(argv, _)| argv.starts_with("auth login"))
            .collect()
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn agent_login_runs_the_cli_login_in_the_account_dir_with_only_allowlisted_env() {
    let host = Host::new();
    host.write_config("");
    let login = host.run(&["agent", "login"]);
    assert_eq!(code(&login), 0, "{}{}", stdout(&login), stderr(&login));
    assert_eq!(
        stdout(&login).trim(),
        "account claude:default logged in (claude.ai)"
    );
    assert_eq!(mode(&host.account()), 0o700);
    let logins = host.logins();
    assert_eq!(logins.len(), 1);
    let (argv, env) = &logins[0];
    assert_eq!(argv, "auth login");
    assert_eq!(
        env.clone(),
        BTreeMap::from([
            ("PATH".to_string(), host.agent_path()),
            ("HOME".to_string(), host.path("home").display().to_string()),
            ("TERM".to_string(), "xterm-test".to_string()),
            (
                "CLAUDE_CONFIG_DIR".to_string(),
                host.account().display().to_string()
            ),
        ])
    );
    for (_, env) in host.calls() {
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        assert_eq!(
            env["CLAUDE_CONFIG_DIR"],
            host.account().display().to_string()
        );
    }

    let again = host.run(&["agent", "login"]);
    assert_eq!(code(&again), 0);
    assert_eq!(
        stdout(&again).trim(),
        "account claude:default already logged in (claude.ai)"
    );
    assert_eq!(
        host.logins().len(),
        1,
        "a logged-in account is not logged in again"
    );
    let forced = host.run(&["agent", "login", "--force", "--console"]);
    assert_eq!(code(&forced), 0, "{}", stderr(&forced));
    let logins = host.logins();
    assert_eq!(logins.len(), 2);
    assert_eq!(logins[1].0, "auth login --console");
}

#[test]
fn agent_login_fails_when_the_cli_login_leaves_the_account_logged_out() {
    let host = Host::new();
    host.write_config("");
    host.fake("login", "noop");
    let login = host.run(&["agent", "login"]);
    assert_eq!(code(&login), 1);
    assert!(
        stderr(&login).contains("error: agent_login_failed {\"exit_code\":0}"),
        "{}",
        stderr(&login)
    );
}

#[test]
fn agent_login_tightens_an_open_account_dir_and_refuses_a_personal_one() {
    let host = Host::new();
    host.write_config("");
    std::fs::create_dir_all(host.account()).unwrap();
    std::fs::set_permissions(host.account(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code(&host.run(&["agent", "login"])), 0);
    assert_eq!(mode(&host.account()), 0o700);

    let personal = Host::new();
    personal.write_config(&format!(
        "account_dir = \"{}\"\n",
        personal.path("home/.claude").display()
    ));
    let refused = personal.run(&["agent", "login"]);
    assert_eq!(code(&refused), 2);
    assert!(
        stderr(&refused).contains("account_dir_personal"),
        "{}",
        stderr(&refused)
    );
    assert!(personal.calls().is_empty());
}

#[test]
fn agent_commands_without_the_cli_exit_1() {
    let host = Host::new();
    host.write_config("command = \"no-such-cli\"\n");
    for args in [&["agent", "login"][..], &["agent", "status"][..]] {
        let o = host.run(args);
        assert_eq!(code(&o), 1, "{args:?}");
        assert!(
            stderr(&o).contains("error: agent_cli_not_found {\"command\":\"no-such-cli\"}"),
            "{}",
            stderr(&o)
        );
    }
}

#[test]
fn agent_status_exits_by_login_and_prints_the_documented_json() {
    let host = Host::new();
    host.write_config("");
    let out = host.run(&["agent", "status"]);
    assert_eq!(code(&out), 1);
    assert_eq!(stdout(&out).trim(), "claude:default not logged in");
    host.fake("status", "in");
    let ok = host.run(&["agent", "status"]);
    assert_eq!(code(&ok), 0);
    assert_eq!(stdout(&ok).trim(), "claude:default logged in (claude.ai)");
    let json = host.run(&["--json", "agent", "status"]);
    assert_eq!(code(&json), 0);
    let body: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(
        body,
        serde_json::json!({
            "account": "claude:default",
            "config_dir": host.account().display().to_string(),
            "cli": { "path": host.path("bin/claude").display().to_string(), "version": "2.1.7" },
            "logged_in": true,
            "auth_method": "claude.ai",
        })
    );
    assert!(!stdout(&json).contains("operator@example.com"));
}

fn doctor(host: &Host, server: &str, args: &[&str]) -> Output {
    let mut all = vec!["--server", server, "doctor"];
    all.extend_from_slice(args);
    host.run(&all)
}

#[test]
fn doctor_exit_codes_follow_failures_warnings_and_flags() {
    let sim = Sim::new(SimConfig {
        auth_enabled: true,
        workflow_policy: saka_core::workflow::WorkflowPolicy::default(),
        ..SimConfig::default()
    });
    sim.engine()
        .store()
        .transact(|tx| {
            tx.execute(
                "INSERT INTO credential (id, name, passkey, created_at)
                 VALUES ('00000000-0000-0000-0000-0000000000c1', 'Laptop', '{}', 1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let server = serve_running(&sim);
    let host = Host::new();

    let clean = doctor(&host, &server, &[]);
    assert_eq!(code(&clean), 0, "{}{}", stdout(&clean), stderr(&clean));
    let last = stdout(&clean).lines().last().unwrap().to_string();
    assert!(last.ends_with("0 warn, 0 fail, 8 skip (online)"), "{last}");

    let json = doctor(&host, &server, &["--json"]);
    let body: Value = serde_json::from_slice(&json.stdout).unwrap();
    let ids: Vec<&str> = body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "cli.config",
            "server.reachable",
            "server.version",
            "server.ready",
            "db.migrations",
            "runtime.supervisor",
            "agent.binary",
            "agent.login",
            "agent.quota",
            "toolchain.cache",
            "toolchain.sources",
            "auth.config",
            "scm.credential",
            "scm.credential",
            "scm.repo_access",
            "scm.webhook",
            "scm.webhook",
            "scm.user_link",
            "scm.user_link",
            "project.user_token",
            "project.config",
            "toolchain.repo",
        ]
    );
    assert_eq!(body["mode"], "online");
    assert_eq!(body["checks"][0]["code"], "config_absent");
    assert_eq!(body["checks"][2]["code"], "api_compatible");

    sim.world.host().script(|s| {
        s.failing_sources.insert(Tool::Node);
    });
    assert_eq!(
        code(&doctor(&host, &server, &[])),
        0,
        "a warning alone passes"
    );
    assert_eq!(code(&doctor(&host, &server, &["--strict"])), 1);

    sim.world.host().script(|s| {
        s.failing_sources.clear();
        s.login = AgentLogin::NotLoggedIn;
    });
    let failed = doctor(&host, &server, &[]);
    assert_eq!(code(&failed), 1);
    let line = stdout(&failed)
        .lines()
        .find(|l| l.contains("agent_not_logged_in"))
        .unwrap()
        .to_string();
    assert!(
        line.starts_with("fail  agent.login  claude:default  agent_not_logged_in "),
        "{line}"
    );
    assert!(line.ends_with("  → saka agent login"), "{line}");

    let only = doctor(&host, &server, &["--json", "--check", "agent.binary"]);
    let body: Value = serde_json::from_slice(&only.stdout).unwrap();
    assert_eq!(body["checks"].as_array().unwrap().len(), 4);
    assert_eq!(code(&only), 0);

    for id in ["nope", "scm.app", "scm.installation"] {
        let unknown = doctor(&host, &server, &["--check", id]);
        assert_eq!(code(&unknown), 2);
        assert!(
            stderr(&unknown).contains(&format!("error: check_unknown {{\"id\":\"{id}\"}}")),
            "{}",
            stderr(&unknown)
        );
    }
}

fn closed_port() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

#[test]
fn doctor_runs_the_local_checks_read_only_when_the_server_is_down() {
    let host = Host::new();
    host.write_config("");
    std::fs::create_dir_all(host.path("data")).unwrap();
    let db = host.path("data/saka.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0))
            .unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migration (version INTEGER PRIMARY KEY, checksum TEXT NOT NULL, applied_at INTEGER NOT NULL) STRICT;
             INSERT INTO schema_migration VALUES (1, 'sha256:old', 1);",
        )
        .unwrap();
    }
    let o = doctor(&host, &closed_port(), &["--json"]);
    assert_eq!(code(&o), 1, "{}", stderr(&o));
    let body: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(body["mode"], "offline");
    let by_id = |id: &str| {
        body["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("{id} in {body}"))
            .clone()
    };
    assert_eq!(by_id("cli.config")["code"], "config_ok");
    assert_eq!(by_id("server.reachable")["code"], "server_unreachable");
    assert_eq!(by_id("server.version")["status"], "skip");
    let migrations = by_id("db.migrations");
    assert_eq!(migrations["code"], "migrations_pending");
    assert_eq!(migrations["params"]["applied"], 1);
    assert_eq!(
        migrations["fix"],
        serde_json::json!({ "code": "run_migrate", "params": { "command": "saka migrate" } })
    );
    assert_eq!(by_id("agent.login")["code"], "agent_not_logged_in");
    assert_eq!(by_id("agent.binary")["code"], "agent_cli_found");
    assert_eq!(by_id("toolchain.cache")["code"], "toolchain_cache_ok");
    for id in [
        "server.ready",
        "agent.quota",
        "toolchain.sources",
        "auth.config",
        "scm.credential",
        "scm.repo_access",
        "scm.webhook",
        "scm.user_link",
        "project.user_token",
        "project.config",
        "toolchain.repo",
    ] {
        assert_eq!(by_id(id)["status"], "skip", "{id}");
        assert_eq!(by_id(id)["code"], "server_unreachable", "{id}");
    }
    assert!(
        host.calls()
            .iter()
            .any(|(argv, _)| argv == "auth status --json"),
        "no `auth status --json` call recorded\nbin: {:?}\ncalls:\n{}\nstdout:\n{}\nstderr:\n{}",
        host.bin_listing(),
        host.calls_text(),
        String::from_utf8_lossy(&o.stdout),
        stderr(&o),
    );
    let conn = rusqlite::Connection::open(&db).unwrap();
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migration", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(version, 1, "the doctor never migrates");
}
