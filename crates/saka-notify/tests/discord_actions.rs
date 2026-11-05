#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert by panicking"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use saka_core::ids::BlockerId;
use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_notify::{ChannelKind, ChannelSpec, ChatConfig, ChatNotifier, Lang, PollTiming};
use saka_ports::{
    ChatAction, ChatActionKind, ChatActions, ChatOutcome, Clock, Notification, Notifier,
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "MTA.SECRET.bot-token";
const CHANNEL: &str = "998877";
const OPERATOR: &str = "4242";
const BLOCKER: u128 = 0x0193_f0c4_0000_7000_8000_0000_0000_0006;

struct Fixed;

impl Clock for Fixed {
    fn now(&self) -> Timestamp {
        Timestamp(0)
    }
}

struct Recorder {
    calls: Mutex<Vec<ChatAction>>,
    outcome: ChatOutcome,
}

#[async_trait]
impl ChatActions for Recorder {
    async fn act(&self, action: &ChatAction) -> ChatOutcome {
        self.calls.lock().unwrap().push(action.clone());
        self.outcome
    }
}

fn recorder(outcome: ChatOutcome) -> Arc<Recorder> {
    Arc::new(Recorder {
        calls: Mutex::new(Vec::new()),
        outcome,
    })
}

enum Cmd {
    Send(Value),
    Close(Option<u16>),
}

struct FakeGateway {
    url: String,
    frames: Arc<Mutex<Vec<(usize, Value)>>>,
    commands: mpsc::UnboundedSender<Cmd>,
}

impl FakeGateway {
    async fn start() -> FakeGateway {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let frames = Arc::new(Mutex::new(Vec::new()));
        let (commands, mut inbox) = mpsc::unbounded_channel();
        let seen = frames.clone();
        let own_url = url.clone();
        tokio::spawn(async move {
            let mut connection = 0;
            while let Ok((stream, _)) = listener.accept().await {
                connection += 1;
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    continue;
                };
                let hello = json!({ "op": 10, "d": { "heartbeat_interval": 60 } });
                let _ = ws.send(Message::text(hello.to_string())).await;
                loop {
                    tokio::select! {
                        frame = ws.next() => {
                            let Some(Ok(Message::Text(text))) = frame else { break };
                            let value: Value = serde_json::from_str(text.as_str()).unwrap();
                            let op = value["op"].as_u64().unwrap();
                            seen.lock().unwrap().push((connection, value));
                            let reply = match op {
                                1 => Some(json!({ "op": 11 })),
                                2 => Some(json!({ "op": 0, "t": "READY", "s": 1, "d": {
                                    "session_id": format!("sess-{connection}"),
                                    "resume_gateway_url": own_url
                                }})),
                                6 => Some(json!({ "op": 0, "t": "RESUMED", "s": 2, "d": {} })),
                                _ => None,
                            };
                            if let Some(reply) = reply {
                                let _ = ws.send(Message::text(reply.to_string())).await;
                            }
                        }
                        command = inbox.recv() => match command {
                            Some(Cmd::Send(value)) => {
                                let _ = ws.send(Message::text(value.to_string())).await;
                            }
                            Some(Cmd::Close(code)) => {
                                let frame = code.map(|c| CloseFrame {
                                    code: CloseCode::from(c),
                                    reason: "".into(),
                                });
                                let _ = ws.send(Message::Close(frame)).await;
                                break;
                            }
                            None => return,
                        },
                    }
                }
            }
        });
        FakeGateway {
            url,
            frames,
            commands,
        }
    }

    fn frames_of(&self, op: u64) -> Vec<(usize, Value)> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, v)| v["op"] == op)
            .cloned()
            .collect()
    }

    fn send(&self, value: Value) {
        self.commands.send(Cmd::Send(value)).unwrap();
    }

    fn close(&self, code: Option<u16>) {
        self.commands.send(Cmd::Close(code)).unwrap();
    }
}

fn custom_id() -> String {
    format!("r:{}", Uuid::from_u128(BLOCKER).simple())
}

fn interaction(seq: u64, id: &str, user: &str, channel: &str, data: &str) -> Value {
    json!({
        "op": 0, "t": "INTERACTION_CREATE", "s": seq,
        "d": {
            "id": id, "token": format!("tok-{id}"), "type": 3, "channel_id": channel,
            "member": { "user": { "id": user } },
            "data": { "custom_id": data, "component_type": 2 },
            "message": { "content": "Blocked: tasks/s1 in tasks" }
        }
    })
}

fn notifier(server: &MockServer, allowed: Option<Vec<String>>) -> ChatNotifier {
    let clock: Arc<dyn Clock> = Arc::new(Fixed);
    ChatNotifier::new(
        ChatConfig {
            language: Lang::En,
            public_url: None,
            channels: vec![ChannelSpec {
                name: "chat".into(),
                kind: ChannelKind::Discord,
                chat: CHANNEL.into(),
                api_url: server.uri(),
                token: SecretText::new(TOKEN.into()),
                allowed_users: allowed,
            }],
            user_agent: "saka/test".into(),
            poll: PollTiming {
                long_poll_secs: 0,
                backoff_start: Duration::from_millis(10),
                backoff_max: Duration::from_millis(40),
            },
        },
        &clock,
    )
    .unwrap()
}

async fn mount_rest(server: &MockServer, gateway_url: &str) {
    Mock::given(method("GET"))
        .and(path("/gateway/bot"))
        .and(header("Authorization", format!("Bot {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "url": gateway_url })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(204))
        .mount(server)
        .await;
}

async fn callbacks(server: &MockServer) -> Vec<(String, Value)> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().contains("/interactions/"))
        .map(|r| {
            (
                r.url.path().to_string(),
                serde_json::from_slice(&r.body).unwrap(),
            )
        })
        .collect()
}

async fn until<F: std::future::Future<Output = bool>>(mut check: impl FnMut() -> F) {
    for _ in 0..300 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the condition never held");
}

struct Running {
    stop: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Running {
    async fn finish(self) {
        self.stop.send(true).unwrap();
        for task in self.tasks {
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .expect("the gateway task stops with the shutdown signal")
                .unwrap();
        }
    }
}

fn start(server: &MockServer, handler: &Arc<Recorder>) -> Running {
    let chat = notifier(server, Some(vec![OPERATOR.to_string()]));
    let (stop, rx) = watch::channel(false);
    let dynamic: Arc<dyn ChatActions> = handler.clone();
    let tasks = chat.spawn_listeners(&dynamic, &rx);
    assert_eq!(tasks.len(), 1);
    Running { stop, tasks }
}

async fn ready(gateway: &FakeGateway) {
    until(|| async { !gateway.frames_of(2).is_empty() }).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn the_gateway_identifies_heartbeats_and_runs_the_press_of_the_operator_once() {
    let gateway = FakeGateway::start().await;
    let server = MockServer::start().await;
    mount_rest(&server, &gateway.url).await;
    let handler = recorder(ChatOutcome::Done);
    let running = start(&server, &handler);
    ready(&gateway).await;

    let identify = &gateway.frames_of(2)[0].1;
    assert_eq!(identify["d"]["token"], TOKEN);
    assert_eq!(identify["d"]["intents"], 0);
    until(|| async { gateway.frames_of(1).len() >= 2 }).await;
    assert_eq!(gateway.frames_of(1).last().unwrap().1["d"], 1);

    gateway.send(interaction(5, "i-1", OPERATOR, CHANNEL, &custom_id()));
    gateway.send(interaction(6, "i-1", OPERATOR, CHANNEL, &custom_id()));
    until(|| async { !callbacks(&server).await.is_empty() }).await;
    tokio::time::sleep(Duration::from_millis(150)).await;

    let calls = handler.calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![ChatAction {
            platform: "discord",
            user_id: OPERATOR.into(),
            kind: ChatActionKind::Retry,
            blocker_id: BlockerId(Uuid::from_u128(BLOCKER)),
        }],
        "the repeated interaction id ran nothing"
    );
    let answers = callbacks(&server).await;
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].0, "/interactions/i-1/tok-i-1/callback");
    assert_eq!(answers[0].1["type"], 7);
    assert_eq!(
        answers[0].1["data"]["content"],
        "Blocked: tasks/s1 in tasks\n\nRetry started."
    );
    assert_eq!(answers[0].1["data"]["components"], json!([]));
    running.finish().await;
}

#[tokio::test]
async fn nobody_but_the_listed_users_in_the_listed_channel_can_act() {
    let gateway = FakeGateway::start().await;
    let server = MockServer::start().await;
    mount_rest(&server, &gateway.url).await;
    let handler = recorder(ChatOutcome::Done);
    let running = start(&server, &handler);
    ready(&gateway).await;

    gateway.send(interaction(2, "a", "999", CHANNEL, &custom_id()));
    gateway.send(interaction(3, "b", OPERATOR, "111", &custom_id()));
    gateway.send(interaction(4, "c", OPERATOR, CHANNEL, "r:nothex"));
    let mut slash = interaction(5, "d", OPERATOR, CHANNEL, &custom_id());
    slash["d"]["type"] = json!(2);
    gateway.send(slash);
    until(|| async { callbacks(&server).await.len() == 3 }).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let answers = callbacks(&server).await;
    assert_eq!(answers.len(), 3, "only message components are answered");
    for (_, body) in &answers {
        assert_eq!(body["type"], 4);
        assert_eq!(body["data"]["flags"], 64);
    }
    assert_eq!(answers[0].1["data"]["content"], "Not allowed.");
    assert_eq!(answers[1].1["data"]["content"], "Not allowed.");
    assert_eq!(answers[2].1["data"]["content"], "Invalid button.");
    assert!(handler.calls.lock().unwrap().is_empty());
    running.finish().await;
}

#[tokio::test]
async fn a_dropped_connection_resumes_the_session_and_an_invalid_one_identifies_again() {
    let gateway = FakeGateway::start().await;
    let server = MockServer::start().await;
    mount_rest(&server, &gateway.url).await;
    let handler = recorder(ChatOutcome::Done);
    let running = start(&server, &handler);
    ready(&gateway).await;

    gateway.send(json!({ "op": 0, "t": "TYPING_START", "s": 9, "d": {} }));
    tokio::time::sleep(Duration::from_millis(50)).await;
    gateway.close(None);
    until(|| async { !gateway.frames_of(6).is_empty() }).await;
    let resume = &gateway.frames_of(6)[0];
    assert_eq!(resume.0, 2);
    assert_eq!(resume.1["d"]["session_id"], "sess-1");
    assert_eq!(resume.1["d"]["seq"], 9);
    assert_eq!(resume.1["d"]["token"], TOKEN);

    tokio::time::sleep(Duration::from_millis(50)).await;
    gateway.send(json!({ "op": 9, "d": false }));
    until(|| async { gateway.frames_of(2).len() == 2 }).await;
    assert_eq!(gateway.frames_of(2)[1].0, 3);

    tokio::time::sleep(Duration::from_millis(50)).await;
    gateway.send(interaction(3, "after", OPERATOR, CHANNEL, &custom_id()));
    until(|| async { handler.calls.lock().unwrap().len() == 1 }).await;
    running.finish().await;
}

#[tokio::test]
async fn a_rejected_token_ends_the_gateway_task_by_itself() {
    let gateway = FakeGateway::start().await;
    let server = MockServer::start().await;
    mount_rest(&server, &gateway.url).await;
    let handler = recorder(ChatOutcome::Done);
    let Running { stop: _stop, tasks } = start(&server, &handler);
    until(|| async { !gateway.frames_of(2).is_empty() }).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    gateway.close(Some(4004));
    for task in tasks {
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("authentication failure is final")
            .unwrap();
    }
    assert_eq!(gateway.frames_of(2).len(), 1);

    let denied = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/gateway/bot"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&denied)
        .await;
    let Running { stop: _keep, tasks } = start(&denied, &handler);
    for task in tasks {
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("a refused token is final")
            .unwrap();
    }
}

#[tokio::test]
async fn a_message_gets_a_button_only_for_what_a_press_can_run() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/channels/{CHANNEL}/messages")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "1" })))
        .mount(&server)
        .await;
    let blocked = |action: Value| Notification {
        channel: "chat".into(),
        code: "hook.event".into(),
        params: json!({
            "event": "blocked",
            "project": { "name": "tasks" },
            "task": { "id": "0193f0c4-0000-7000-8000-000000000004", "key": "tasks/s1" },
            "blocker": { "id": Uuid::from_u128(BLOCKER).to_string(), "code": "ci_red", "action": action }
        }),
        dedupe_key: "run".into(),
    };
    let with_actions = notifier(&server, Some(vec![OPERATOR.to_string()]));
    with_actions.send(&blocked(json!("retry"))).await.unwrap();
    with_actions.send(&blocked(json!("cancel"))).await.unwrap();
    with_actions
        .send(&blocked(json!("edit_spec")))
        .await
        .unwrap();
    notifier(&server, None)
        .send(&blocked(json!("retry")))
        .await
        .unwrap();

    let sent: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    let button = |i: usize| sent[i].pointer("/components/0/components/0").cloned();
    assert_eq!(
        button(0),
        Some(json!({ "type": 2, "style": 1, "label": "Retry", "custom_id": custom_id() }))
    );
    assert_eq!(button(1).unwrap()["style"], 4);
    assert_eq!(button(1).unwrap()["label"], "Cancel task");
    assert_eq!(sent[0]["components"][0]["type"], 1);
    assert!(custom_id().len() <= 100);
    assert_eq!(button(2), None, "an action that needs input has no button");
    assert_eq!(button(3), None, "a channel without actions has no button");
}
