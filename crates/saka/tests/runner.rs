#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a test binary reports on stdout and fails loudly"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use saka_agents::launch::{ClaudeSettings, LaunchSpec, Redaction};
use saka_agents::protocol::{LogEvent, PROTOCOL_VERSION, ServerFrame};
use saka_agents::redact;
use saka_agents::runner::{EXIT_OK, EXIT_PROTOCOL_UNSUPPORTED};
use saka_agents::{AgentsConfig, ClaudeAgents, client::RunnerConnection};
use saka_core::ids::{EnvironmentId, RepoId, SessionId, WorkItemId};
use saka_env::{DirectSupervisor, SystemdSupervisor};
use saka_ports::{
    Agents, EnvRef, RepoRef, RunnerEvent, RunnerEventKind, SessionLaunch, SessionRef, Supervisor,
    UnitLimits,
};
use saka_testkit::fake_claude;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(10);
const KNOWN: &str = "operator-known-value-42";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("0.0.0-fake (Claude Code)");
        return ExitCode::SUCCESS;
    }
    if std::env::var_os(fake_claude::SCRIPT_VAR).is_some() {
        return ExitCode::from(u8::try_from(fake_claude::main()).unwrap_or(1));
    }
    let filter: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let tests: &[(&str, fn())] = &[
        (
            "events_are_in_the_file_before_the_socket_and_the_runner_outlives_the_server",
            events_are_in_the_file_before_the_socket_and_the_runner_outlives_the_server,
        ),
        (
            "an_unsupported_protocol_version_ends_the_runner_with_code_3",
            an_unsupported_protocol_version_ends_the_runner_with_code_3,
        ),
        (
            "a_second_connection_is_refused",
            a_second_connection_is_refused,
        ),
        (
            "secrets_never_reach_the_file_the_socket_or_the_logs",
            secrets_never_reach_the_file_the_socket_or_the_logs,
        ),
        (
            "the_adapter_starts_drives_and_stops_a_session",
            the_adapter_starts_drives_and_stops_a_session,
        ),
        (
            "systemd_runs_the_runner_in_a_limited_unit_and_stop_leaves_nothing",
            systemd_runs_the_runner_in_a_limited_unit_and_stop_leaves_nothing,
        ),
    ];
    let mut failed = Vec::new();
    for (name, test) in tests {
        if !filter.is_empty() && !filter.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let ok = std::panic::catch_unwind(test).is_ok();
        println!("test {name} ... {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failed.push(*name);
        }
    }
    if failed.is_empty() {
        println!("test result: ok");
        ExitCode::SUCCESS
    } else {
        println!("test result: FAILED ({})", failed.join(", "));
        ExitCode::FAILURE
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

struct Fixture {
    dir: tempfile::TempDir,
    session: SessionId,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture {
            dir: tempfile::tempdir().unwrap(),
            session: SessionId(Uuid::now_v7()),
        }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    fn socket(&self) -> PathBuf {
        self.path(&format!("run/saka/runners/{}.sock", self.session))
    }

    fn log(&self) -> PathBuf {
        self.path("session/events.ndjson")
    }

    fn launch(&self, script: &str, secrets: &[String]) -> PathBuf {
        let worktree = self.path("env/worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        let mut env = BTreeMap::new();
        env.insert(fake_claude::SCRIPT_VAR.to_string(), script.to_string());
        env.insert(
            fake_claude::SECRETS_VAR.to_string(),
            serde_json::to_string(secrets).unwrap(),
        );
        env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
        let spec = LaunchSpec {
            session_id: self.session.to_string(),
            protocol_version: PROTOCOL_VERSION,
            socket_path: self.socket().to_string_lossy().into_owned(),
            log_path: self.log().to_string_lossy().into_owned(),
            stderr_path: self
                .path("session/cli.stderr")
                .to_string_lossy()
                .into_owned(),
            cwd: worktree.to_string_lossy().into_owned(),
            program: std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            args: vec![],
            env,
            redaction: Redaction {
                known_values: vec![KNOWN.to_string()],
            },
        };
        std::fs::create_dir_all(self.path("session")).unwrap();
        let file = self.path("session/launch.json");
        std::fs::write(&file, serde_json::to_vec(&spec).unwrap()).unwrap();
        file
    }

    async fn spawn_runner(&self, launch: &Path) -> Child {
        let log = std::fs::File::create(self.path("runner.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_saka"))
            .args(["runner", "--session", &self.session.to_string(), "--launch"])
            .arg(launch)
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        wait_for(|| self.socket().exists()).await;
        child
    }

    async fn connect(&self, from: u64) -> (RunnerConnection, mpsc::UnboundedReceiver<RunnerEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let conn = RunnerConnection::connect(&self.socket(), self.session, from, "test", tx)
            .await
            .unwrap();
        (conn, rx)
        }
    }

    fn file_events(&self) -> Vec<LogEvent> {
        std::fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

async fn wait_for(mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn next_event(
    fx: &Fixture,
    rx: &mut mpsc::UnboundedReceiver<RunnerEvent>,
    want: impl Fn(&RunnerEventKind) -> bool,
) -> RunnerEvent {
    loop {
        let ev = tokio::time::timeout(WAIT, rx.recv())
            .await
            .expect("event in time")
            .expect("connection open");
        let in_file = fx.file_events().iter().any(|f| f.offset == ev.offset);
        assert!(
            in_file,
            "event {} was sent before it was written",
            ev.offset
        );
        if want(&ev.kind) {
            return ev;
        }
    }
}

fn events_are_in_the_file_before_the_socket_and_the_runner_outlives_the_server() {
    block_on(async {
        let fx = Fixture::new();
        let launch = fx.launch("ask", &[]);
        let mut runner = fx.spawn_runner(&launch).await;
        let (conn, mut rx) = fx.connect(0).await;
        conn.send(
            fx.session,
            &ServerFrame::Turn {
                turn: 1,
                text: "work".into(),
            },
        )
        .await
        .unwrap();
        let ask = next_event(&fx, &mut rx, |k| {
            matches!(k, RunnerEventKind::PermissionRequest { .. })
        })
        .await;
        let RunnerEventKind::PermissionRequest {
            request_id, tool, ..
        } = ask.kind
        else {
            unreachable!()
        };
        assert_eq!(tool, "Bash");

        drop(conn);
        drop(rx);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            runner.try_wait().unwrap().is_none(),
            "the runner exited with the server"
        );

        let (conn, mut rx) = fx.connect(0).await;
        assert_eq!(conn.next_offset, fx.file_events().len() as u64);
        for expected in 0..conn.next_offset {
            let ev = tokio::time::timeout(WAIT, rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(ev.offset, expected);
        }
        conn.send(
            fx.session,
            &ServerFrame::PermissionAnswer {
                request_id: request_id.clone(),
                behavior: "allow".into(),
                message: None,
            },
        )
        .await
        .unwrap();
        next_event(&fx, &mut rx, |k| {
            matches!(k, RunnerEventKind::PermissionDelivered { .. })
        })
        .await;
        let said = next_event(
            &fx,
            &mut rx,
            |k| matches!(k, RunnerEventKind::Activity { category, .. } if category == "text"),
        )
        .await;
        assert!(
            matches!(said.kind, RunnerEventKind::Activity { summary, .. } if summary == "allowed: make deploy")
        );

        conn.send(
            fx.session,
            &ServerFrame::PermissionAnswer {
                request_id,
                behavior: "allow".into(),
                message: None,
            },
        )
        .await
        .unwrap();
        next_event(&fx, &mut rx, |k| {
            matches!(k, RunnerEventKind::PermissionDelivered { .. })
        })
        .await;

        drop(conn);
        drop(rx);
        wait_for(|| fx.file_events().iter().any(|e| e.kind == "turn_ended")).await;
        let delivered = fx
            .file_events()
            .iter()
            .filter(|e| e.kind == "permission_delivered")
            .count();
        assert_eq!(delivered, 2);

        let (conn, _rx) = fx.connect(fx.file_events().len() as u64).await;
        conn.send(fx.session, &ServerFrame::Shutdown).await.unwrap();
        let status = tokio::time::timeout(WAIT, runner.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.code(), Some(EXIT_OK));
        assert!(!fx.socket().exists());
    });
}

fn an_unsupported_protocol_version_ends_the_runner_with_code_3() {
    block_on(async {
        let fx = Fixture::new();
        let launch = fx.launch("", &[]);
        let mut runner = fx.spawn_runner(&launch).await;
        let (tx, _rx) = mpsc::unbounded_channel();
        let refused =
            RunnerConnection::connect_with(&fx.socket(), fx.session, 2, 0, "test", tx).await;
        assert_eq!(
            refused.err().map(|e| e.code),
            Some("protocol_unsupported".to_string())
        );
        let status = tokio::time::timeout(WAIT, runner.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.code(), Some(EXIT_PROTOCOL_UNSUPPORTED));
    });
}

fn a_second_connection_is_refused() {
    block_on(async {
        let fx = Fixture::new();
        let launch = fx.launch("", &[]);
        let _runner = fx.spawn_runner(&launch).await;
        let (_first, _rx) = fx.connect(0).await;
        let (tx, _rx2) = mpsc::unbounded_channel();
        let second = RunnerConnection::connect(&fx.socket(), fx.session, 0, "test", tx).await;
        assert_eq!(
            second.err().map(|e| e.code),
            Some("already_connected".to_string())
        );
    });
}

fn secrets_never_reach_the_file_the_socket_or_the_logs() {
    block_on(async {
        let fx = Fixture::new();
        let samples = redact::samples(KNOWN);
        let secrets: Vec<String> = samples.iter().map(|(_, s)| s.clone()).collect();
        let launch = fx.launch("secrets", &secrets);
        let mut runner = fx.spawn_runner(&launch).await;
        let (conn, mut rx) = fx.connect(0).await;
        conn.send(
            fx.session,
            &ServerFrame::Turn {
                turn: 1,
                text: "go".into(),
            },
        )
        .await
        .unwrap();
        let mut socket = String::new();
        loop {
            let ev = tokio::time::timeout(WAIT, rx.recv())
                .await
                .unwrap()
                .unwrap();
            socket.push_str(&format!("{:?}\n", ev.kind));
            if matches!(ev.kind, RunnerEventKind::TurnEnded { .. }) {
                break;
            }
        }
        conn.send(fx.session, &ServerFrame::Shutdown).await.unwrap();
        tokio::time::timeout(WAIT, runner.wait())
            .await
            .unwrap()
            .unwrap();

        let file = std::fs::read_to_string(fx.log()).unwrap();
        let stderr = std::fs::read_to_string(fx.path("session/cli.stderr")).unwrap();
        let logs = std::fs::read_to_string(fx.path("runner.log")).unwrap();
        // The JSON file escapes newlines; compare with the escaped form too.
        for (name, secret) in &samples {
            let escaped = serde_json::to_string(secret).unwrap();
            let escaped = escaped.trim_matches('"');
            for (place, text) in [
                ("events.ndjson", &file),
                ("socket", &socket),
                ("cli.stderr", &stderr),
                ("runner log", &logs),
            ] {
                assert!(
                    !text.contains(secret.as_str()) && !text.contains(escaped),
                    "{name} leaked into {place}"
                );
            }
            let marker = format!("«secret:{name}»");
            assert!(
                file.contains(&marker),
                "{name} not redacted in events.ndjson"
            );
            assert!(
                stderr.contains(&marker),
                "{name} not redacted in cli.stderr"
            );
        }
    });
}

fn settings() -> ClaudeSettings {
    ClaudeSettings {
        command: std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        account_dir: "/nonexistent/claude".into(),
        model: None,
        path: vec!["/usr/bin".into(), "/bin".into()],
        author_name: "Saka Agent".into(),
        author_email: "agent@saka.invalid".into(),
    }
}

fn adapter(
    fx: &Fixture,
    supervisor: Arc<dyn Supervisor>,
    script: &str,
) -> (ClaudeAgents, mpsc::UnboundedReceiver<RunnerEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let cfg = AgentsConfig {
        claude: settings(),
        sessions_dir: fx.path("sessions"),
        runtime_dir: fx.path("run"),
        runner_program: PathBuf::from(env!("CARGO_BIN_EXE_saka")),
        limits: UnitLimits {
            memory_max: "1G".into(),
            cpu_quota: "50%".into(),
            tasks_max: 64,
        },
        known_values: vec![],
        server_version: "test".into(),
        extra_env: vec![(fake_claude::SCRIPT_VAR.into(), script.into())],
    };
    (ClaudeAgents::new(cfg, supervisor, tx), rx)
}

fn launch_for(fx: &Fixture) -> SessionLaunch {
    let root = fx.path("env");
    std::fs::create_dir_all(root.join("worktree")).unwrap();
    SessionLaunch {
        session_id: fx.session,
        work_item_id: WorkItemId(Uuid::now_v7()),
        environment: EnvRef {
            environment_id: EnvironmentId(Uuid::now_v7()),
            repo: RepoRef {
                repo_id: RepoId(Uuid::now_v7()),
                integration_id: "github".into(),
                full_name: "acme/app".into(),
                url: "https://github.com/acme/app".into(),
            },
            branch: "saka/x/s1".into(),
            root_path: root.to_string_lossy().into_owned(),
            worktree_path: root.join("worktree").to_string_lossy().into_owned(),
        },
    }
}

async fn turn_ends(rx: &mut mpsc::UnboundedReceiver<RunnerEvent>) {
    loop {
        let ev = tokio::time::timeout(WAIT, rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let RunnerEventKind::TurnEnded { status, .. } = ev.kind {
            assert_eq!(status, "success");
            return;
        }
    }
}

fn the_adapter_starts_drives_and_stops_a_session() {
    block_on(async {
        let fx = Fixture::new();
        let supervisor = Arc::new(DirectSupervisor::new(fx.path("logs")));
        let (agents, mut rx) = adapter(&fx, supervisor.clone(), "");
        let launched = agents.start(&launch_for(&fx)).await.unwrap();
        assert_eq!(launched.runner_unit, format!("saka-session-{}", fx.session));
        assert_eq!(launched.cli_version.as_deref(), Some("0.0.0-fake"));
        let launch_file = fx.path(&format!("sessions/{}/launch.json", fx.session));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&launch_file)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(fx.path("env/home").is_dir() && fx.path("env/tmp").is_dir());

        let session = SessionRef {
            session_id: fx.session,
            runner_unit: launched.runner_unit.clone(),
            socket_path: launched.socket_path.clone(),
        };
        agents.send_turn(&session, 1, "work").await.unwrap();
        turn_ends(&mut rx).await;
        let pid = supervisor.pid(&launched.runner_unit).unwrap();
        agents
            .stop(fx.session, Some(&launched.runner_unit))
            .await
            .unwrap();
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "the runner is gone"
        );
        assert!(agents.send_turn(&session, 2, "more").await.is_err());
    });
}

fn systemd_runs_the_runner_in_a_limited_unit_and_stop_leaves_nothing() {
    if std::env::var("SAKA_TEST_SYSTEMD").as_deref() != Ok("1") {
        println!("  (skipped: set SAKA_TEST_SYSTEMD=1, or run `cargo xtask test-systemd`)");
        return;
    }
    block_on(async {
        let fx = Fixture::new();
        let (agents, mut rx) = adapter(&fx, Arc::new(SystemdSupervisor), "hang");
        let launched = agents.start(&launch_for(&fx)).await.unwrap();
        let unit = launched.runner_unit.clone();
        assert!(SystemdSupervisor::is_active(&unit).await);
        assert_eq!(
            SystemdSupervisor::property(&unit, "MemoryMax")
                .await
                .as_deref(),
            Some("1073741824")
        );
        assert_eq!(
            SystemdSupervisor::property(&unit, "CPUQuotaPerSecUSec")
                .await
                .as_deref(),
            Some("500ms")
        );
        assert_eq!(
            SystemdSupervisor::property(&unit, "TasksMax")
                .await
                .as_deref(),
            Some("64")
        );
        let session = SessionRef {
            session_id: fx.session,
            runner_unit: unit.clone(),
            socket_path: launched.socket_path,
        };
        agents.send_turn(&session, 1, "work").await.unwrap();
        let processes = SystemdSupervisor::processes(&unit).await;
        assert!(
            processes.len() >= 2,
            "runner and CLI in the unit: {processes:?}"
        );
        while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
            assert!(
                !matches!(ev.kind, RunnerEventKind::TurnEnded { .. }),
                "the hanging turn ended"
            );
        }
        agents.stop(fx.session, Some(&unit)).await.unwrap();
        assert!(!SystemdSupervisor::is_active(&unit).await);
        assert!(SystemdSupervisor::processes(&unit).await.is_empty());
        for pid in processes {
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "process {pid} survived"
            );
        }
    });
}
