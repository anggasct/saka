#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a process test fails loudly"
)]

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use saka_core::decision::Question;
use saka_core::ids::{DecisionRequestId, SessionId};
use saka_core::secret::SecretText;
use saka_mcp::{SOCKET_ENV, TOKEN_ENV, Tokens};
use saka_ports::{AskRefusal, Asked, Decisions};
use serde_json::{Value, json};
use uuid::Uuid;

const TOKEN: &str = "process-test-token-0123456789abcdef";
const WAIT: Duration = Duration::from_secs(20);

struct FixedToken;

impl Tokens for FixedToken {
    fn token(&self, _session: SessionId) -> Option<SecretText> {
        Some(SecretText::new(TOKEN.to_string()))
    }
}

#[derive(Default)]
struct Scripted {
    asked: Mutex<Vec<(SessionId, Question)>>,
    refusals: Mutex<VecDeque<AskRefusal>>,
}

#[async_trait]
impl Decisions for Scripted {
    async fn ask(&self, session: SessionId, question: Question) -> Result<Asked, AskRefusal> {
        self.asked.lock().unwrap().push((session, question));
        match self.refusals.lock().unwrap().pop_front() {
            Some(refusal) => Err(refusal),
            None => Ok(Asked {
                decision_id: DecisionRequestId(Uuid::from_u128(7)),
            }),
        }
    }
}

struct Engine {
    _dir: tempfile::TempDir,
    _rt: tokio::runtime::Runtime,
    socket: PathBuf,
    decisions: Arc<Scripted>,
}

fn engine() -> Engine {
    let dir = tempfile::tempdir().unwrap();
    let socket = std::fs::canonicalize(dir.path()).unwrap().join("mcp.sock");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let decisions = Arc::new(Scripted::default());
    {
        let _guard = rt.enter();
        let listener = saka_mcp::bind(&socket).unwrap();
        rt.spawn(saka_mcp::serve(
            listener,
            Arc::new(FixedToken),
            decisions.clone(),
        ));
    }
    Engine {
        _dir: dir,
        _rt: rt,
        socket,
        decisions,
    }
}

fn session() -> String {
    Uuid::from_u128(1).to_string()
}

fn saka_mcp(socket: Option<&Path>, token: Option<&str>) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_saka"));
    cmd.args(["mcp", "--session", &session()])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(socket) = socket {
        cmd.env(SOCKET_ENV, socket);
    }
    if let Some(token) = token {
        cmd.env(TOKEN_ENV, token);
    }
    cmd.spawn().unwrap()
}

struct Peer {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Peer {
    fn start(engine: &Engine) -> Peer {
        let mut child = saka_mcp(Some(&engine.socket), Some(TOKEN));
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Peer {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, value: Value) {
        writeln!(self.stdin, "{value}").unwrap();
    }

    fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let line = self
                .lines
                .recv_timeout(WAIT)
                .expect("no answer from saka mcp");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                return v;
            }
        }
    }

    fn handshake(&mut self) -> Value {
        let init = self.call(
            1,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "process-test", "version": "0" },
            }),
        );
        self.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        init
    }

    fn finish(mut self) -> std::process::ExitStatus {
        drop(self.stdin);
        self.child.wait().unwrap()
    }
}

fn valid() -> Value {
    json!({
        "question": "Where do we store audio?",
        "options": [{ "label": "Local", "detail": "Simple" }, { "label": "R2" }],
        "context": "The recorder needs a place.",
    })
}

fn text_of(response: &Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn it_serves_initialize_tools_list_and_tools_call_over_stdio() {
    let engine = engine();
    let mut peer = Peer::start(&engine);
    let init = peer.handshake();
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "{init}"
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "saka");

    let listed = peer.call(2, "tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "{listed}");
    assert_eq!(tools[0]["name"], "request_decision");
    assert_eq!(
        tools[0]["inputSchema"]["required"],
        json!(["question", "options"])
    );
    assert_eq!(
        tools[0]["inputSchema"]["properties"]["options"]["minItems"],
        2
    );
    assert_eq!(
        tools[0]["inputSchema"]["properties"]["options"]["maxItems"],
        3
    );

    let called = peer.call(
        3,
        "tools/call",
        json!({ "name": "request_decision", "arguments": valid() }),
    );
    assert_ne!(called["result"]["isError"], true, "{called}");
    let body: Value = serde_json::from_str(&text_of(&called)).unwrap();
    assert_eq!(body["decision_id"], Uuid::from_u128(7).to_string());
    assert_eq!(body["status"], "asked");
    assert!(
        body["instruction"]
            .as_str()
            .unwrap()
            .contains("End your turn")
    );
    {
        let asked = engine.decisions.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, SessionId(Uuid::from_u128(1)));
        assert_eq!(asked[0].1.question, "Where do we store audio?");
        assert_eq!(asked[0].1.options[0].detail.as_deref(), Some("Simple"));
        assert_eq!(asked[0].1.options[1].detail, None);
    }
    assert!(peer.finish().success());
}

#[test]
fn bad_input_is_a_tool_error_and_never_reaches_the_engine() {
    let engine = engine();
    let mut peer = Peer::start(&engine);
    peer.handshake();
    let mut one_option = valid();
    one_option["options"] = json!([{ "label": "Only" }]);
    let mut long = valid();
    long["question"] = json!("x".repeat(501));
    let mut extra = valid();
    extra["urgent"] = json!(true);
    for (id, arguments) in [
        (2, one_option),
        (3, long),
        (4, extra),
        (5, json!({ "question": "no options" })),
    ] {
        let called = peer.call(
            id,
            "tools/call",
            json!({ "name": "request_decision", "arguments": arguments }),
        );
        assert_eq!(called["result"]["isError"], true, "{called}");
        assert_eq!(text_of(&called), "invalid_input");
    }
    assert!(engine.decisions.asked.lock().unwrap().is_empty());
    let unknown = peer.call(6, "tools/call", json!({ "name": "other", "arguments": {} }));
    assert!(unknown["error"].is_object(), "{unknown}");
    assert!(peer.finish().success());
}

#[test]
fn engine_refusals_reach_the_agent_as_tool_errors_with_a_next_step() {
    let engine = engine();
    {
        let mut refusals = engine.decisions.refusals.lock().unwrap();
        refusals.push_back(AskRefusal::OnePerSession);
        refusals.push_back(AskRefusal::LimitReached);
    }
    let mut peer = Peer::start(&engine);
    peer.handshake();
    let call = |peer: &mut Peer, id| {
        peer.call(
            id,
            "tools/call",
            json!({ "name": "request_decision", "arguments": valid() }),
        )
    };
    let first = call(&mut peer, 2);
    assert_eq!(first["result"]["isError"], true);
    assert_eq!(text_of(&first), "one_open_per_session");
    let second = call(&mut peer, 3);
    assert_eq!(second["result"]["isError"], true);
    let text = text_of(&second);
    assert!(text.starts_with("limit_reached"), "{text}");
    assert!(text.contains("Decide yourself"), "{text}");
    assert!(peer.finish().success());
}

fn refused(mut child: Child) {
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "stdout must stay empty, got {:?}",
        out.stdout
    );
}

#[test]
fn a_wrong_token_exits_2_and_writes_nothing_to_stdout() {
    let engine = engine();
    refused(saka_mcp(Some(&engine.socket), Some("not-the-token")));
    assert!(engine.decisions.asked.lock().unwrap().is_empty());
}

#[test]
fn a_missing_socket_or_endpoint_exits_2_and_writes_nothing_to_stdout() {
    let engine = engine();
    refused(saka_mcp(
        Some(&engine.socket.with_file_name("gone.sock")),
        Some(TOKEN),
    ));
    refused(saka_mcp(None, Some(TOKEN)));
    refused(saka_mcp(Some(&engine.socket), None));
}
