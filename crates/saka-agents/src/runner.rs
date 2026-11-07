use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use saka_ports::BestEffort;
use saka_ports::MAX_RESULT_TEXT_BYTES;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use crate::claude::{self, Output, RESULT_SUMMARY_MAX, SUMMARY_MAX};
use crate::launch::LaunchSpec;
use crate::protocol::{self, LogEvent, Malformed, RunnerFrame, ServerFrame};
use crate::redact::Redactor;
use crate::secrets::SessionSecrets;

pub const EXIT_OK: i32 = 0;
pub const EXIT_VERSION_MISMATCH: i32 = 3;
pub const EXIT_LAUNCH_INVALID: i32 = 4;
pub const EXIT_CLI_FAILED: i32 = 5;

const CHANNEL_CAPACITY: usize = 1024;
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const STDIN_CLOSE_WAIT: Duration = Duration::from_secs(10);
const TERM_WAIT: Duration = Duration::from_secs(5);
const ORPHAN_TIMEOUT: Duration = Duration::from_secs(600);

pub fn runner_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

pub async fn run(session_id: &str, launch_path: &Path) -> i32 {
    let launch = match read_launch(launch_path) {
        Ok(l) if l.session_id == session_id => l,
        Ok(_) => {
            tracing::error!(
                code = "launch_invalid",
                "launch.json is for another session"
            );
            return EXIT_LAUNCH_INVALID;
        }
        Err(e) => {
            tracing::error!(code = "launch_invalid", error = %e, "cannot read launch.json");
            return EXIT_LAUNCH_INVALID;
        }
    };
    let secrets = match &launch.secrets_path {
        None => SessionSecrets::default(),
        Some(path) => match SessionSecrets::take(Path::new(path)) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(code = "launch_invalid", error = %e, "cannot read the session secrets");
                return EXIT_LAUNCH_INVALID;
            }
        },
    };
    match Runner::start(launch, secrets).await {
        Ok(runner) => runner.serve().await,
        Err(code) => code,
    }
}

fn read_launch(path: &Path) -> std::io::Result<LaunchSpec> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

struct EventLog {
    file: std::fs::File,
    path: PathBuf,
    next: u64,
}

impl EventLog {
    fn open(path: &Path) -> std::io::Result<EventLog> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let next = match std::fs::File::open(path) {
            Ok(f) => std::io::BufReader::new(f).lines().count() as u64,
            Err(_) => 0,
        };
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        Ok(EventLog {
            file,
            path: path.to_path_buf(),
            next,
        })
    }

    fn append(&mut self, kind: &str, data: Value) -> std::io::Result<LogEvent> {
        let event = LogEvent {
            offset: self.next,
            at: now_ms(),
            kind: kind.to_string(),
            data,
        };
        let mut line = serde_json::to_string(&event).map_err(std::io::Error::other)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.file.sync_data()?;
        self.next += 1;
        Ok(event)
    }

    fn read_from(&self, from: u64) -> std::io::Result<Vec<LogEvent>> {
        let f = std::fs::File::open(&self.path)?;
        let mut out = Vec::new();
        for line in std::io::BufReader::new(f).lines() {
            let event: LogEvent = serde_json::from_str(&line?).map_err(std::io::Error::other)?;
            if event.offset >= from {
                out.push(event);
            }
        }
        Ok(out)
    }
}

enum Incoming {
    Frame(ServerFrame),
    Bad(Malformed),
    Closed,
}

struct Conn {
    id: u64,
    writer: OwnedWriteHalf,
}

enum Flow {
    Continue,
    Shutdown,
    Exit(i32),
}

struct Runner {
    launch: LaunchSpec,
    redactor: Arc<Redactor>,
    log: EventLog,
    listener: UnixListener,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: mpsc::Receiver<String>,
    frames_tx: mpsc::Sender<(u64, Incoming)>,
    frames: mpsc::Receiver<(u64, Incoming)>,
    conn: Option<Conn>,
    next_conn: u64,
    turn: u32,
    turns: BTreeSet<u32>,
    steers: BTreeSet<u32>,
    activity_seen: bool,
    cli_session: Option<String>,
    pending: HashMap<String, Value>,
    answered: HashMap<String, String>,
    control_seq: u64,
}

impl Runner {
    async fn start(launch: LaunchSpec, secrets: SessionSecrets) -> Result<Runner, i32> {
        let redactor = Arc::new(Redactor::named(
            &launch.redaction.known_values,
            &secrets.redact,
        ));
        let mut log = EventLog::open(Path::new(&launch.log_path)).map_err(|e| {
            tracing::error!(code = "log_open_failed", error = %e, "cannot open events.ndjson");
            EXIT_LAUNCH_INVALID
        })?;
        let listener = bind(Path::new(&launch.socket_path)).map_err(|e| {
            tracing::error!(code = "socket_bind_failed", error = %e, "cannot bind the runner socket");
            EXIT_LAUNCH_INVALID
        })?;
        let mut child = match Command::new(&launch.program)
            .args(&launch.args)
            .env_clear()
            .envs(&launch.env)
            .envs(&secrets.env)
            .current_dir(&launch.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                tracing::error!(code = "cli_spawn_failed", error = %e, "cannot start the CLI");
                log.append(
                    "runner_error",
                    json!({ "code": "cli_spawn_failed", "params": {} }),
                )
                .best_effort("record the failed start");
                return Err(EXIT_CLI_FAILED);
            }
        };
        let stdin = child.stdin.take();
        let (stdout_tx, stdout) = mpsc::channel(CHANNEL_CAPACITY);
        if let Some(out) = child.stdout.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if stdout_tx.send(line).await.is_err() {
                        break;
                    }
                }
            });
        }
        if let Some(err) = child.stderr.take() {
            let redactor = Arc::clone(&redactor);
            let path = PathBuf::from(&launch.stderr_path);
            tokio::spawn(async move { copy_stderr(err, &path, &redactor).await });
        }
        let (frames_tx, frames) = mpsc::channel(CHANNEL_CAPACITY);
        let pid = child.id();
        let mut runner = Runner {
            launch,
            redactor,
            log,
            listener,
            child,
            stdin,
            stdout,
            frames_tx,
            frames,
            conn: None,
            next_conn: 0,
            turn: 0,
            turns: BTreeSet::new(),
            steers: BTreeSet::new(),
            activity_seen: false,
            cli_session: None,
            pending: HashMap::new(),
            answered: HashMap::new(),
            control_seq: 0,
        };
        runner
            .emit("cli_started", json!({ "pid": pid, "cli_version": null }))
            .await;
        let id = runner.control_id();
        runner.write_cli(&claude::initialize(&id)).await;
        Ok(runner)
    }

    async fn serve(mut self) -> i32 {
        let mut stdout_open = true;
        let mut exited = false;
        let mut orphan_deadline = tokio::time::Instant::now() + ORPHAN_TIMEOUT;
        let mut heartbeat =
            tokio::time::interval(Duration::from_millis(self.launch.heartbeat_ms.max(1)));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let flow = tokio::select! {
                biased;
                line = self.stdout.recv(), if stdout_open => {
                    match line {
                        Some(line) => self.on_cli_line(&line).await,
                        None => stdout_open = false,
                    }
                    Flow::Continue
                }
                status = self.child.wait(), if !stdout_open && !exited => {
                    exited = true;
                    orphan_deadline = tokio::time::Instant::now() + ORPHAN_TIMEOUT;
                    let (code, signal) = match status {
                        Ok(s) => {
                            use std::os::unix::process::ExitStatusExt;
                            (s.code(), s.signal())
                        }
                        Err(_) => (None, None),
                    };
                    self.emit("cli_exited", json!({ "code": code, "signal": signal })).await;
                    Flow::Continue
                }
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, _)) => self.on_connect(stream).await,
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                        Flow::Continue
                    }
                },
                Some((id, incoming)) = self.frames.recv() => self.on_frame(id, incoming).await,
                _ = tokio::time::sleep_until(orphan_deadline), if exited && self.conn.is_none() => Flow::Shutdown,
                _ = heartbeat.tick(), if self.conn.is_some() => {
                    self.send(&RunnerFrame::Heartbeat { at: now_ms() }).await;
                    Flow::Continue
                }
            };
            match flow {
                Flow::Continue => {}
                Flow::Shutdown => return self.shutdown(exited).await,
                Flow::Exit(code) => {
                    std::fs::remove_file(&self.launch.socket_path)
                        .best_effort("remove the runner socket");
                    return code;
                }
            }
        }
    }

    fn control_id(&mut self) -> String {
        self.control_seq += 1;
        format!("saka-{}", self.control_seq)
    }

    async fn emit(&mut self, kind: &str, mut data: Value) {
        self.redactor.value(&mut data);
        // Cut only after redaction, so a cut never leaves a partial secret the patterns miss.
        for (key, max) in [
            ("summary", SUMMARY_MAX),
            ("result_summary", RESULT_SUMMARY_MAX),
        ] {
            if let Some(text) = data.get(key).and_then(Value::as_str) {
                let cut = claude::cut(text, max);
                data[key] = Value::String(cut);
            }
        }
        if let Some(text) = data.get("result_text").and_then(Value::as_str) {
            data["result_text"] = Value::String(claude::cut_bytes(text, MAX_RESULT_TEXT_BYTES));
        }
        match self.log.append(kind, data) {
            Ok(event) => self.send(&RunnerFrame::Event(event)).await,
            Err(e) => {
                tracing::error!(code = "log_write_failed", error = %e, "cannot append to events.ndjson");
                self.send(&RunnerFrame::Error {
                    code: "log_write_failed".into(),
                })
                .await;
            }
        }
    }

    async fn send(&mut self, frame: &RunnerFrame) {
        if let Some(conn) = &mut self.conn
            && conn
                .writer
                .write_all(protocol::encode(frame).as_bytes())
                .await
                .is_err()
        {
            self.conn = None;
        }
    }

    async fn write_cli(&mut self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        if let Some(stdin) = &mut self.stdin
            && (stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err())
        {
            self.stdin = None;
        }
    }

    async fn on_cli_line(&mut self, line: &str) {
        match claude::parse(line, self.turn) {
            Output::Event(kind, data) => {
                if kind == "session_init" {
                    self.cli_session = data["cli_session_id"].as_str().map(String::from);
                }
                if kind == "activity" {
                    self.activity_seen = true;
                }
                self.emit(kind, data).await;
            }
            // A resumed CLI can answer the first message with an empty result before doing
            // anything; that is not the end of the turn the server asked for.
            Output::TurnEnded {
                num_turns: Some(0), ..
            } if self.launch.resumed && self.turn == 1 && !self.activity_seen => {}
            Output::TurnEnded { data, .. } => self.emit("turn_ended", data).await,
            Output::Permission {
                request_id,
                tool,
                input,
            } => {
                self.pending.insert(request_id.clone(), input.clone());
                self.emit(
                    "permission_request",
                    json!({ "request_id": request_id, "tool": tool, "input": input }),
                )
                .await;
            }
            Output::Nothing => {}
        }
    }

    async fn on_connect(&mut self, stream: UnixStream) -> Flow {
        let (read, mut writer) = stream.into_split();
        if self.conn.is_some() {
            // Read the hello first: closing with unread data resets the connection, and
            // the other side would never see why.
            tokio::spawn(async move {
                let mut lines = BufReader::new(read).lines();
                tokio::time::timeout(HELLO_TIMEOUT, lines.next_line())
                    .await
                    .best_effort("read the hello of a second client");
                let frame = RunnerFrame::Error {
                    code: "already_connected".into(),
                };
                writer
                    .write_all(protocol::encode(&frame).as_bytes())
                    .await
                    .best_effort("refuse a second client");
                writer
                    .shutdown()
                    .await
                    .best_effort("close a refused client");
            });
            return Flow::Continue;
        }
        let mut lines = BufReader::new(read).lines();
        let first = tokio::time::timeout(HELLO_TIMEOUT, lines.next_line()).await;
        let Ok(Ok(Some(first))) = first else {
            return Flow::Continue;
        };
        let from_offset = match protocol::decode::<ServerFrame>(&first) {
            Ok(ServerFrame::Hello {
                server_version,
                from_offset,
            }) if server_version == runner_version() => from_offset,
            Ok(ServerFrame::Hello { server_version, .. }) => {
                tracing::error!(
                    code = "version_mismatch",
                    server_version = %server_version,
                    runner_version = %runner_version(),
                    "the server runs another binary"
                );
                let frame = RunnerFrame::Error {
                    code: "version_mismatch".into(),
                };
                writer
                    .write_all(protocol::encode(&frame).as_bytes())
                    .await
                    .best_effort("report a version mismatch");
                return Flow::Exit(EXIT_VERSION_MISMATCH);
            }
            _ => {
                let frame = RunnerFrame::Error {
                    code: "hello_required".into(),
                };
                writer
                    .write_all(protocol::encode(&frame).as_bytes())
                    .await
                    .best_effort("ask for the hello first");
                return Flow::Continue;
            }
        };
        self.next_conn += 1;
        let id = self.next_conn;
        self.conn = Some(Conn { id, writer });
        let hello = RunnerFrame::Hello {
            runner_version: runner_version(),
            session_id: self.launch.session_id.clone(),
            next_offset: self.log.next,
        };
        self.send(&hello).await;
        match self.log.read_from(from_offset) {
            Ok(events) => {
                for event in events {
                    self.send(&RunnerFrame::Event(event)).await;
                }
            }
            Err(e) => tracing::error!(error = %e, "cannot replay events.ndjson"),
        }
        let tx = self.frames_tx.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                let incoming = match protocol::decode::<ServerFrame>(&line) {
                    Ok(frame) => Incoming::Frame(frame),
                    Err(bad) => Incoming::Bad(bad),
                };
                if tx.send((id, incoming)).await.is_err() {
                    return;
                }
            }
            tx.send((id, Incoming::Closed))
                .await
                .best_effort("report the closed connection");
        });
        Flow::Continue
    }

    async fn on_frame(&mut self, id: u64, incoming: Incoming) -> Flow {
        if self.conn.as_ref().map(|c| c.id) != Some(id) {
            return Flow::Continue;
        }
        match incoming {
            Incoming::Closed => self.conn = None,
            Incoming::Bad(Malformed::Unknown(_)) => {
                self.send(&RunnerFrame::Error {
                    code: "frame_unknown".into(),
                })
                .await;
            }
            Incoming::Bad(Malformed::Invalid) => {
                self.send(&RunnerFrame::Error {
                    code: "frame_invalid".into(),
                })
                .await;
            }
            Incoming::Frame(ServerFrame::Hello { .. }) => {}
            Incoming::Frame(ServerFrame::Turn { turn, text }) => {
                if self.turns.insert(turn) {
                    self.turn = turn;
                    self.activity_seen = false;
                    let message = claude::user_message(&text, self.cli_session.as_deref());
                    self.write_cli(&message).await;
                }
            }
            Incoming::Frame(ServerFrame::PermissionAnswer {
                request_id,
                behavior,
                message,
            }) => {
                if let Some(input) = self.pending.remove(&request_id) {
                    let allow = behavior == "allow";
                    let response =
                        claude::permission_response(&request_id, allow, &input, message.as_deref());
                    self.write_cli(&response).await;
                    self.answered.insert(request_id.clone(), behavior.clone());
                    self.emit(
                        "permission_delivered",
                        json!({ "request_id": request_id, "behavior": behavior }),
                    )
                    .await;
                } else if let Some(previous) = self.answered.get(&request_id).cloned() {
                    self.emit(
                        "permission_delivered",
                        json!({ "request_id": request_id, "behavior": previous }),
                    )
                    .await;
                } else {
                    self.send(&RunnerFrame::Error {
                        code: "request_unknown".into(),
                    })
                    .await;
                }
            }
            Incoming::Frame(ServerFrame::Steer { n, text }) => {
                if self.steers.insert(n) {
                    let turn = self.turn;
                    self.emit("steer", json!({ "n": n, "turn": turn, "text": text }))
                        .await;
                    let message = claude::user_message(&text, self.cli_session.as_deref());
                    self.write_cli(&message).await;
                }
            }
            Incoming::Frame(ServerFrame::Interrupt) => {
                let id = self.control_id();
                self.write_cli(&claude::interrupt(&id)).await;
            }
            Incoming::Frame(ServerFrame::Shutdown) => return Flow::Shutdown,
        }
        Flow::Continue
    }

    async fn shutdown(mut self, exited: bool) -> i32 {
        self.stdin = None;
        if !exited
            && tokio::time::timeout(STDIN_CLOSE_WAIT, self.child.wait())
                .await
                .is_err()
        {
            if let Some(pid) = self
                .child
                .id()
                .and_then(|p| rustix::process::Pid::from_raw(p as i32))
            {
                rustix::process::kill_process(pid, rustix::process::Signal::TERM)
                    .best_effort("ask the CLI to stop");
            }
            if tokio::time::timeout(TERM_WAIT, self.child.wait())
                .await
                .is_err()
            {
                self.child.kill().await.best_effort("kill the CLI");
            }
        }
        std::fs::remove_file(&self.launch.socket_path).best_effort("remove the runner socket");
        EXIT_OK
    }
}

fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

async fn copy_stderr(err: tokio::process::ChildStderr, path: &Path, redactor: &Redactor) {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path);
    let Ok(mut file) = file else {
        return;
    };
    let mut lines = BufReader::new(err).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut redacted = redactor.text(&line).into_owned();
        redacted.push('\n');
        if file.write_all(redacted.as_bytes()).is_err() {
            return;
        }
    }
}
