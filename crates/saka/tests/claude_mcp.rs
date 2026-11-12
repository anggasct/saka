#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "an opt-in conformance test against the installed CLI fails loudly"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use saka_agents::claude;
use saka_agents::launch::{self, ClaudeSettings, LaunchOptions, LaunchSpec, SessionPaths};
use saka_core::authorship::GitIdentity;
use saka_core::decision::Question;
use saka_core::ids::{DecisionRequestId, SessionId};
use saka_core::secret::SecretText;
use saka_mcp::{TOOL_FULL_NAME, Tokens};
use saka_ports::{AskRefusal, Asked, Decisions};
use serde_json::{Value, json};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(120);
const TOKEN: &str = "conformance-token-0123456789abcdef";

struct FixedToken;

impl Tokens for FixedToken {
    fn token(&self, _session: SessionId) -> Option<SecretText> {
        Some(SecretText::new(TOKEN.to_string()))
    }
}

#[derive(Default)]
struct Recorder {
    asked: Mutex<Vec<(SessionId, Question)>>,
}

#[async_trait]
impl Decisions for Recorder {
    async fn ask(&self, session: SessionId, question: Question) -> Result<Asked, AskRefusal> {
        self.asked.lock().unwrap().push((session, question));
        Ok(Asked {
            decision_id: DecisionRequestId(Uuid::from_u128(7)),
        })
    }
}

#[async_trait]
impl saka_ports::Regressions for Recorder {
    async fn request(
        &self,
        _session: SessionId,
    ) -> Result<saka_ports::Requested, saka_ports::RegressionRefusal> {
        Err(saka_ports::RegressionRefusal::ToolUnavailable)
    }
}

fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let target = line
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some((target, body))
}

fn sse(events: &[(&str, Value)]) -> String {
    events
        .iter()
        .map(|(name, data)| format!("event: {name}\ndata: {data}\n\n"))
        .collect()
}

fn message_start() -> (&'static str, Value) {
    (
        "message_start",
        json!({ "type": "message_start", "message": {
            "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-fake",
            "content": [], "stop_reason": null, "stop_sequence": null,
            "usage": { "input_tokens": 10, "output_tokens": 1 },
        } }),
    )
}

fn end(stop: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "message_delta",
            json!({ "type": "message_delta", "delta": { "stop_reason": stop, "stop_sequence": null },
                    "usage": { "output_tokens": 5 } }),
        ),
        ("message_stop", json!({ "type": "message_stop" })),
    ]
}

fn tool_use_reply(question: &Value) -> String {
    let mut events = vec![
        message_start(),
        (
            "content_block_start",
            json!({ "type": "content_block_start", "index": 0, "content_block": {
                "type": "tool_use", "id": "toolu_1", "name": TOOL_FULL_NAME, "input": {} } }),
        ),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 0, "delta": {
                "type": "input_json_delta", "partial_json": question.to_string() } }),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }),
        ),
    ];
    events.extend(end("tool_use"));
    sse(&events)
}

fn text_reply(text: &str) -> String {
    let mut events = vec![
        message_start(),
        (
            "content_block_start",
            json!({ "type": "content_block_start", "index": 0, "content_block": {
                "type": "text", "text": "" } }),
        ),
        (
            "content_block_delta",
            json!({ "type": "content_block_delta", "index": 0, "delta": {
                "type": "text_delta", "text": text } }),
        ),
        (
            "content_block_stop",
            json!({ "type": "content_block_stop", "index": 0 }),
        ),
    ];
    events.extend(end("end_turn"));
    sse(&events)
}

struct Api {
    url: String,
    bodies: mpsc::Receiver<Value>,
}

fn fake_api(question: Value) -> Api {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, bodies) = mpsc::channel();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let Some((target, body)) = read_request(&mut stream) else {
                continue;
            };
            let (status, content_type, reply) = if target.starts_with("POST /v1/messages") {
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let has_tool = body["tools"]
                    .as_array()
                    .is_some_and(|t| t.iter().any(|t| t["name"] == TOOL_FULL_NAME));
                let answered = body.to_string().contains("tool_result");
                let reply = if has_tool && !answered {
                    tool_use_reply(&question)
                } else {
                    text_reply("Understood.")
                };
                let _ = tx.send(body);
                ("200 OK", "text/event-stream", reply)
            } else {
                ("200 OK", "application/json", String::new())
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    Api { url, bodies }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    account: PathBuf,
    socket: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("envs").join("e-1");
        let account = dir.path().join("account");
        for d in [
            root.join("worktree"),
            root.join("home"),
            root.join("tmp"),
            account.clone(),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        let git = Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.join("worktree"))
            .status()
            .unwrap();
        assert!(git.success());
        let socket = std::fs::canonicalize(dir.path()).unwrap().join("mcp.sock");
        Fixture {
            root: std::fs::canonicalize(&root).unwrap(),
            socket,
            _dir: dir,
            account,
        }
    }

    fn spec(&self, session: SessionId, token: &str, mcp: bool, tools: Option<&str>) -> LaunchSpec {
        let settings = ClaudeSettings {
            command: "claude".into(),
            account: "default".into(),
            account_dir: self.account.to_string_lossy().into_owned(),
            model: None,
            path: std::env::var("PATH")
                .unwrap_or_default()
                .split(':')
                .map(String::from)
                .collect(),
            committer: saka(),
        };
        let program = settings
            .program()
            .expect("`claude` is not on PATH; install Claude Code to run this test");
        let paths = SessionPaths::new(
            &self.root.join("run"),
            &self.root.join("sessions"),
            &session.to_string(),
            &self.root,
            &self.root.join("worktree"),
        );
        std::fs::create_dir_all(&paths.session_dir).unwrap();
        if mcp {
            let config = launch::mcp_config(
                Path::new(env!("CARGO_BIN_EXE_saka")),
                &session.to_string(),
                &self.socket,
                token,
                false,
            );
            std::fs::write(paths.mcp_config_file(), config).unwrap();
        }
        let options = LaunchOptions {
            cwd: &paths.worktree,
            tools,
            model: None,
            resume: None,
            author: &saka(),
            add_dirs: &[],
            isolated: false,
            mcp,
        };
        launch::claude(
            &settings,
            &program,
            &session.to_string(),
            &paths,
            vec![],
            &options,
        )
        .unwrap()
    }
}

fn saka() -> GitIdentity {
    GitIdentity {
        name: "Saka Agent".into(),
        email: "agent@saka.invalid".into(),
    }
}

struct Run {
    init: Value,
    permissions: Vec<Value>,
    result: Value,
    alive_after_result: bool,
}

fn drive(spec: &LaunchSpec, api: &str) -> Run {
    let mut child = Command::new(&spec.program)
        .args(&spec.args)
        .env_clear()
        .envs(&spec.env)
        .env("ANTHROPIC_BASE_URL", api)
        .env("ANTHROPIC_API_KEY", "sk-ant-conformance-not-a-key")
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    for frame in [
        claude::initialize("init-1"),
        claude::user_message("Ask the operator which store to use.", None),
    ] {
        writeln!(stdin, "{frame}").unwrap();
    }
    let (mut init, mut permissions, mut result) = (Value::Null, Vec::new(), Value::Null);
    while let Ok(line) = lines.recv_timeout(WAIT) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v["type"].as_str() {
            Some("system") if v["subtype"] == "init" => init = v,
            Some("control_request") if v["request"]["subtype"] == "can_use_tool" => {
                let answer = claude::permission_response(
                    v["request_id"].as_str().unwrap(),
                    true,
                    &v["request"]["input"],
                    None,
                );
                writeln!(stdin, "{answer}").unwrap();
                permissions.push(v);
            }
            Some("result") => {
                result = v;
                break;
            }
            _ => {}
        }
    }
    let alive_after_result = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();
    Run {
        init,
        permissions,
        result,
        alive_after_result,
    }
}

fn skipped() -> bool {
    if std::env::var("SAKA_TEST_CLAUDE").as_deref() == Ok("1") {
        return false;
    }
    println!("skipped: set SAKA_TEST_CLAUDE=1, or run `cargo xtask test-claude-mcp`");
    true
}

fn question() -> Value {
    json!({
        "question": "Where do we store audio?",
        "options": [
            { "label": "Local disk", "detail": "Simple, no cost" },
            { "label": "R2" },
        ],
        "context": "The recorder needs a place to keep files.",
    })
}

fn check_session(tools: Option<&str>) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let fx = Fixture::new();
    let recorder = Arc::new(Recorder::default());
    {
        let _guard = rt.enter();
        let listener = saka_mcp::bind(&fx.socket).unwrap();
        rt.spawn(saka_mcp::serve(
            listener,
            Arc::new(FixedToken),
            recorder.clone(),
            recorder.clone(),
        ));
    }
    let api = fake_api(question());
    let session = SessionId(Uuid::from_u128(1));
    let run = drive(&fx.spec(session, TOKEN, true, tools), &api.url);

    let tools: Vec<&str> = run.init["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        tools.contains(&TOOL_FULL_NAME),
        "the session does not list {TOOL_FULL_NAME}: {tools:?}"
    );
    assert!(
        run.init["mcp_servers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "saka" && s["status"] == "connected"),
        "the saka MCP server is not connected: {}",
        run.init["mcp_servers"]
    );
    assert!(
        run.permissions
            .iter()
            .any(|p| p["request"]["tool_name"] == TOOL_FULL_NAME),
        "the CLI did not ask about {TOOL_FULL_NAME}: {:?}",
        run.permissions
    );

    let asked = recorder.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1, "the tool call did not reach the engine");
    assert_eq!(asked[0].0, session);
    assert_eq!(asked[0].1.question, "Where do we store audio?");
    assert_eq!(asked[0].1.options.len(), 2);

    let bodies: Vec<String> = api.bodies.try_iter().map(|b| b.to_string()).collect();
    let returned = bodies
        .iter()
        .find(|b| b.contains("tool_result") && b.contains("toolu_1"))
        .expect("the tool result never went back to the model");
    assert!(returned.contains("End your turn"), "{returned}");
    assert!(
        returned.contains(&Uuid::from_u128(7).to_string()),
        "{returned}"
    );

    assert_eq!(run.result["subtype"], "success", "{}", run.result);
    assert!(
        run.alive_after_result,
        "the CLI exited at the end of the turn instead of waiting for the next message"
    );
}

#[test]
fn a_dev_session_sees_the_decision_tool_calls_it_and_ends_its_turn() {
    if !skipped() {
        check_session(None);
    }
}

#[test]
fn an_architect_session_with_a_tool_list_still_sees_the_decision_tool() {
    if !skipped() {
        check_session(Some(launch::ARCHITECT_TOOLS));
    }
}

#[test]
fn a_session_without_the_config_has_no_decision_tool() {
    if skipped() {
        return;
    }
    let fx = Fixture::new();
    let api = fake_api(question());
    let run = drive(
        &fx.spec(SessionId(Uuid::from_u128(2)), TOKEN, false, None),
        &api.url,
    );
    let tools = run.init["tools"].to_string();
    assert!(!tools.contains("mcp__saka__"), "{tools}");
}
