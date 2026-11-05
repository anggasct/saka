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
use saka_agents::protocol::{LogEvent, RunnerFrame, ServerFrame};
use saka_agents::redact;
use saka_agents::runner::{EXIT_OK, EXIT_VERSION_MISMATCH, runner_version};
use saka_agents::{AgentsConfig, ClaudeAgents, client::RunnerConnection};
use saka_core::authorship::GitIdentity;
use saka_core::ids::{EnvironmentId, RepoId, SessionId, StageRunId, WorkItemId};
use saka_core::model::RunOwner;
use saka_core::secret::{SecretText, SecretVar};
use saka_core::vocab::SecretPurpose;
use saka_env::{DirectSupervisor, SystemdSupervisor};
use saka_ports::{
    Agents, CommandUnit, EnvRef, LaunchProfile, RepoRef, RunnerEvent, RunnerEventKind,
    SessionLaunch, SessionRef, Supervisor, UnitLimits,
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
            "a_server_of_another_version_ends_the_runner_with_code_3",
            a_server_of_another_version_ends_the_runner_with_code_3,
        ),
        (
            "the_runner_sends_heartbeats_that_never_reach_the_file",
            the_runner_sends_heartbeats_that_never_reach_the_file,
        ),
        (
            "a_steer_reaches_the_cli_once_and_is_logged",
            a_steer_reaches_the_cli_once_and_is_logged,
        ),
        (
            "tool_results_are_summarized_redacted_and_cut",
            tool_results_are_summarized_redacted_and_cut,
        ),
        (
            "a_resumed_cli_empty_first_result_is_dropped",
            a_resumed_cli_empty_first_result_is_dropped,
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
            "session_secrets_reach_only_the_cli_env_and_are_redacted_by_name",
            session_secrets_reach_only_the_cli_env_and_are_redacted_by_name,
        ),
        (
            "an_agent_commit_is_authored_as_the_launch_author_and_committed_by_saka",
            an_agent_commit_is_authored_as_the_launch_author_and_committed_by_saka,
        ),
        (
            "the_adapter_launches_in_the_physical_worktree_and_scopes_memory_to_it",
            the_adapter_launches_in_the_physical_worktree_and_scopes_memory_to_it,
        ),
        (
            "a_worktree_the_cli_would_read_as_a_glob_is_never_launched",
            a_worktree_the_cli_would_read_as_a_glob_is_never_launched,
        ),
        (
            "systemd_runs_the_runner_in_a_limited_unit_and_stop_leaves_nothing",
            systemd_runs_the_runner_in_a_limited_unit_and_stop_leaves_nothing,
        ),
        (
            "systemd_runs_recipe_commands_in_a_limited_unit_with_only_their_env",
            systemd_runs_recipe_commands_in_a_limited_unit_with_only_their_env,
        ),
        (
            "systemd_runs_a_host_hook_with_only_the_allowlisted_env_and_keeps_its_output",
            systemd_runs_a_host_hook_with_only_the_allowlisted_env_and_keeps_its_output,
        ),
        (
            "systemd_probe_finds_the_user_manager",
            systemd_probe_finds_the_user_manager,
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
        self.launch_with(script, secrets, false, 10_000)
    }

    fn launch_with(
        &self,
        script: &str,
        secrets: &[String],
        resumed: bool,
        heartbeat_ms: u64,
    ) -> PathBuf {
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
            args: if resumed {
                vec!["--resume".into(), "fake-session".into()]
            } else {
                vec![]
            },
            env,
            redaction: Redaction {
                known_values: vec![KNOWN.to_string()],
            },
            secrets_path: None,
            resumed,
            heartbeat_ms,
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
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let (tx, rx) = mpsc::unbounded_channel();
            match RunnerConnection::connect(
                &self.socket(),
                self.session,
                from,
                &runner_version(),
                tx,
            )
            .await
            {
                Ok(conn) => return (conn, rx),
                Err(e) if e.code == "already_connected" => {
                    assert!(tokio::time::Instant::now() < deadline, "timed out");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("{e:?}"),
            }
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

fn a_server_of_another_version_ends_the_runner_with_code_3() {
    block_on(async {
        let fx = Fixture::new();
        let launch = fx.launch("", &[]);
        let mut runner = fx.spawn_runner(&launch).await;
        let (tx, _rx) = mpsc::unbounded_channel();
        let refused =
            RunnerConnection::connect(&fx.socket(), fx.session, 0, "0.0.0-other", tx).await;
        assert_eq!(
            refused.err().map(|e| e.code),
            Some("version_mismatch".to_string())
        );
        let status = tokio::time::timeout(WAIT, runner.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.code(), Some(EXIT_VERSION_MISMATCH));
    });
}

fn the_runner_sends_heartbeats_that_never_reach_the_file() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    block_on(async {
        let fx = Fixture::new();
        let launch = fx.launch_with("hang", &[], false, 100);
        let _runner = fx.spawn_runner(&launch).await;
        let stream = tokio::net::UnixStream::connect(fx.socket()).await.unwrap();
        let (read, mut write) = stream.into_split();
        let hello = ServerFrame::Hello {
            server_version: runner_version(),
            from_offset: 0,
        };
        write
            .write_all(saka_agents::protocol::encode(&hello).as_bytes())
            .await
            .unwrap();
        let mut lines = BufReader::new(read).lines();
        let mut beats = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1_000);
        while tokio::time::Instant::now() < deadline {
            let Ok(Ok(Some(line))) = tokio::time::timeout_at(deadline, lines.next_line()).await
            else {
                break;
            };
            if let Ok(RunnerFrame::Heartbeat { at }) = saka_agents::protocol::decode(&line) {
                beats.push(at);
            }
        }
        assert!(beats.len() >= 5, "{beats:?}");
        assert!(beats.windows(2).all(|w| w[1] >= w[0]));
        assert!(
            fx.file_events().iter().all(|e| e.kind != "heartbeat"),
            "heartbeats are not events"
        );
    });
}

fn a_steer_reaches_the_cli_once_and_is_logged() {
    block_on(async {
        let fx = Fixture::new();
        let launch = fx.launch("hang", &[]);
        let _runner = fx.spawn_runner(&launch).await;
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
        next_event(&fx, &mut rx, |k| {
            matches!(k, RunnerEventKind::SessionInit { .. })
        })
        .await;
        let steer = ServerFrame::Steer {
            n: 1,
            text: format!("use {KNOWN} instead"),
        };
        conn.send(fx.session, &steer).await.unwrap();
        conn.send(fx.session, &steer).await.unwrap();
        let ev = next_event(&fx, &mut rx, |k| matches!(k, RunnerEventKind::Steer { .. })).await;
        let RunnerEventKind::Steer { n, turn, text } = ev.kind else {
            unreachable!()
        };
        assert_eq!((n, turn), (1, 1));
        assert_eq!(text, "use «secret:known_value» instead");
        tokio::time::sleep(Duration::from_millis(300)).await;
        let steers: Vec<LogEvent> = fx
            .file_events()
            .into_iter()
            .filter(|e| e.kind == "steer")
            .collect();
        assert_eq!(steers.len(), 1, "a repeated steer is sent once");
        assert!(!std::fs::read_to_string(fx.log()).unwrap().contains(KNOWN));
    });
}

fn tool_results_are_summarized_redacted_and_cut() {
    block_on(async {
        let fx = Fixture::new();
        let launch = fx.launch("tools", &[]);
        let _runner = fx.spawn_runner(&launch).await;
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
        let ev = next_event(&fx, &mut rx, |k| {
            matches!(k, RunnerEventKind::Activity { category, .. } if category == "tool_result")
        })
        .await;
        let RunnerEventKind::Activity {
            summary, is_error, ..
        } = ev.kind
        else {
            unreachable!()
        };
        assert_eq!(is_error, Some(true));
        assert!(
            summary.starts_with("token «secret:github_token» xxx"),
            "{summary}"
        );
        assert_eq!(summary.chars().count(), 500);
        let file = std::fs::read_to_string(fx.log()).unwrap();
        assert!(!file.contains(fake_claude::TOOL_OUTPUT_SECRET));
        let used = next_event(&fx, &mut rx, |k| {
            matches!(k, RunnerEventKind::Activity { category, .. } if category == "tool_use")
                || matches!(k, RunnerEventKind::TurnEnded { .. })
        });
        drop(used);
        assert!(
            fx.file_events()
                .iter()
                .any(|e| e.kind == "activity" && e.data["tool"] == "Bash")
        );
    });
}

fn a_resumed_cli_empty_first_result_is_dropped() {
    block_on(async {
        let fx = Fixture::new();
        let launch = fx.launch_with("resume-empty", &[], true, 10_000);
        let _runner = fx.spawn_runner(&launch).await;
        let (conn, mut rx) = fx.connect(0).await;
        conn.send(
            fx.session,
            &ServerFrame::Turn {
                turn: 1,
                text: "continue".into(),
            },
        )
        .await
        .unwrap();
        next_event(&fx, &mut rx, |k| {
            matches!(k, RunnerEventKind::TurnEnded { .. })
        })
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let ended: Vec<LogEvent> = fx
            .file_events()
            .into_iter()
            .filter(|e| e.kind == "turn_ended")
            .collect();
        assert_eq!(ended.len(), 1, "{ended:?}");
        assert_eq!(ended[0].data["result_summary"], "done");
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
        account: "default".into(),
        account_dir: "/nonexistent/claude".into(),
        model: None,
        path: vec!["/usr/bin".into(), "/bin".into()],
        committer: saka(),
    }
}

fn saka() -> GitIdentity {
    GitIdentity {
        name: "Saka Agent".into(),
        email: "agent@saka.invalid".into(),
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
        server_version: runner_version(),
        extra_env: vec![(fake_claude::SCRIPT_VAR.into(), script.into())],
    };
    (ClaudeAgents::new(cfg, supervisor, tx), rx)
}

fn launch_for(fx: &Fixture) -> SessionLaunch {
    launch_in(fx, &fx.path("env"))
}

fn launch_in(fx: &Fixture, root: &Path) -> SessionLaunch {
    std::fs::create_dir_all(root.join("worktree")).unwrap();
    SessionLaunch {
        session_id: fx.session,
        owner: RunOwner::StageRun {
            work_item_id: WorkItemId(Uuid::now_v7()),
            stage_run_id: StageRunId(Uuid::now_v7()),
        },
        environment: EnvRef {
            environment_id: EnvironmentId(Uuid::now_v7()),
            repo: RepoRef {
                repo_id: RepoId(Uuid::now_v7()),
                integration_id: "github".into(),
                full_name: "acme/app".into(),
                url: "https://github.com/acme/app".into(),
            },
            branch: Some("saka/x/s1".into()),
            root_path: root.to_string_lossy().into_owned(),
            worktree_path: root.join("worktree").to_string_lossy().into_owned(),
            vars: Default::default(),
            path_prefix: Vec::new(),
            secrets: Vec::new(),
            extra_dirs: Vec::new(),
        },
        resume_cli_session_id: None,
        profile: LaunchProfile::Task,
        model: None,
        author: saka(),
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

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Seed")
        .env("GIT_AUTHOR_EMAIL", "seed@example.test")
        .env("GIT_COMMITTER_NAME", "Seed")
        .env("GIT_COMMITTER_EMAIL", "seed@example.test")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn an_agent_commit_is_authored_as_the_launch_author_and_committed_by_saka() {
    block_on(async {
        let fx = Fixture::new();
        let supervisor = Arc::new(DirectSupervisor::new(fx.path("logs")));
        let (agents, mut rx) = adapter(&fx, supervisor, "commit");
        let mut launch = launch_for(&fx);
        let worktree = PathBuf::from(&launch.environment.worktree_path);
        git_in(&worktree, &["init", "-q", "-b", "main"]);
        git_in(&worktree, &["commit", "-q", "--allow-empty", "-m", "seed"]);
        launch.author = GitIdentity {
            name: "Octo Cat".into(),
            email: "1+octo@users.noreply.github.com".into(),
        };
        let launched = agents.start(&launch).await.unwrap();
        let session = SessionRef {
            session_id: fx.session,
            runner_unit: launched.runner_unit.clone(),
            socket_path: launched.socket_path.clone(),
        };
        agents.send_turn(&session, 1, "work").await.unwrap();
        turn_ends(&mut rx).await;
        agents
            .stop(fx.session, Some(&launched.runner_unit))
            .await
            .unwrap();
        assert_eq!(
            git_in(&worktree, &["log", "-1", "--format=%an <%ae>|%cn <%ce>"]),
            "Octo Cat <1+octo@users.noreply.github.com>|Saka Agent <agent@saka.invalid>"
        );
        assert_eq!(
            git_in(&worktree, &["log", "-1", "--format=%s"]),
            "feat: fake change"
        );
        let launch_file =
            std::fs::read_to_string(fx.path(&format!("sessions/{}/launch.json", fx.session)))
                .unwrap();
        for marker in ["ghu_", "ghr_", "github_pat_"] {
            assert!(!launch_file.contains(marker), "{marker} in launch.json");
        }
    });
}

fn session_secrets_reach_only_the_cli_env_and_are_redacted_by_name() {
    const SESSION_VALUE: &str = "session-secret-value-1";
    const SETUP_VALUE: &str = "setup-secret-value-22";
    block_on(async {
        let fx = Fixture::new();
        let supervisor = Arc::new(DirectSupervisor::new(fx.path("logs")));
        let (agents, mut rx) = adapter(&fx, supervisor.clone(), "env:TEST_API_KEY,env:NPM_TOKEN");
        let mut launch = launch_for(&fx);
        launch.environment.secrets = vec![
            SecretVar {
                name: "TEST_API_KEY".into(),
                value: SecretText::new(SESSION_VALUE.into()),
                purposes: vec![SecretPurpose::Setup, SecretPurpose::Session],
            },
            SecretVar {
                name: "NPM_TOKEN".into(),
                value: SecretText::new(SETUP_VALUE.into()),
                purposes: vec![SecretPurpose::Setup],
            },
        ];
        let launched = agents.start(&launch).await.unwrap();
        let session = SessionRef {
            session_id: fx.session,
            runner_unit: launched.runner_unit.clone(),
            socket_path: launched.socket_path.clone(),
        };
        agents.send_turn(&session, 1, "work").await.unwrap();
        turn_ends(&mut rx).await;
        agents
            .stop(fx.session, Some(&launched.runner_unit))
            .await
            .unwrap();

        let log = std::fs::read_to_string(&launched.log_path).unwrap();
        assert!(log.contains("TEST_API_KEY=«secret:TEST_API_KEY»"), "{log}");
        assert!(log.contains("NPM_TOKEN="), "{log}");
        let launch_file = fx.path(&format!("sessions/{}/launch.json", fx.session));
        let launch_json = std::fs::read_to_string(launch_file).unwrap();
        for value in [SESSION_VALUE, SETUP_VALUE] {
            assert!(!log.contains(value), "a value reached events.ndjson");
            assert!(!launch_json.contains(value), "a value reached launch.json");
        }
        let left: Vec<_> = std::fs::read_dir(fx.path("run/saka/runners"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".secrets.json"))
            .collect();
        assert!(left.is_empty(), "the runner removes the secrets file");
    });
}

fn settings_arg(spec: &LaunchSpec) -> serde_json::Value {
    let at = spec.args.iter().position(|a| a == "--settings").unwrap();
    serde_json::from_str(&spec.args[at + 1]).unwrap()
}

fn the_adapter_launches_in_the_physical_worktree_and_scopes_memory_to_it() {
    let fx = Fixture::new();
    std::fs::create_dir_all(fx.path("real data")).unwrap();
    std::os::unix::fs::symlink(fx.path("real data"), fx.path("link")).unwrap();
    let (agents, _rx) = adapter(&fx, Arc::new(DirectSupervisor::new(fx.path("logs"))), "");
    let launch = launch_in(&fx, &fx.path("link/env"));
    let (spec, _) = agents
        .launch_spec(Path::new("/usr/bin/claude"), &launch)
        .unwrap();
    let physical = std::fs::canonicalize(fx.path("real data/env/worktree")).unwrap();
    assert_eq!(spec.cwd, physical.to_string_lossy());
    assert_eq!(
        settings_arg(&spec)["claudeMdExcludes"],
        serde_json::json!([format!("!{}/**", physical.display())])
    );
    assert_eq!(spec.env["HOME"], fx.path("link/env/home").to_string_lossy());
}

fn a_worktree_the_cli_would_read_as_a_glob_is_never_launched() {
    block_on(async {
        let fx = Fixture::new();
        let supervisor = Arc::new(DirectSupervisor::new(fx.path("logs")));
        let (agents, _rx) = adapter(&fx, supervisor.clone(), "");
        let launch = launch_in(&fx, &fx.path("en[v]s/e-1"));
        let err = agents.start(&launch).await.unwrap_err();
        let physical = std::fs::canonicalize(fx.path("en[v]s/e-1/worktree")).unwrap();
        assert_eq!(err.code, "envs_dir_unsupported");
        assert!(!err.retryable);
        assert_eq!(err.params["path"], physical.to_string_lossy().as_ref());
        assert!(!fx.path(&format!("sessions/{}", fx.session)).exists());
        assert_eq!(supervisor.pid(&saka_agents::unit_name(fx.session)), None);
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

fn systemd_probe_finds_the_user_manager() {
    if std::env::var("SAKA_TEST_SYSTEMD").as_deref() != Ok("1") {
        println!("  (skipped: set SAKA_TEST_SYSTEMD=1, or run `cargo xtask test-systemd`)");
        return;
    }
    block_on(async {
        assert_eq!(
            SystemdSupervisor.probe().await.unwrap(),
            saka_core::diagnostics::SupervisorProbe::Available
        );
    });
}

fn systemd_runs_a_host_hook_with_only_the_allowlisted_env_and_keeps_its_output() {
    if std::env::var("SAKA_TEST_SYSTEMD").as_deref() != Ok("1") {
        println!("  (skipped: set SAKA_TEST_SYSTEMD=1, or run `cargo xtask test-systemd`)");
        return;
    }
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let runner = saka_env::HostRunner {
            supervisor: Arc::new(SystemdSupervisor),
            limits: UnitLimits {
                memory_max: "1G".into(),
                cpu_quota: "50%".into(),
                tasks_max: 64,
            },
            path: vec!["/usr/bin".into(), "/bin".into()],
            scratch_root: dir.path().join("hooks"),
            logs_dir: dir.path().join("logs"),
        };
        let command = saka_ports::HostCommand {
            name: format!("hook-test-{}", std::process::id()),
            command: "env; exit 4".into(),
            cwd: None,
            env: BTreeMap::from([
                ("SAKA_EVENT".to_string(), "merged".to_string()),
                ("DEPLOY_TOKEN".to_string(), "must-not-pass".to_string()),
            ]),
            scratch_key: "project-1/deploy".into(),
            timeout_ms: 30_000,
        };
        let out = saka_ports::HostCommands::run(&runner, &command)
            .await
            .unwrap();
        assert_eq!((out.exit_code, out.timed_out), (Some(4), false));
        let log = std::fs::read_to_string(&out.log_path).unwrap();
        assert!(log.contains("SAKA_EVENT=merged"), "{log}");
        assert!(
            !log.contains("must-not-pass") && !log.contains("DEPLOY_TOKEN"),
            "{log}"
        );
        let mut names: Vec<&str> = log
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k))
            .filter(|k| !matches!(*k, "PWD" | "SHLVL" | "_" | "OLDPWD"))
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["HOME", "LANG", "PATH", "SAKA_EVENT"], "{log}");
    });
}

fn systemd_runs_recipe_commands_in_a_limited_unit_with_only_their_env() {
    if std::env::var("SAKA_TEST_SYSTEMD").as_deref() != Ok("1") {
        println!("  (skipped: set SAKA_TEST_SYSTEMD=1, or run `cargo xtask test-systemd`)");
        return;
    }
    block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().into_owned();
        let name = format!("saka-env-test-{}", std::process::id());
        let unit = |script: &str, timeout: Duration| CommandUnit {
            name: name.clone(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: cwd.clone(),
            env: BTreeMap::from([
                ("APP_ENV".to_string(), "test".to_string()),
                ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            ]),
            secret_env: Vec::new(),
            limits: UnitLimits {
                memory_max: "1G".into(),
                cpu_quota: "50%".into(),
                tasks_max: 64,
            },
            timeout,
        };
        let supervisor = Arc::new(SystemdSupervisor);

        let done = supervisor
            .run(&unit(
                "env; pwd; echo oops >&2; exit 3",
                Duration::from_secs(30),
            ))
            .await
            .unwrap();
        assert_eq!((done.exit_code, done.timed_out), (Some(3), false));
        let output = String::from_utf8_lossy(&done.output).into_owned();
        let mut names: Vec<&str> = output
            .lines()
            .filter_map(|l| l.split_once('=').map(|(k, _)| k))
            .filter(|k| !matches!(*k, "PWD" | "SHLVL" | "_" | "OLDPWD"))
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["APP_ENV", "PATH"], "{output}");
        assert!(output.contains(&cwd) && output.contains("oops"), "{output}");

        let slow = {
            let supervisor = supervisor.clone();
            let unit = unit("(sleep 60) & sleep 60", Duration::from_secs(3));
            tokio::spawn(async move { supervisor.run(&unit).await })
        };
        let mut processes = Vec::new();
        for _ in 0..50 {
            processes = SystemdSupervisor::processes(&name).await;
            if processes.len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(processes.len() >= 2, "{processes:?}");
        for (property, value) in [
            ("MemoryMax", "1073741824"),
            ("CPUQuotaPerSecUSec", "500ms"),
            ("TasksMax", "64"),
            ("RuntimeMaxUSec", "33s"),
            ("Environment", ""),
        ] {
            assert_eq!(
                SystemdSupervisor::property(&name, property)
                    .await
                    .as_deref(),
                Some(value),
                "{property}"
            );
        }
        let timed_out = slow.await.unwrap().unwrap();
        assert!(timed_out.timed_out && timed_out.exit_code.is_none());

        // A secret reaches the command's env exactly, but never its argv or the unit's properties.
        let value = "tok'en $HOME \"quoted\"\nsecond line";
        let mut with_secret = unit(
            "printf '%s' \"$API_TOKEN\"; sleep 2",
            Duration::from_secs(30),
        );
        with_secret.secret_env = vec![("API_TOKEN".into(), SecretText::new(value.into()))];
        let running = {
            let supervisor = supervisor.clone();
            tokio::spawn(async move { supervisor.run(&with_secret).await })
        };
        let mut seen = Vec::new();
        for _ in 0..50 {
            seen = SystemdSupervisor::processes(&name).await;
            if !seen.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        for pid in &seen {
            let argv = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            assert!(
                !String::from_utf8_lossy(&argv).contains("tok'en"),
                "the value is in argv of {pid}"
            );
        }
        assert_eq!(
            SystemdSupervisor::property(&name, "Environment")
                .await
                .as_deref(),
            Some("")
        );
        let done = running.await.unwrap().unwrap();
        assert_eq!(done.exit_code, Some(0));
        assert_eq!(String::from_utf8_lossy(&done.output), value);
        let secrets_dir = saka_env::runtime_dir().join("saka/secrets");
        assert!(
            !secrets_dir.join(format!("{name}.env")).exists(),
            "the secrets file is removed"
        );
        assert!(!SystemdSupervisor::is_active(&name).await);
        for pid in processes {
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "process {pid} survived"
            );
        }
    });
}
