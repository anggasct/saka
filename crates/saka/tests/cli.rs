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
use saka_testkit::{Sim, SimConfig, one_step_spec};
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
fn migrate_refuses_a_changed_migration_and_a_newer_database() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(dir.path(), "");
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
