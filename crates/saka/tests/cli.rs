#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use saka_api::{ApiConfig, AppState};
use saka_testkit::fixtures::TWO_STEP_SPEC;
use saka_testkit::{
    Layout, SHOP_REPO, Sim, SimConfig, WEB_REPO, one_step_spec, repo_entry, saka_toml,
};
use serde_json::Value;

const SAKA: &str = env!("CARGO_BIN_EXE_saka");

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

fn saka(server: &str, args: &[&str]) -> Output {
    Command::new(SAKA)
        .arg("--server")
        .arg(server)
        .args(args)
        .env_remove("SAKA_CONFIG")
        .output()
        .unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|_| panic!("not JSON: {}", stdout(o)))
}

#[test]
fn operator_commands_call_the_api_and_exit_with_documented_codes() {
    let sim = Sim::new(SimConfig::default());
    sim.world.put_doc(
        "other/saka.toml",
        b"[project]\nname = \"other\"\n\n[[repo]]\nname = \"app\"\nurl = \"https://github.com/acme/tasks\"\n",
    );
    sim.world.put_doc(
        "other/specs/feature.md",
        TWO_STEP_SPEC
            .replace("Mark tasks as done", "Feature")
            .as_bytes(),
    );
    sim.world
        .put_doc("other/specs/broken.md", b"---\nsaka: spec\n---\n# Broken\n");
    let server = serve(&sim);

    let added = saka(
        &server,
        &[
            "--json",
            "project",
            "add",
            "https://github.com/acme/tasks",
            "--path",
            "other",
        ],
    );
    assert_eq!(
        code(&added),
        0,
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    assert_eq!(json(&added)["name"], "other");
    let listed = saka(&server, &["project", "list"]);
    assert_eq!(code(&listed), 0);
    assert!(
        stdout(&listed).contains("  other  acme/tasks/other"),
        "{}",
        stdout(&listed)
    );

    let approved = saka(
        &server,
        &["spec", "approve", "other", "other/specs/feature.md"],
    );
    assert_eq!(
        code(&approved),
        0,
        "{}",
        String::from_utf8_lossy(&approved.stderr)
    );
    assert_eq!(stdout(&approved).trim(), "approved feature (2 steps)");
    let again = saka(
        &server,
        &["spec", "approve", "other", "other/specs/feature.md"],
    );
    assert_eq!(code(&again), 4, "409 spec_already_approved");
    assert!(String::from_utf8_lossy(&again.stderr).contains("spec_already_approved"));

    let lint = saka(
        &server,
        &["spec", "approve", "other", "other/specs/broken.md"],
    );
    assert_eq!(code(&lint), 3);
    let out = stdout(&lint);
    assert!(out.lines().all(|l| l.starts_with("line ")), "{out}");
    assert!(out.contains("ac_section_missing"), "{out}");
    let missing = saka(
        &server,
        &["spec", "approve", "other", "other/specs/none.md"],
    );
    assert_eq!(code(&missing), 1, "404 spec_file_not_found");

    let tasks = saka(
        &server,
        &["--json", "task", "list", "--state", "queued,admitted"],
    );
    assert_eq!(code(&tasks), 0);
    let items = json(&tasks)["items"].as_array().unwrap().clone();
    assert!(!items.is_empty());
    let id = items[0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        code(&saka(&server, &["task", "merge", &id])),
        4,
        "merge_not_ready / invalid_state"
    );
    assert_eq!(
        code(&saka(&server, &["task", "retry", &id, "--reason", "again"])),
        4
    );
    assert_eq!(
        code(&saka(
            &server,
            &["task", "takeover", &id, "--reason", "early"]
        )),
        4,
        "a queued task is not taken over"
    );
    assert_eq!(
        code(&saka(
            &server,
            &["task", "return", &id, "--reason", "early"]
        )),
        4,
        "only a taken over task returns"
    );
    assert_eq!(
        code(&saka(
            &server,
            &["task", "return", &id, "--stage", "sync", "--reason", "x"]
        )),
        2,
        "usage: the stage is dev, review, fix or merge"
    );
    let cancelled = saka(
        &server,
        &["--json", "task", "cancel", &id, "--reason", "not needed"],
    );
    assert_eq!(code(&cancelled), 0);
    assert_eq!(json(&cancelled)["state"], "cancelled");

    assert_eq!(code(&saka(&server, &["permission", "list"])), 0);
    let unknown = uuid::Uuid::now_v7().to_string();
    assert_eq!(
        code(&saka(&server, &["permission", "answer", &unknown, "allow"])),
        1,
        "404"
    );
    assert_eq!(
        code(&saka(&server, &["task", "retry", &id])),
        2,
        "usage: --reason is required"
    );
    assert_eq!(
        code(&saka(&server, &["permission", "answer", &unknown, "maybe"])),
        2
    );
    assert_eq!(
        code(&saka("http://127.0.0.1:1", &["project", "list"])),
        1,
        "server unreachable"
    );

    let keys: Vec<String> = sim
        .engine()
        .store()
        .read(|c| {
            let mut stmt = c
                .prepare("SELECT idempotency_key FROM action_request")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            Ok(rows.map(Result::unwrap).collect())
        })
        .unwrap();
    assert!(keys.len() >= 8, "{keys:?}");
    let mut unique = keys.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), keys.len());
    assert!(keys.iter().all(|k| k.parse::<uuid::Uuid>().is_ok()));
}

fn write_config(dir: &Path, extra: &str) -> std::path::PathBuf {
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        format!(
            "[server]\ndata_dir = \"{}\"\n{extra}",
            dir.join("data").display()
        ),
    )
    .unwrap();
    path
}

fn own_listen() -> String {
    format!("listen = \"127.0.0.1:{}\"\n", free_port())
}

fn run_saka(config: &Path, args: &[&str]) -> Output {
    Command::new(SAKA)
        .args(args)
        .env("SAKA_CONFIG", config)
        .output()
        .unwrap()
}

#[test]
fn serve_refuses_a_listen_address_beyond_loopback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[server]\nlisten = \"0.0.0.0:7420\"\n").unwrap();
    let out = run_saka(&path, &["serve"]);
    assert_eq!(code(&out), 2);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("\"code\":\"non_loopback_requires_auth\"")
    );

    std::fs::write(&path, "[server]\nport = 1\n").unwrap();
    let out = run_saka(&path, &["serve"]);
    assert_eq!(code(&out), 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("config_unknown_field"));
}

#[test]
fn serve_refuses_a_data_dir_agent_sessions_cannot_scope_their_memory_to() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("sa[ka]");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        format!("[server]\ndata_dir = \"{}\"\n", data.display()),
    )
    .unwrap();
    let out = run_saka(&path, &["serve"]);
    assert_eq!(code(&out), 2);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("\"code\":\"envs_dir_unsupported\""),
        "{stderr}"
    );
    assert!(
        stderr.contains(&data.join("envs").display().to_string()),
        "{stderr}"
    );
    assert!(!data.join("saka.db").exists());
}

#[test]
fn migrate_refuses_a_changed_migration_and_a_newer_database() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), &own_listen());
    assert_eq!(code(&run_saka(&config, &["migrate"])), 0);
    let db = dir.path().join("data/saka.db");
    assert!(db.is_file());

    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE schema_migration SET checksum = 'changed' WHERE version = 1",
        [],
    )
    .unwrap();
    let out = run_saka(&config, &["migrate"]);
    assert_eq!(code(&out), 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("migration_checksum_mismatch"));
    let out = run_saka(&config, &["serve"]);
    assert_eq!(code(&out), 2, "serve refuses it too");

    let checksum: String = {
        let fresh = tempfile::tempdir().unwrap();
        let cfg = write_config(fresh.path(), "");
        assert_eq!(code(&run_saka(&cfg, &["migrate"])), 0);
        let c = rusqlite::Connection::open(fresh.path().join("data/saka.db")).unwrap();
        c.query_row(
            "SELECT checksum FROM schema_migration WHERE version = 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    conn.execute(
        "UPDATE schema_migration SET checksum = ?1 WHERE version = 1",
        [&checksum],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO schema_migration (version, checksum, applied_at) VALUES (999, 'x', 0)",
        [],
    )
    .unwrap();
    drop(conn);
    let out = run_saka(&config, &["migrate"]);
    assert_eq!(code(&out), 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("migration_version_ahead"));
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn http_get(port: u16, path: &str) -> Option<(u16, String)> {
    use std::io::Write;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut text = String::new();
    s.read_to_string(&mut text).ok()?;
    let status = text.split_whitespace().nth(1)?.parse().ok()?;
    let body = text
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_string();
    Some((status, body))
}

#[test]
fn serve_becomes_ready_logs_json_with_correlation_ids_and_stops_on_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port();
    std::fs::create_dir_all(dir.path().join("real")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("data")).unwrap();
    let config = write_config(dir.path(), &format!("listen = \"127.0.0.1:{port}\"\n"));
    let mut child = Command::new(SAKA)
        .arg("serve")
        .env("SAKA_CONFIG", &config)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut ready = false;
    while Instant::now() < deadline {
        if let Some((200, body)) = http_get(port, "/readyz") {
            assert!(body.contains("\"status\":\"ready\""), "{body}");
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(ready, "never ready");
    let (status, body) = http_get(port, "/api/v1/meta").unwrap();
    assert_eq!(status, 200);
    assert!(body.contains("\"api_version\":\"1\""));
    assert_eq!(http_get(port, "/healthz").unwrap().0, 200);

    let pid = child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let lines: Vec<Value> = stderr
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|_| panic!("not JSON: {l}")))
        .collect();
    let requests: Vec<&Value> = lines.iter().filter(|l| l["span"] == "request").collect();
    assert!(
        requests.iter().any(|l| l["route"] == "/api/v1/meta"),
        "{stderr}"
    );
    for line in &requests {
        assert!(
            line["correlation_id"]
                .as_str()
                .is_some_and(|c| c.parse::<uuid::Uuid>().is_ok()),
            "{line}"
        );
    }
    assert!(lines.iter().any(|l| l["message"] == "shutting down"));
}

#[test]
fn spec_template_prints_the_template_and_approve_reports_warnings_and_dependencies() {
    let sim = Sim::new(SimConfig::default());
    sim.world.put_doc(
        "docs/specs/warned.md",
        one_step_spec("Warned", "")
            .replace("- `make test`\n", "- `make test`\n- `make test`\n")
            .as_bytes(),
    );
    sim.world.put_doc(
        "docs/specs/needs-ghost.md",
        one_step_spec("Needs ghost", "depends_on: [ghost]\n").as_bytes(),
    );
    let server = serve(&sim);

    let en = saka(&server, &["spec", "template"]);
    assert_eq!(code(&en), 0, "{}", String::from_utf8_lossy(&en.stderr));
    assert_eq!(stdout(&en), saka_spec::template(saka_spec::Locale::En));
    let id = saka(&server, &["spec", "template", "--locale", "id"]);
    assert_eq!(code(&id), 0);
    assert_eq!(stdout(&id), saka_spec::template(saka_spec::Locale::Id));
    let bad = saka(&server, &["spec", "template", "--locale", "fr"]);
    assert_eq!(code(&bad), 1);
    assert!(String::from_utf8_lossy(&bad.stderr).contains("validation_failed"));

    let warned = saka(
        &server,
        &["spec", "approve", "tasks", "docs/specs/warned.md"],
    );
    assert_eq!(
        code(&warned),
        0,
        "{}",
        String::from_utf8_lossy(&warned.stderr)
    );
    let out = stdout(&warned);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "warning line 10: step_command_duplicate {\"command\":\"make test\",\"step\":\"S1\"}",
            "approved warned (1 steps)"
        ],
        "{out}"
    );

    let refused = saka(
        &server,
        &["spec", "approve", "tasks", "docs/specs/needs-ghost.md"],
    );
    assert_eq!(code(&refused), 3);
    assert_eq!(
        stdout(&refused).trim(),
        "line 3: depends_on_unknown {\"slug\":\"ghost\"}"
    );
}

#[test]
fn spec_approve_summarizes_a_re_approval_and_spec_withdraw_closes_a_spec() {
    use saka_core::vocab::WorkItemState;
    use saka_testkit::{APPROVAL_PATH, approval_spec};

    let mut sim = Sim::new(SimConfig {
        max_active_tasks: 1,
        ..SimConfig::default()
    });
    let server = serve(&sim);
    let put = |sim: &Sim, steps: &[(&str, &str)]| {
        sim.world
            .put_doc(APPROVAL_PATH, approval_spec("", steps).as_bytes());
    };
    put(
        &sim,
        &[("One", "AC-01"), ("Two", "AC-02"), ("Three", "AC-02")],
    );
    let first = saka(&server, &["spec", "approve", "tasks", APPROVAL_PATH]);
    assert_eq!(
        code(&first),
        0,
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(stdout(&first).trim(), "approved approval (3 steps)");
    let spec = sim.spec_by_slug("approval");
    assert!(sim.run_until(5_000, |s| s.tasks_of(spec)[0].item.state
        == WorkItemState::Done));

    put(
        &sim,
        &[
            ("One", "AC-01"),
            ("Two, again", "AC-02"),
            ("Three, again", "AC-02"),
            ("Four", "AC-02"),
        ],
    );
    let again = saka(&server, &["spec", "approve", "tasks", APPROVAL_PATH]);
    assert_eq!(
        code(&again),
        0,
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert_eq!(
        stdout(&again).trim(),
        "approved approval (4 steps: 1 kept, 2 repinned, 1 added, 0 superseded)"
    );

    put(
        &sim,
        &[
            ("One, rewritten", "AC-01"),
            ("Two, again", "AC-02"),
            ("Three, again", "AC-02"),
            ("Four", "AC-02"),
        ],
    );
    let conflict = saka(&server, &["spec", "approve", "tasks", APPROVAL_PATH]);
    assert_eq!(code(&conflict), 4);
    assert_eq!(stdout(&conflict).trim(), "conflict S1: changed work");

    assert!(sim.run_until(5_000, |s| s.tasks_of(spec)[1].item.state
        == WorkItemState::InStage));
    let busy = saka(
        &server,
        &[
            "spec",
            "withdraw",
            "tasks",
            APPROVAL_PATH,
            "--reason",
            "not needed",
        ],
    );
    assert_eq!(code(&busy), 4);
    assert!(String::from_utf8_lossy(&busy.stderr).contains("spec_has_active_tasks"));

    let missing = saka(&server, &["spec", "withdraw", "tasks", APPROVAL_PATH]);
    assert_eq!(code(&missing), 2);

    sim.world
        .put_doc("docs/specs/solo.md", one_step_spec("Solo", "").as_bytes());
    let solo = saka(&server, &["spec", "approve", "tasks", "docs/specs/solo.md"]);
    assert_eq!(code(&solo), 0, "{}", String::from_utf8_lossy(&solo.stderr));
    let withdrawn = saka(
        &server,
        &[
            "spec",
            "withdraw",
            "tasks",
            "docs/specs/solo.md",
            "--reason",
            "not needed",
        ],
    );
    assert_eq!(
        code(&withdrawn),
        0,
        "{}",
        String::from_utf8_lossy(&withdrawn.stderr)
    );
    assert_eq!(stdout(&withdrawn).trim(), "withdrawn solo");
}

#[test]
fn task_and_permission_commands_drive_a_session_and_remembered_rules() {
    use saka_core::vocab::SessionState;
    use saka_testkit::{AgentOutcome, AgentScript};

    let mut sim = Sim::new(SimConfig {
        agent: AgentScript {
            outcome: AgentOutcome::Hang,
            ask: Some((
                "Bash".into(),
                serde_json::json!({ "command": "npm install" }),
            )),
            ..AgentScript::default()
        },
        ..SimConfig::default()
    });
    let server = serve(&sim);
    sim.approve();
    assert!(sim.run_until(5_000, |s| !s.task(1).permissions.is_empty()));
    let task = sim.task(1).item.id.to_string();
    let request = sim.task(1).permissions[0].id.to_string();
    let err = |o: &Output| String::from_utf8_lossy(&o.stderr).into_owned();

    let answered = saka(
        &server,
        &["permission", "answer", &request, "allow", "--remember"],
    );
    assert_eq!(code(&answered), 0, "{}", err(&answered));
    let rules = saka(&server, &["permission", "rules", "tasks"]);
    assert_eq!(code(&rules), 0, "{}", err(&rules));
    let line = stdout(&rules);
    assert!(line.trim_end().ends_with("Bash  npm install"), "{line}");
    let rule = line.split_whitespace().next().unwrap().to_string();
    assert!(sim.run_until(5_000, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Running)
    }));

    let steered = saka(&server, &["task", "steer", &task, "keep going"]);
    assert_eq!(code(&steered), 0, "{}", err(&steered));
    assert!(stdout(&steered).trim().ends_with("running"));
    let interrupted = saka(&server, &["task", "interrupt", &task]);
    assert_eq!(code(&interrupted), 0, "{}", err(&interrupted));
    assert!(stdout(&interrupted).trim().ends_with("interrupted"));
    assert!(sim.run_until(5_000, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Idle)
    }));
    assert_eq!(code(&saka(&server, &["task", "interrupt", &task])), 4);
    let stopped = saka(&server, &["task", "stop", &task, "--reason", "wrong file"]);
    assert_eq!(code(&stopped), 0, "{}", err(&stopped));
    assert!(stdout(&stopped).trim().ends_with("failed"));
    assert_eq!(
        code(&saka(
            &server,
            &["task", "stop", &task, "--reason", "again"]
        )),
        4
    );

    let timeline = saka(&server, &["task", "timeline", &task]);
    assert_eq!(code(&timeline), 0, "{}", err(&timeline));
    let printed: Vec<String> = stdout(&timeline).lines().map(String::from).collect();
    let session = sim.task(1).session.unwrap().id;
    assert_eq!(printed.len(), sim.timeline(session).len());
    assert!(
        printed.iter().any(|l| l.contains("  steer  keep going")),
        "{printed:?}"
    );
    assert!(printed.iter().any(|l| l.contains("  permission  ")));

    let revoked = saka(
        &server,
        &["permission", "revoke", &rule, "--reason", "too broad"],
    );
    assert_eq!(code(&revoked), 0, "{}", err(&revoked));
    assert_eq!(stdout(&revoked).trim(), format!("{rule} revoked"));
    assert_eq!(
        code(&saka(
            &server,
            &["permission", "revoke", &rule, "--reason", "again"]
        )),
        4
    );
}

fn saka_with_stdin(server: &str, args: &[&str], input: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new(SAKA)
        .arg("--server")
        .arg(server)
        .args(args)
        .env_remove("SAKA_CONFIG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stored_secret(sim: &Sim, name: &str) -> Option<String> {
    use saka_ports::{Sealed, SecretStore};
    let rows = sim
        .engine()
        .store()
        .read(|c| saka_store::read::project_secrets(c, sim.project_id))
        .unwrap();
    rows.into_iter().find(|r| r.name == name).map(|r| {
        let sealed = Sealed {
            nonce: r.nonce,
            ciphertext: r.ciphertext,
        };
        let scope = saka_core::secret::SecretScope::Project {
            project_id: sim.project_id,
            name,
        };
        sim.world
            .open(&scope, &sealed)
            .unwrap()
            .expose()
            .to_string()
    })
}

#[test]
fn secret_commands_read_the_value_from_stdin_only() {
    let sim = Sim::new(SimConfig::default());
    let server = serve(&sim);

    let set = saka_with_stdin(
        &server,
        &["secret", "set", "tasks", "NPM_TOKEN"],
        "npm-token-abcdef\nignored second line\n",
    );
    assert_eq!(code(&set), 0, "{}", String::from_utf8_lossy(&set.stderr));
    assert_eq!(stdout(&set).trim(), "secret NPM_TOKEN set");
    assert_eq!(
        stored_secret(&sim, "NPM_TOKEN").as_deref(),
        Some("npm-token-abcdef")
    );

    let multi = saka_with_stdin(
        &server,
        &["secret", "set", "tasks", "TLS_KEY", "--multiline"],
        "line one\r\nline two\n",
    );
    assert_eq!(
        code(&multi),
        0,
        "{}",
        String::from_utf8_lossy(&multi.stderr)
    );
    assert_eq!(
        stored_secret(&sim, "TLS_KEY").as_deref(),
        Some("line one\r\nline two\n")
    );

    let short = saka_with_stdin(&server, &["secret", "set", "tasks", "NPM_TOKEN"], "tiny\n");
    assert_eq!(code(&short), 1);
    let stderr = String::from_utf8_lossy(&short.stderr);
    assert!(stderr.contains("secret_too_short"), "{stderr}");
    assert!(!stderr.contains("tiny"), "{stderr}");

    for flag in ["--value", "--secret"] {
        let refused = saka_with_stdin(
            &server,
            &[
                "secret",
                "set",
                "tasks",
                "NPM_TOKEN",
                flag,
                "argv-value-123",
            ],
            "",
        );
        assert_eq!(code(&refused), 2, "{flag} is not an option");
    }
    assert_eq!(
        stored_secret(&sim, "NPM_TOKEN").as_deref(),
        Some("npm-token-abcdef")
    );

    let list = saka(&server, &["secret", "list", "tasks"]);
    assert_eq!(code(&list), 0);
    let names: Vec<String> = stdout(&list)
        .lines()
        .map(|l| l.split('\t').next().unwrap().to_string())
        .collect();
    assert_eq!(names, ["NPM_TOKEN", "TLS_KEY"]);
    assert!(!stdout(&list).contains("npm-token"));

    let rm = saka(&server, &["secret", "rm", "tasks", "TLS_KEY"]);
    assert_eq!(code(&rm), 0);
    assert_eq!(stdout(&rm).trim(), "secret TLS_KEY removed");
    assert_eq!(stored_secret(&sim, "TLS_KEY"), None);
    let again = saka(&server, &["secret", "rm", "tasks", "TLS_KEY"]);
    assert_eq!(code(&again), 0, "removing twice is fine");
}

#[test]
fn serve_refuses_a_secret_key_that_is_not_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), &own_listen());
    let key = dir.path().join("secret.key");
    std::fs::write(&key, [0u8; 32]).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = run_saka(&config, &["serve"]);
    assert_eq!(code(&out), 2);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("\"code\":\"config_key_file_mode\""),
        "{stderr}"
    );
    assert!(!dir.path().join("data").join("saka.db").exists());
}

fn private_file(path: &Path, bytes: &[u8], mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn webhook_config(
    dir: &Path,
    listen: &str,
    hooks: &str,
    secret: Option<&Path>,
) -> std::path::PathBuf {
    let key = dir.join("app.pem");
    private_file(&key, b"not a real key", 0o600);
    let secret_line = secret.map_or(String::new(), |s| {
        format!("webhook_secret_file = \"{}\"\n", s.display())
    });
    write_config(
        dir,
        &format!(
            "listen = \"{listen}\"\n\n[hooks]\nlisten = \"{hooks}\"\n\n[[scm]]\nid = \"github\"\nprovider = \"github\"\napp_id = 1\nprivate_key_file = \"{}\"\n{secret_line}",
            key.display()
        ),
    )
}

#[test]
fn serve_refuses_webhook_settings_it_cannot_use_safely() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("hook.secret");
    private_file(&secret, &[b'k'; 40], 0o600);
    let cases = [
        (
            "127.0.0.1:7420",
            "127.0.0.1:7420",
            Some(&secret),
            "config_invalid",
        ),
        ("127.0.0.1:7420", "nope", Some(&secret), "config_invalid"),
        ("127.0.0.1:7420", "127.0.0.1:7421", None, "config_invalid"),
    ];
    for (listen, hooks, secret, expected) in cases {
        let config = webhook_config(dir.path(), listen, hooks, secret.map(|p| p.as_path()));
        let out = run_saka(&config, &["serve"]);
        assert_eq!(code(&out), 2, "{hooks}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(expected), "{hooks}: {stderr}");
    }
    private_file(&secret, &[b'k'; 40], 0o644);
    let config = webhook_config(
        dir.path(),
        "127.0.0.1:7420",
        "127.0.0.1:7421",
        Some(&secret),
    );
    let stderr = String::from_utf8_lossy(&run_saka(&config, &["serve"]).stderr).to_string();
    assert!(
        stderr.contains("\"code\":\"config_key_file_mode\""),
        "{stderr}"
    );
    private_file(&secret, b"short\n", 0o600);
    let out = run_saka(&config, &["serve"]);
    assert_eq!(code(&out), 2);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("\"code\":\"config_webhook_secret_invalid\""),
        "{stderr}"
    );
    assert!(!dir.path().join("data").join("saka.db").exists());
}

fn http_post(port: u16, path: &str, body: &str) -> Option<(u16, String)> {
    http_post_with(port, path, &[], body)
}

fn http_post_with(
    port: u16,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Option<(u16, String)> {
    use std::io::Write;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let extra: String = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: hooks.example\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut text = String::new();
    s.read_to_string(&mut text).ok()?;
    let status = text.split_whitespace().nth(1)?.parse().ok()?;
    let body = text
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_string();
    Some((status, body))
}

#[test]
fn serve_opens_a_webhook_listener_that_only_serves_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let (api, hooks) = (free_port(), free_port());
    let secret_value = "a-webhook-secret-that-is-long-enough-1234";
    let secret = dir.path().join("hook.secret");
    private_file(&secret, format!("{secret_value}\n").as_bytes(), 0o600);
    let config = webhook_config(
        dir.path(),
        &format!("127.0.0.1:{api}"),
        &format!("127.0.0.1:{hooks}"),
        Some(&secret),
    );
    let mut child = Command::new(SAKA)
        .arg("serve")
        .env("SAKA_CONFIG", &config)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && http_get(api, "/readyz").map(|r| r.0) != Some(200) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        http_get(api, "/readyz").map(|r| r.0),
        Some(200),
        "never ready"
    );
    assert_eq!(http_get(hooks, "/api/v1/meta").map(|r| r.0), Some(404));
    assert_eq!(http_get(hooks, "/readyz").map(|r| r.0), Some(404));
    assert_eq!(http_get(hooks, "/hooks/github").map(|r| r.0), Some(405));
    let unsigned = http_post(hooks, "/hooks/github", "{}").unwrap();
    assert_eq!(unsigned.0, 401, "{}", unsigned.1);
    assert!(
        unsigned.1.contains("webhook_signature_invalid"),
        "{}",
        unsigned.1
    );
    assert_eq!(
        http_post(hooks, "/hooks/elsewhere", "{}").map(|r| r.0),
        Some(404)
    );

    let pid = child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    assert!(child.wait().unwrap().success());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.contains("listening for webhooks"), "{stderr}");
    assert!(stderr.contains("scm.webhook.rejected"), "{stderr}");
    assert!(
        !stderr.contains(secret_value),
        "the secret reached the logs"
    );
}

fn gitlab_config(dir: &Path, extra: &str) -> std::path::PathBuf {
    write_config(
        dir,
        &format!(
            "{}[[scm]]\nid = \"gitlab\"\nprovider = \"gitlab\"\ntoken_file = \"{}\"\n{extra}",
            own_listen(),
            dir.join("gitlab.token").display()
        ),
    )
}

#[test]
fn serve_refuses_a_gitlab_integration_it_cannot_use() {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("gitlab.token");
    let cases = [
        (None, "", "config_key_file_unreadable"),
        (Some(0o644), "", "config_key_file_mode"),
        (
            Some(0o600),
            "merge_method = \"rebase\"\n",
            "config_merge_method_unsupported",
        ),
        (
            Some(0o600),
            "client_id = \"Iv23liAbCdEfGh123456\"\n",
            "config_user_authorship_unsupported",
        ),
    ];
    for (mode, extra, expected) in cases {
        if let Some(mode) = mode {
            private_file(&token, b"glpat-not-a-real-token\n", mode);
        }
        let config = gitlab_config(dir.path(), extra);
        let out = run_saka(&config, &["serve"]);
        assert_eq!(code(&out), 2, "{expected}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(expected), "{expected}: {stderr}");
        assert!(!stderr.contains("glpat-not-a-real-token"), "{stderr}");
    }
    let config = write_config(
        dir.path(),
        &format!(
            "{}[[scm]]\nid = \"gitlab\"\nprovider = \"gitlab\"\napp_id = 1\n",
            own_listen()
        ),
    );
    let out = run_saka(&config, &["serve"]);
    assert_eq!(code(&out), 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("config_invalid"));
}

#[test]
fn serve_routes_webhooks_to_a_gitlab_integration_next_to_a_github_one() {
    let dir = tempfile::tempdir().unwrap();
    let (api, hooks) = (free_port(), free_port());
    let key = dir.path().join("app.pem");
    private_file(&key, b"not a real key", 0o600);
    let token = dir.path().join("gitlab.token");
    private_file(&token, b"glpat-not-a-real-token\n", 0o600);
    let hook_secret = "a-gitlab-secret-token-that-is-long-enough";
    let gitlab_secret = dir.path().join("gitlab.secret");
    private_file(&gitlab_secret, hook_secret.as_bytes(), 0o600);
    let github_secret = dir.path().join("github.secret");
    private_file(&github_secret, &[b'k'; 40], 0o600);
    let config = write_config(
        dir.path(),
        &format!(
            "listen = \"127.0.0.1:{api}\"\n\n[hooks]\nlisten = \"127.0.0.1:{hooks}\"\n\n[[scm]]\nid = \"github\"\nprovider = \"github\"\napp_id = 1\nprivate_key_file = \"{}\"\nwebhook_secret_file = \"{}\"\n\n[[scm]]\nid = \"gitlab\"\nprovider = \"gitlab\"\nweb_url = \"https://git.example.com\"\ntoken_file = \"{}\"\nmerge_method = \"merge\"\nwebhook_secret_file = \"{}\"\n",
            key.display(),
            github_secret.display(),
            token.display(),
            gitlab_secret.display(),
        ),
    );
    let mut child = Command::new(SAKA)
        .arg("serve")
        .env("SAKA_CONFIG", &config)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && http_get(api, "/readyz").map(|r| r.0) != Some(200) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        http_get(api, "/readyz").map(|r| r.0),
        Some(200),
        "never ready"
    );
    let body = r#"{"object_kind":"pipeline","object_attributes":{"sha":"1111111111111111111111111111111111111111","status":"success"},"project":{"path_with_namespace":"acme/app"}}"#;
    let post = |id: &str, token: Option<&str>| {
        let mut headers = vec![("Idempotency-Key", "3f9a1c-delivery")];
        if let Some(t) = token {
            headers.push(("X-Gitlab-Token", t));
        }
        http_post_with(hooks, &format!("/hooks/{id}"), &headers, body).unwrap()
    };
    let wrong = post("gitlab", Some("another-secret-token-0123456789abcdef"));
    assert_eq!(wrong.0, 401, "{}", wrong.1);
    assert!(wrong.1.contains("webhook_signature_invalid"), "{}", wrong.1);
    assert_eq!(post("gitlab", None).0, 401);
    let accepted = post("gitlab", Some(hook_secret));
    assert_eq!(accepted.0, 202, "{}", accepted.1);
    assert_eq!(post("github", Some(hook_secret)).0, 401);

    let pid = child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    assert!(child.wait().unwrap().success());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(!stderr.contains(hook_secret), "the secret reached the logs");
    assert!(
        !stderr.contains("glpat-not-a-real-token"),
        "the token reached the logs"
    );
}

const DRAFT_PATH: &str = "docs/specs/export.md";

fn draft_spec(valid: bool) -> Vec<u8> {
    let criteria = if valid {
        "## Acceptance criteria\n\n- **AC-01**: `tasks export` writes a CSV file.\n\n"
    } else {
        ""
    };
    format!("---\nsaka: spec\n---\n# Export tasks\n\n{criteria}## Done when\n\n- `make test`\n")
        .into_bytes()
}

fn draft_write(path: &str, valid: bool) -> saka_testkit::DraftTurn {
    saka_testkit::DraftTurn::Write {
        path: path.into(),
        bytes: draft_spec(valid),
    }
}

struct Waiting {
    child: std::process::Child,
    id: saka_core::ids::SpecDraftId,
}

fn spawn_wait(server: &str, path: &str) -> Waiting {
    use std::io::BufRead;
    let mut child = Command::new(SAKA)
        .arg("--server")
        .arg(server)
        .args([
            "spec",
            "draft",
            "tasks",
            path,
            "--idea",
            "Export tasks as CSV.",
            "--wait",
        ])
        .env_remove("SAKA_CONFIG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    let id = saka_core::ids::SpecDraftId(line.trim().parse().unwrap_or_else(|_| {
        let mut e = String::new();
        child.stderr.take().unwrap().read_to_string(&mut e).unwrap();
        panic!("no draft id: {line:?} {e}")
    }));
    Waiting { child, id }
}

fn drive(sim: &mut Sim, mut done: impl FnMut(&Sim) -> bool) {
    let started = Instant::now();
    while !done(sim) {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the sim did not get there"
        );
        if matches!(sim.step(), saka_testkit::Step::Idle) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn finished(w: Waiting) -> (i32, String, String) {
    let out = w.child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn draft_state(sim: &Sim, id: saka_core::ids::SpecDraftId) -> saka_core::vocab::SpecDraftState {
    sim.draft(id).draft.state
}

#[test]
fn spec_draft_and_draft_commands_drive_drafts_through_the_api() {
    use saka_core::vocab::{SessionState, SpecDraftState};
    use saka_testkit::{AgentOutcome, AgentScript, DraftTurn};

    let mut sim = Sim::new(SimConfig::default());
    sim.world.put_doc("docs/prd.md", b"# PRD\n");
    let server = serve(&sim);
    let err = |o: &Output| String::from_utf8_lossy(&o.stderr).into_owned();

    let both = saka(
        &server,
        &[
            "spec",
            "draft",
            "tasks",
            DRAFT_PATH,
            "--idea",
            "x",
            "--idea-file",
            "idea.txt",
        ],
    );
    assert_eq!(code(&both), 2, "--idea and --idea-file together");
    assert_eq!(
        code(&saka(&server, &["spec", "draft", "tasks", DRAFT_PATH])),
        2,
        "an idea is required"
    );
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("none.txt");
    assert_eq!(
        code(&saka(
            &server,
            &[
                "spec",
                "draft",
                "tasks",
                DRAFT_PATH,
                "--idea-file",
                missing.to_str().unwrap()
            ]
        )),
        1
    );

    sim.world.script_draft(vec![draft_write(DRAFT_PATH, true)]);
    let idea = dir.path().join("idea.txt");
    std::fs::write(&idea, "Export tasks as CSV.\n").unwrap();
    let created = saka(
        &server,
        &[
            "spec",
            "draft",
            "tasks",
            DRAFT_PATH,
            "--idea-file",
            idea.to_str().unwrap(),
            "--context",
            "docs/prd.md",
            "--locale",
            "id",
        ],
    );
    assert_eq!(code(&created), 0, "{}", err(&created));
    let id = saka_core::ids::SpecDraftId(stdout(&created).trim().parse().unwrap());
    let d = sim.draft(id).draft;
    assert_eq!(d.context_paths, ["docs/prd.md"]);
    assert_eq!(d.idea, "Export tasks as CSV.\n");
    assert_eq!(d.locale.as_str(), "id");
    let taken = saka(
        &server,
        &["spec", "draft", "tasks", DRAFT_PATH, "--idea", "Again."],
    );
    assert_eq!(code(&taken), 4, "409 spec_draft_target_taken");
    assert!(
        err(&taken).contains("spec_draft_target_taken"),
        "{}",
        err(&taken)
    );
    let invalid = saka(
        &server,
        &["spec", "draft", "tasks", "docs/x.txt", "--idea", "x"],
    );
    assert_eq!(code(&invalid), 1, "422 validation_failed");
    drive(&mut sim, |s| {
        draft_state(s, id) == SpecDraftState::Succeeded
    });
    let shown = saka(&server, &["draft", "show", &id.0.to_string()]);
    assert_eq!(code(&shown), 0, "{}", err(&shown));
    assert!(stdout(&shown).contains("succeeded"), "{}", stdout(&shown));
    assert!(stdout(&shown).contains("spec "), "{}", stdout(&shown));
    assert_eq!(
        code(&saka(
            &server,
            &["draft", "cancel", &id.0.to_string(), "--reason", "x"]
        )),
        4
    );

    sim.world
        .script_draft(vec![draft_write("docs/specs/ok.md", true)]);
    let ok = spawn_wait(&server, "docs/specs/ok.md");
    drive(&mut sim, |s| draft_state(s, ok.id).is_finished());
    let spec_id = sim.draft(ok.id).draft.spec_id.unwrap();
    let (c, out, e) = finished(ok);
    assert_eq!(c, 0, "{e}");
    assert!(
        out.contains(&format!("succeeded spec {}", spec_id.0)),
        "{out}"
    );

    sim.world.script_draft(vec![
        draft_write("docs/specs/lint.md", false),
        DraftTurn::Nothing,
        DraftTurn::Nothing,
        DraftTurn::Nothing,
    ]);
    let lint = spawn_wait(&server, "docs/specs/lint.md");
    let lint_id = lint.id;
    drive(&mut sim, |s| draft_state(s, lint.id).is_finished());
    let (c, out, e) = finished(lint);
    assert_eq!(c, 3, "{e}");
    assert!(out.lines().skip(1).all(|l| l.starts_with("line ")), "{out}");
    assert!(out.contains("ac_section_missing"), "{out}");
    assert!(e.contains("lint_rounds_exhausted"), "{e}");

    let lint_draft = lint_id.0.to_string();
    let listed = saka(&server, &["draft", "list", "tasks", "--state", "all"]);
    assert_eq!(code(&listed), 0, "{}", err(&listed));
    assert_eq!(stdout(&listed).lines().count(), 3, "{}", stdout(&listed));
    let active = saka(&server, &["draft", "list", "tasks"]);
    assert_eq!(
        stdout(&active).trim(),
        format!("{lint_draft}  docs/specs/lint.md  failed lint_rounds_exhausted")
    );
    assert_eq!(
        code(&saka(
            &server,
            &["draft", "list", "tasks", "--state", "done"]
        )),
        2
    );
    let timeline = saka(&server, &["draft", "timeline", &lint_draft]);
    assert_eq!(code(&timeline), 0, "{}", err(&timeline));
    let session = sim.draft(lint_id).session.unwrap().id;
    assert_eq!(
        stdout(&timeline).lines().count(),
        sim.timeline(session).len()
    );
    assert_eq!(
        code(&saka(&server, &["draft", "steer", &lint_draft, "more"])),
        4
    );
    assert_eq!(
        code(&saka(&server, &["draft", "interrupt", &lint_draft])),
        4
    );
    assert_eq!(
        code(&saka(
            &server,
            &["draft", "stop", &lint_draft, "--reason", "x"]
        )),
        4
    );
    let retried = saka(
        &server,
        &[
            "draft",
            "retry",
            &lint_draft,
            "--reason",
            "again",
            "--note",
            "Add criteria.",
        ],
    );
    assert_eq!(code(&retried), 0, "{}", err(&retried));
    assert_eq!(stdout(&retried).trim(), format!("{lint_draft} queued"));
    assert_eq!(
        code(&saka(
            &server,
            &["draft", "retry", &lint_draft, "--reason", "again"]
        )),
        4,
        "409 invalid_state while queued"
    );
    let cancelled = saka(
        &server,
        &["draft", "cancel", &lint_draft, "--reason", "enough"],
    );
    assert_eq!(code(&cancelled), 0, "{}", err(&cancelled));
    assert_eq!(stdout(&cancelled).trim(), format!("{lint_draft} cancelled"));

    sim.world.set_script(AgentScript {
        outcome: AgentOutcome::Hang,
        ..AgentScript::default()
    });
    sim.world
        .script_draft(vec![draft_write("docs/specs/hang.md", true)]);
    let hang = spawn_wait(&server, "docs/specs/hang.md");
    let hang_id = hang.id;
    let running = |s: &Sim| {
        s.draft(hang_id)
            .session
            .is_some_and(|x| x.state == SessionState::Running)
    };
    drive(&mut sim, running);
    let hang_draft = hang_id.0.to_string();
    let steered = saka(&server, &["draft", "steer", &hang_draft, "keep it short"]);
    assert_eq!(code(&steered), 0, "{}", err(&steered));
    assert!(
        stdout(&steered).trim().ends_with("running"),
        "{}",
        stdout(&steered)
    );
    let interrupted = saka(&server, &["draft", "interrupt", &hang_draft]);
    assert_eq!(code(&interrupted), 0, "{}", err(&interrupted));
    drive(&mut sim, |s| {
        s.draft(hang_id)
            .session
            .is_some_and(|x| x.state == SessionState::Idle)
    });
    let stopped = saka(
        &server,
        &["draft", "stop", &hang_draft, "--reason", "wrong idea"],
    );
    assert_eq!(code(&stopped), 0, "{}", err(&stopped));
    drive(&mut sim, |s| draft_state(s, hang_id).is_finished());
    let (c, _, e) = finished(hang);
    assert_eq!(c, 1, "{e}");
    assert!(e.contains("session_failed"), "{e}");

    let gone = spawn_wait(&server, "docs/specs/gone.md");
    let gone_draft = gone.id.0.to_string();
    let cancelled = saka(
        &server,
        &["draft", "cancel", &gone_draft, "--reason", "not needed"],
    );
    assert_eq!(code(&cancelled), 0, "{}", err(&cancelled));
    let (c, _, e) = finished(gone);
    assert_eq!(c, 4, "{e}");
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn project_reload_exits_zero_when_applied_and_one_when_refused() {
    let layout = Layout {
        saka_toml: saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]),
        ..Layout::default()
    };
    let mut sim = Sim::multi_repo(SimConfig::default(), &layout);
    let server = serve(&sim);

    let o = saka(&server, &["project", "reload", "shop"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(stdout(&o).trim(), "project shop unchanged");

    let two = saka_toml(
        "shop",
        &[
            repo_entry("shop", SHOP_REPO, "default = true\n"),
            repo_entry("web", WEB_REPO, ""),
        ],
    );
    sim.world.put_doc("docs/saka.toml", two.as_bytes());
    let id = sim.project_id.0.to_string();
    let o = saka(&server, &["--json", "project", "reload", &id]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let body = json(&o);
    assert_eq!(body["outcome"], "changed");
    assert_eq!(body["project"]["repos"].as_array().unwrap().len(), 2);

    sim.world
        .put_doc("docs/saka.toml", b"[project]\nname = \"shop\"\n");
    let o = saka(&server, &["project", "reload", "shop"]);
    assert_eq!(code(&o), 1);
    assert!(
        stderr(&o).starts_with("error: project_config_invalid "),
        "{}",
        stderr(&o)
    );

    sim.world.put_doc("docs/saka.toml", two.as_bytes());
    sim.approve();
    let only_web = saka_toml("shop", &[repo_entry("web", WEB_REPO, "")]);
    sim.world.put_doc("docs/saka.toml", only_web.as_bytes());
    let o = saka(&server, &["project", "reload", "shop"]);
    assert_eq!(code(&o), 1, "a refused reload exits 1, also for a conflict");
    assert!(
        stderr(&o).starts_with("error: project_repo_in_use "),
        "{}",
        stderr(&o)
    );
}

#[test]
fn serve_refuses_a_config_poll_outside_its_range() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(dir.path(), "[workspace]\nconfig_poll_secs = 10\n");
    let out = run_saka(&path, &["serve"]);
    assert_eq!(code(&out), 2);
    let err = stderr(&out);
    assert!(err.contains("config_invalid"), "{err}");
    assert!(err.contains("workspace.config_poll_secs"), "{err}");
}

fn spawn_link(server: &str) -> (std::process::Child, String) {
    use std::io::BufRead;
    let mut child = Command::new(SAKA)
        .arg("--server")
        .arg(server)
        .args(["scm", "link", "github"])
        .env_remove("SAKA_CONFIG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    (child, line)
}

fn authorization_state(sim: &Sim, n: usize) -> Option<saka_core::vocab::AuthorizationState> {
    let rows = sim
        .engine()
        .store()
        .read(|c| {
            let mut stmt =
                c.prepare("SELECT state FROM user_link_authorization ORDER BY created_at, id")?;
            let out = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(out)
        })
        .unwrap();
    rows.get(n).and_then(|s| s.parse().ok())
}

#[test]
fn scm_link_waits_for_the_device_flow_and_exits_by_its_outcome() {
    use saka_core::vocab::AuthorizationState;
    use saka_testkit::DeviceAnswer;
    let mut sim = Sim::new(SimConfig::default());
    sim.world.accounts().set_device_timing(1, 900);
    let server = serve(&sim);

    let missing = saka(&server, &["scm", "status", "github"]);
    assert_eq!(code(&missing), 1);
    assert!(
        stderr(&missing).contains("user_link_not_found"),
        "{}",
        stderr(&missing)
    );

    let (child, prompt) = spawn_link(&server);
    assert_eq!(
        prompt.trim(),
        "Open https://github.com/login/device and enter FAKE-0001"
    );
    sim.world.accounts().answer(DeviceAnswer::Approve);
    drive(&mut sim, |s| {
        authorization_state(s, 0) == Some(AuthorizationState::Completed)
    });
    let out = child.wait_with_output().unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "linked octo");
    assert!(!stdout(&out).contains("fakedevicecode"));

    let status = saka(&server, &["scm", "status", "github"]);
    assert_eq!(code(&status), 0, "{}", stderr(&status));
    assert!(
        stdout(&status).starts_with("active octo refresh_expires_at 20"),
        "{}",
        stdout(&status)
    );

    let quick = saka(&server, &["scm", "link", "github", "--no-wait"]);
    assert_eq!(code(&quick), 0, "{}", stderr(&quick));
    assert!(
        stdout(&quick).starts_with("Open https://github.com/login/device and enter FAKE-"),
        "{}",
        stdout(&quick)
    );
    let (child, prompt) = spawn_link(&server);
    assert_eq!(prompt, stdout(&quick), "the pending code is reused");
    sim.world.accounts().answer(DeviceAnswer::Deny);
    drive(&mut sim, |s| {
        authorization_state(s, 1) == Some(AuthorizationState::Denied)
    });
    let out = child.wait_with_output().unwrap();
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    assert!(stderr(&out).contains("error: denied"), "{}", stderr(&out));

    let unlinked = saka(&server, &["scm", "unlink", "github"]);
    assert_eq!(code(&unlinked), 0, "{}", stderr(&unlinked));
    assert_eq!(stdout(&unlinked).trim(), "unlinked github");
    assert_eq!(code(&saka(&server, &["scm", "status", "github"])), 1);
    let unknown = saka(&server, &["scm", "status", "gitlab"]);
    assert_eq!(code(&unknown), 1);
    assert!(stderr(&unknown).contains("not_found"));
}

#[test]
fn scm_commands_need_an_integration_when_the_config_has_several() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("app.pem");
    private_file(&key, b"not a real key", 0o600);
    let entry = |id: &str| {
        format!(
            "[[scm]]\nid = \"{id}\"\nprovider = \"github\"\napp_id = 1\nprivate_key_file = \"{}\"\n",
            key.display()
        )
    };
    let two = write_config(
        dir.path(),
        &format!("\n{}{}", entry("github"), entry("ghe")),
    );
    for args in [
        &["scm", "status"][..],
        &["scm", "link", "--no-wait"],
        &["scm", "unlink"],
    ] {
        let out = run_saka(&two, args);
        assert_eq!(code(&out), 2, "{args:?}");
        assert!(
            stderr(&out).contains("integration_required"),
            "{}",
            stderr(&out)
        );
        assert!(stderr(&out).contains("\"ghe\""), "{}", stderr(&out));
    }
}

#[test]
fn the_old_git_author_keys_and_a_bad_client_id_refuse_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let old = write_config(dir.path(), "\n[git]\nauthor_name = \"Saka Agent\"\n");
    let out = run_saka(&old, &["serve"]);
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).contains("config_unknown_field"),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("author_name"), "{}", stderr(&out));

    let bad = write_config(
        dir.path(),
        "\n[[scm]]\nid = \"github\"\nprovider = \"github\"\napp_id = 1\nprivate_key_file = \"/k\"\nclient_id = \"not valid!\"\n",
    );
    let out = run_saka(&bad, &["serve"]);
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).contains("\"code\":\"config_invalid\""),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("scm.client_id"), "{}", stderr(&out));
    assert!(!dir.path().join("data").join("saka.db").exists());
}

#[test]
fn project_authorship_prints_the_mode_and_changes_it_or_exits_with_the_refusal() {
    use saka_testkit::DeviceAnswer;
    let mut sim = Sim::new(SimConfig::default());
    let server = serve(&sim);

    let shown = saka(&server, &["project", "authorship", "tasks"]);
    assert_eq!(code(&shown), 0, "{}", stderr(&shown));
    assert_eq!(
        stdout(&shown).trim(),
        "mode bot author Saka Agent <agent@saka.invalid>"
    );
    let refused = saka(
        &server,
        &["project", "authorship", "tasks", "--mode", "user"],
    );
    assert_eq!(code(&refused), 4, "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("user_link_missing"),
        "{}",
        stderr(&refused)
    );
    let pat = saka(
        &server,
        &["project", "authorship", "tasks", "--mode", "user-pat"],
    );
    assert_eq!(code(&pat), 4);
    assert!(
        stderr(&pat).contains("user_token_missing"),
        "{}",
        stderr(&pat)
    );
    let bad = saka(
        &server,
        &["project", "authorship", "tasks", "--mode", "robot"],
    );
    assert_eq!(code(&bad), 2);

    let (child, _) = spawn_link(&server);
    sim.world.accounts().answer(DeviceAnswer::Approve);
    drive(&mut sim, |s| {
        authorization_state(s, 0) == Some(saka_core::vocab::AuthorizationState::Completed)
    });
    assert_eq!(code(&child.wait_with_output().unwrap()), 0);
    let user = saka(
        &server,
        &["project", "authorship", "tasks", "--mode", "user"],
    );
    assert_eq!(code(&user), 0, "{}", stderr(&user));
    assert_eq!(
        stdout(&user).trim(),
        "mode user author Octo Cat <1+octo@users.noreply.github.com>"
    );
    let as_json = saka(&server, &["--json", "project", "authorship", "tasks"]);
    assert_eq!(json(&as_json)["mode"], "user");
    let unlink = saka(&server, &["scm", "unlink", "github"]);
    assert_eq!(code(&unlink), 1);
    assert!(
        stderr(&unlink).contains("user_link_in_use"),
        "{}",
        stderr(&unlink)
    );
    let bot = saka(
        &server,
        &["project", "authorship", "tasks", "--mode", "bot"],
    );
    assert_eq!(code(&bot), 0, "{}", stderr(&bot));
    assert_eq!(code(&saka(&server, &["scm", "unlink", "github"])), 0);
}

#[test]
fn project_user_token_reads_one_line_from_stdin_and_the_mode_can_then_use_it() {
    let sim = Sim::new(SimConfig::default());
    sim.world.accounts().set_user(7, "ann", None);
    let pat = sim.world.accounts().issue_pat(None);
    let server = serve(&sim);

    let classic = saka_with_stdin(
        &server,
        &["project", "user-token", "set", "tasks"],
        &format!("ghp_{}\n", "a".repeat(36)),
    );
    assert_eq!(code(&classic), 1);
    assert!(
        stderr(&classic).contains("user_token_not_fine_grained"),
        "{}",
        stderr(&classic)
    );
    assert!(!stderr(&classic).contains("ghp_"), "{}", stderr(&classic));

    let set = saka_with_stdin(
        &server,
        &["project", "user-token", "set", "tasks"],
        &format!("{pat}\nignored second line\n"),
    );
    assert_eq!(code(&set), 0, "{}", stderr(&set));
    assert_eq!(stdout(&set).trim(), "user token of ann set, no expiry");
    assert!(!stdout(&set).contains(&pat) && !stderr(&set).contains(&pat));

    let refused = saka_with_stdin(
        &server,
        &["project", "user-token", "set", "tasks", "--value", &pat],
        "",
    );
    assert_eq!(code(&refused), 2, "the token is never an argument");

    let mode = saka(
        &server,
        &["project", "authorship", "tasks", "--mode", "user-pat"],
    );
    assert_eq!(code(&mode), 0, "{}", stderr(&mode));
    assert_eq!(
        stdout(&mode).trim(),
        "mode user_pat author ann <7+ann@users.noreply.github.com>"
    );
    let in_use = saka(&server, &["project", "user-token", "rm", "tasks"]);
    assert_eq!(code(&in_use), 4);
    assert!(
        stderr(&in_use).contains("user_token_in_use"),
        "{}",
        stderr(&in_use)
    );
    let bot = saka(
        &server,
        &["project", "authorship", "tasks", "--mode", "bot"],
    );
    assert_eq!(code(&bot), 0, "{}", stderr(&bot));
    let rm = saka(&server, &["project", "user-token", "rm", "tasks"]);
    assert_eq!(code(&rm), 0, "{}", stderr(&rm));
    assert_eq!(stdout(&rm).trim(), "user token removed");
    assert_eq!(
        code(&saka(&server, &["project", "user-token", "rm", "tasks"])),
        0,
        "removing twice is fine"
    );
}

#[test]
fn serve_refuses_a_hook_it_cannot_run() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(
        dir.path(),
        &format!(
            "{}[[hook]]\nname = \"deploy\"\nproject = \"tasks\"\non = \"merged\"\naction = \"webhook\"\nurl = \"http://ci.example.com/x\"\nsecret = \"KEY_NAME\"\n",
            own_listen()
        ),
    );
    let out = run_saka(&config, &["serve"]);
    assert_eq!(code(&out), 2);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("config_hook_invalid"), "{stderr}");
    assert!(stderr.contains("hook_url_invalid"), "{stderr}");
}

#[test]
fn hook_commands_list_hooks_show_runs_and_run_one_again() {
    use saka_core::hook::{self, RawHook};
    let mut sim = Sim::new(SimConfig {
        hooks: hook::parse_all(&[RawHook {
            name: "announce".into(),
            project: "tasks".into(),
            on: "merged".into(),
            action: "webhook".into(),
            url: Some("https://ci.example.com/saka?token=1".into()),
            secret: Some("KEY_NAME".into()),
            ..RawHook::default()
        }])
        .unwrap(),
        ..SimConfig::default()
    });
    sim.approve();
    assert!(sim.run_to_shipped(8_000));
    assert!(
        sim.run_until(8_000, |s| s.events_of("delivery.hook_run.finished").len()
            == 2)
    );
    let server = serve(&sim);

    let listed = saka(&server, &["hook", "list", "tasks"]);
    assert_eq!(code(&listed), 0);
    assert_eq!(
        stdout(&listed).trim(),
        "announce  on merged  after  webhook"
    );
    let json_listed = saka(&server, &["--json", "hook", "list", "tasks"]);
    assert_eq!(
        json(&json_listed)["items"][0]["target"]["url"],
        "https://ci.example.com/saka"
    );

    let runs = saka(
        &server,
        &["--json", "hook", "runs", "tasks", "--state", "failed"],
    );
    assert_eq!(code(&runs), 0);
    let items = json(&runs)["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2, "the signing secret was never set");
    assert_eq!(items[0]["error"]["code"], "hook_secret_missing");
    let id = items[0]["id"].as_str().unwrap().to_string();

    let again = saka(
        &server,
        &["hook", "rerun", &id, "--reason", "secret is set now"],
    );
    assert_eq!(
        code(&again),
        0,
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert!(
        stdout(&again).trim().ends_with("pending"),
        "{}",
        stdout(&again)
    );
    let pending = saka(&server, &["hook", "runs", "tasks", "--state", "pending"]);
    assert_eq!(stdout(&pending).lines().count(), 1);
    let new_id = stdout(&pending)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let too_early = saka(&server, &["hook", "rerun", &new_id, "--reason", "now"]);
    assert_eq!(code(&too_early), 4, "409 hook_run_pending");
    let unknown = saka(
        &server,
        &[
            "hook",
            "rerun",
            "0193f0c4-0000-7000-8000-000000000000",
            "--reason",
            "x",
        ],
    );
    assert_eq!(code(&unknown), 1);
}

#[test]
fn serve_refuses_a_host_hook_when_other_users_can_write_the_config() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(
        dir.path(),
        &format!(
            "{}[[hook]]\nname = \"host\"\nproject = \"tasks\"\non = \"merged\"\naction = \"run_host\"\ncommand = \"./deploy.sh\"\n",
            own_listen()
        ),
    );
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o666)).unwrap();
    let out = run_saka(&config, &["serve"]);
    assert_eq!(code(&out), 2);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("config_hook_file_writable"), "{stderr}");
}

#[test]
fn review_takes_a_pull_request_and_exits_with_the_documented_codes() {
    let mut text = saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]);
    text.push_str("\n[workflow]\npreset = \"review_only\"\n\n[workflow.change_request]\ntrigger = \"manual\"\n");
    let layout = Layout {
        saka_toml: text,
        ..Layout::default()
    };
    let sim = Sim::multi_repo(SimConfig::default(), &layout);
    let number = sim.world.open_external_change(
        SHOP_REPO,
        saka_testkit::ChangeSpec::new("Add retries", "alice", "feature/retries"),
    );
    let server = serve(&sim);

    let taken = saka(&server, &["review", &number.to_string()]);
    assert_eq!(
        code(&taken),
        0,
        "{}",
        String::from_utf8_lossy(&taken.stderr)
    );
    assert_eq!(
        stdout(&taken).trim(),
        format!("review requested shop#{number} awaiting_review")
    );

    let again = saka(&server, &["review", &number.to_string(), "--repo", "shop"]);
    assert_eq!(code(&again), 4, "an already requested head is a refusal");
    assert!(
        String::from_utf8_lossy(&again.stderr).contains("review_already_requested"),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );

    let missing = saka(&server, &["review", "999"]);
    assert_eq!(code(&missing), 1);
    assert!(String::from_utf8_lossy(&missing.stderr).contains("change_request_not_found"));

    let named = saka(
        &server,
        &["--json", "review", &number.to_string(), "--project", "shop"],
    );
    assert_eq!(code(&named), 4);
}

#[test]
fn the_task_list_shows_where_a_merge_request_stands_in_the_queue() {
    let mut cfg = SimConfig::default();
    cfg.operator.merge = false;
    let mut sim = Sim::new(cfg);
    sim.approve();
    assert!(sim.run_until(5_000, |s| {
        s.task(1).is_at(
            saka_core::vocab::Stage::Merge,
            saka_core::vocab::StagePhase::AwaitingMerge,
        )
    }));
    let task = sim.task(1);
    let engine = sim.engine();
    engine
        .request_merge(
            task.item.id,
            task.item.version,
            None,
            &engine.cause("human:operator:local"),
        )
        .unwrap();
    drop(engine);
    let server = serve(&sim);

    let listed = saka(&server, &["task", "list", "--state", "in_stage"]);
    assert_eq!(code(&listed), 0);
    assert!(
        stdout(&listed).contains("merge/queued  queue: main 1 of 1"),
        "{}",
        stdout(&listed)
    );
    let raw = json(&saka(
        &server,
        &["--json", "task", "list", "--state", "in_stage"],
    ));
    assert_eq!(raw["items"][0]["merge_queue"]["size"], 1);
}
