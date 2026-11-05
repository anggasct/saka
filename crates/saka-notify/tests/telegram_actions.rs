#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert by panicking"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use saka_core::ids::BlockerId;
use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_notify::{ChannelKind, ChannelSpec, ChatConfig, ChatNotifier, Lang, PollTiming};
use saka_ports::{
    ChatAction, ChatActionKind, ChatActions, ChatOutcome, Clock, Notification, Notifier,
};
use serde_json::{Value, json};
use tokio::sync::watch;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const TOKEN: &str = "123456:SECRET-bot-token";
const CHAT: i64 = -1001;
const OPERATOR: i64 = 4242;
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

fn notifier(server: &MockServer, allowed: Option<Vec<String>>) -> ChatNotifier {
    let clock: Arc<dyn Clock> = Arc::new(Fixed);
    ChatNotifier::new(
        ChatConfig {
            language: Lang::En,
            public_url: None,
            channels: vec![ChannelSpec {
                name: "ops".into(),
                kind: ChannelKind::Telegram,
                chat: CHAT.to_string(),
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

fn press(update_id: i64, user: i64, chat: i64, data: &str) -> Value {
    json!({
        "update_id": update_id,
        "callback_query": {
            "id": format!("cb-{update_id}"),
            "from": { "id": user, "is_bot": false },
            "message": {
                "message_id": 77,
                "chat": { "id": chat },
                "text": "Blocked: tasks/s1 in tasks"
            },
            "data": data
        }
    })
}

fn retry_data() -> String {
    format!("r:{}", Uuid::from_u128(BLOCKER).simple())
}

fn api(method_name: &str) -> String {
    format!("/bot{TOKEN}/{method_name}")
}

async fn mount_idle(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path(api("deleteWebhook")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "result": true })),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(api("answerCallbackQuery")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(api("editMessageText")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(server)
        .await;
}

async fn mount_updates(server: &MockServer, first: Vec<Value>) {
    Mock::given(method("POST"))
        .and(path(api("getUpdates")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "result": first })),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(api("getUpdates")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "ok": true, "result": [] }))
                .set_delay(Duration::from_millis(30)),
        )
        .with_priority(2)
        .mount(server)
        .await;
}

async fn calls_to(server: &MockServer, method_name: &str) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r: &&Request| r.url.path() == api(method_name))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

async fn until<F: std::future::Future<Output = bool>>(mut check: impl FnMut() -> F) {
    for _ in 0..200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
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
                .expect("the poller stops with the shutdown signal")
                .unwrap();
        }
    }
}

fn start(server: &MockServer, handler: &Arc<Recorder>) -> Running {
    let chat = notifier(server, Some(vec![OPERATOR.to_string()]));
    let (stop, rx) = watch::channel(false);
    let dynamic: Arc<dyn ChatActions> = handler.clone();
    let tasks = chat.spawn_pollers(&dynamic, &rx);
    assert_eq!(tasks.len(), 1);
    Running { stop, tasks }
}

#[tokio::test]
async fn a_press_by_the_operator_runs_the_action_once_and_settles_the_message() {
    let server = MockServer::start().await;
    mount_idle(&server).await;
    mount_updates(&server, vec![press(900, OPERATOR, CHAT, &retry_data())]).await;
    let handler = recorder(ChatOutcome::Done);
    let running = start(&server, &handler);

    until(|| async { !calls_to(&server, "editMessageText").await.is_empty() }).await;
    let calls = handler.calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![ChatAction {
            platform: "telegram",
            user_id: OPERATOR.to_string(),
            kind: ChatActionKind::Retry,
            blocker_id: BlockerId(Uuid::from_u128(BLOCKER)),
        }]
    );
    let answers = calls_to(&server, "answerCallbackQuery").await;
    assert_eq!(answers[0]["callback_query_id"], "cb-900");
    assert_eq!(answers[0]["text"], "Retry started.");
    assert_eq!(answers[0]["show_alert"], false);
    let edits = calls_to(&server, "editMessageText").await;
    assert_eq!(
        edits[0]["text"],
        "Blocked: tasks/s1 in tasks\n\nRetry started."
    );
    assert_eq!(edits[0]["message_id"], 77);

    until(|| async { calls_to(&server, "getUpdates").await.len() >= 2 }).await;
    let polls = calls_to(&server, "getUpdates").await;
    assert_eq!(polls[0]["allowed_updates"], json!(["callback_query"]));
    assert!(polls[0].get("offset").is_none());
    assert_eq!(polls[1]["offset"], 901);
    let order: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().rsplit('/').next().unwrap().to_string())
        .collect();
    assert_eq!(order[0], "deleteWebhook");
    assert_eq!(handler.calls.lock().unwrap().len(), 1);
    running.finish().await;
}

#[tokio::test]
async fn nobody_but_the_listed_users_in_the_listed_chat_can_act() {
    let server = MockServer::start().await;
    mount_idle(&server).await;
    mount_updates(
        &server,
        vec![
            press(1, 999, CHAT, &retry_data()),
            press(2, OPERATOR, -555, &retry_data()),
            press(3, OPERATOR, CHAT, "r:nothex"),
            press(
                4,
                OPERATOR,
                CHAT,
                &format!("x:{}", Uuid::from_u128(BLOCKER).simple()),
            ),
        ],
    )
    .await;
    let handler = recorder(ChatOutcome::Done);
    let running = start(&server, &handler);

    until(|| async { calls_to(&server, "answerCallbackQuery").await.len() == 4 }).await;
    let answers = calls_to(&server, "answerCallbackQuery").await;
    assert_eq!(answers[0]["text"], "Not allowed.");
    assert_eq!(answers[0]["show_alert"], true);
    assert_eq!(answers[1]["text"], "Not allowed.");
    assert_eq!(answers[2]["text"], "Invalid button.");
    assert_eq!(answers[3]["text"], "Invalid button.");
    assert!(handler.calls.lock().unwrap().is_empty());
    assert!(calls_to(&server, "editMessageText").await.is_empty());
    running.finish().await;
}

#[tokio::test]
async fn a_stale_press_is_explained_and_a_failed_one_keeps_its_button() {
    for (outcome, text, edited) in [
        (ChatOutcome::Gone, "No longer pending.", true),
        (ChatOutcome::Changed, "The action changed. Open Saka.", true),
        (ChatOutcome::Failed, "Could not do that. Try again.", false),
    ] {
        let server = MockServer::start().await;
        mount_idle(&server).await;
        mount_updates(&server, vec![press(5, OPERATOR, CHAT, &retry_data())]).await;
        let handler = recorder(outcome);
        let running = start(&server, &handler);
        until(|| async { !calls_to(&server, "answerCallbackQuery").await.is_empty() }).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            calls_to(&server, "answerCallbackQuery").await[0]["text"],
            text
        );
        assert_eq!(
            !calls_to(&server, "editMessageText").await.is_empty(),
            edited
        );
        running.finish().await;
    }
}

#[tokio::test]
async fn a_second_poller_for_the_same_bot_backs_off_and_then_carries_on() {
    let server = MockServer::start().await;
    mount_idle(&server).await;
    Mock::given(method("POST"))
        .and(path(api("getUpdates")))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({
            "ok": false, "error_code": 409, "description": "Conflict"
        })))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    mount_updates(&server, vec![press(8, OPERATOR, CHAT, &retry_data())]).await;
    let handler = recorder(ChatOutcome::Done);
    let running = start(&server, &handler);

    until(|| async { handler.calls.lock().unwrap().len() == 1 }).await;
    assert!(calls_to(&server, "getUpdates").await.len() >= 3);
    running.finish().await;
}

#[tokio::test]
async fn a_rejected_token_stops_the_poller_without_hammering_the_platform() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(api("deleteWebhook")))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "ok": false })))
        .mount(&server)
        .await;
    let handler = recorder(ChatOutcome::Done);
    let chat = notifier(&server, Some(vec![OPERATOR.to_string()]));
    let (_stop, rx) = watch::channel(false);
    let dynamic: Arc<dyn ChatActions> = handler.clone();
    let tasks = chat.spawn_pollers(&dynamic, &rx);
    for task in tasks {
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("a poller with a rejected token ends by itself")
            .unwrap();
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn only_channels_with_actions_get_a_poller_and_a_button() {
    let server = MockServer::start().await;
    let handler = recorder(ChatOutcome::Done);
    let plain = notifier(&server, None);
    let (_stop, rx) = watch::channel(false);
    let dynamic: Arc<dyn ChatActions> = handler.clone();
    assert!(plain.spawn_pollers(&dynamic, &rx).is_empty());

    Mock::given(method("POST"))
        .and(path(api("sendMessage")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .mount(&server)
        .await;
    let blocked = |action: Value| Notification {
        channel: "ops".into(),
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
        .send(&blocked(json!("set_secret")))
        .await
        .unwrap();
    with_actions.send(&blocked(Value::Null)).await.unwrap();
    plain.send(&blocked(json!("retry"))).await.unwrap();

    let sent = calls_to(&server, "sendMessage").await;
    let key = |i: usize| {
        sent[i]
            .pointer("/reply_markup/inline_keyboard/0/0")
            .cloned()
    };
    assert_eq!(
        key(0),
        Some(json!({ "text": "Retry", "callback_data": retry_data() }))
    );
    assert_eq!(
        key(1),
        Some(json!({
            "text": "Cancel task",
            "callback_data": format!("c:{}", Uuid::from_u128(BLOCKER).simple())
        }))
    );
    assert!(retry_data().len() <= 64);
    assert_eq!(key(2), None, "an action that needs input has no button");
    assert_eq!(key(3), None);
    assert_eq!(key(4), None, "a channel without actions has no button");
}
