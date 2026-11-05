#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use saka_core::blocker::primary_action;
use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_core::vocab::{BlockerAction, Stage, StagePhase, WorkItemState};
use saka_notify::{ChannelKind, ChannelSpec, ChatConfig, ChatNotifier, Lang, PollTiming};
use saka_ports::{ChatActions, Clock, Notification, Notifier};
use saka_testkit::{CiScript, CommandScript, Sim, SimConfig};
use serde_json::{Value, json};
use tokio::sync::watch;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "123456:SECRET-bot-token";
const CHAT: i64 = -1001;
const OPERATOR: i64 = 4242;
const STEPS: usize = 8_000;

struct Fixed;

impl Clock for Fixed {
    fn now(&self) -> Timestamp {
        Timestamp(0)
    }
}

fn api(method_name: &str) -> String {
    format!("/bot{TOKEN}/{method_name}")
}

fn blocked_sim() -> Sim {
    let mut cfg = SimConfig {
        ci: CiScript::Pass,
        ..SimConfig::default()
    };
    cfg.operator.merge = false;
    cfg.operator.retry = vec![];
    let mut sim = Sim::new(cfg);
    for _ in 0..9 {
        sim.world
            .script_command("cd cli && go test ./...", CommandScript::Crash);
    }
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Blocked));
    sim
}

fn wait(what: &str, mut check: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

async fn calls_to(server: &MockServer, method_name: &str) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == api(method_name))
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

fn press(update_id: i64, user: i64, data: &str) -> Value {
    json!({
        "update_id": update_id,
        "callback_query": {
            "id": format!("cb-{update_id}"),
            "from": { "id": user },
            "message": { "message_id": 9, "chat": { "id": CHAT }, "text": "Blocked" },
            "data": data
        }
    })
}

#[test]
fn a_button_in_a_telegram_message_retries_the_blocked_task_once_and_only_for_the_operator() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = rt.block_on(MockServer::start());
    let mut sim = blocked_sim();
    let task = sim.task(1);
    let blocker = task.open_blocker().cloned().unwrap();
    let action = primary_action(&task).unwrap();
    assert_eq!(action.kind, BlockerAction::Retry);

    let clock: Arc<dyn Clock> = Arc::new(Fixed);
    let chat = ChatNotifier::new(
        ChatConfig {
            language: Lang::En,
            public_url: Some("https://saka.example.com".into()),
            channels: vec![ChannelSpec {
                name: "ops".into(),
                kind: ChannelKind::Telegram,
                chat: CHAT.to_string(),
                api_url: server.uri(),
                token: SecretText::new(TOKEN.into()),
                allowed_users: Some(vec![OPERATOR.to_string()]),
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
    .unwrap();
    rt.block_on(async {
        Mock::given(method("POST"))
            .and(path(api("sendMessage")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;
        chat.send(&Notification {
            channel: "ops".into(),
            code: "hook.event".into(),
            params: json!({
                "event": "blocked",
                "project": { "name": "tasks" },
                "task": { "id": task.item.id.0.to_string(), "key": task.item.task_key },
                "blocker": {
                    "id": blocker.id.0.to_string(),
                    "code": blocker.code.as_str(),
                    "action": action.kind.as_str()
                }
            }),
            dedupe_key: "run-1".into(),
        })
        .await
        .unwrap();
    });
    let sent = rt.block_on(calls_to(&server, "sendMessage"));
    let data = sent[0]["reply_markup"]["inline_keyboard"][0][0]["callback_data"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(sent[0]["text"].as_str().unwrap().ends_with(&format!(
        "https://saka.example.com/delivery/tasks/{}",
        task.item.id.0
    )));

    rt.block_on(async {
        for name in ["deleteWebhook", "answerCallbackQuery", "editMessageText"] {
            Mock::given(method("POST"))
                .and(path(api(name)))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
                .mount(&server)
                .await;
        }
        let updates = vec![
            press(1, 999, &data),
            press(2, OPERATOR, &data),
            press(3, OPERATOR, &data),
        ];
        Mock::given(method("POST"))
            .and(path(api("getUpdates")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "result": updates })),
            )
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(api("getUpdates")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "ok": true, "result": [] }))
                    .set_delay(Duration::from_millis(30)),
            )
            .with_priority(2)
            .mount(&server)
            .await;
    });
    let (stop, stopped) = watch::channel(false);
    let handler: Arc<dyn ChatActions> = sim.engine();
    let _guard = rt.enter();
    let pollers = chat.spawn_listeners(&handler, &stopped);
    assert_eq!(pollers.len(), 1);

    wait("the three presses to be answered", || {
        rt.block_on(calls_to(&server, "answerCallbackQuery")).len() == 3
    });
    let texts: Vec<String> = rt
        .block_on(calls_to(&server, "answerCallbackQuery"))
        .iter()
        .map(|a| a["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        texts,
        ["Not allowed.", "Retry started.", "No longer pending."]
    );

    let resolved = sim.events_of("delivery.blocker.resolved");
    assert_eq!(resolved.len(), 1, "the retry ran once");
    assert_eq!(resolved[0].actor, "operator:telegram:4242");
    assert_eq!(resolved[0].payload["resolution"], "retried");
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    assert_eq!(sim.task(1).retries[&Stage::Merge].manual, 1);

    stop.send(true).unwrap();
    for poller in pollers {
        rt.block_on(poller).unwrap();
    }
}

fn fake_gateway(rt: &tokio::runtime::Runtime, interaction: Value) -> String {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let listener = rt
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    rt.spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let hello = json!({ "op": 10, "d": { "heartbeat_interval": 60_000 } });
        ws.send(Message::text(hello.to_string())).await.unwrap();
        while let Some(Ok(Message::Text(text))) = ws.next().await {
            let frame: Value = serde_json::from_str(text.as_str()).unwrap();
            if frame["op"] == 2 {
                let ready = json!({ "op": 0, "t": "READY", "s": 1, "d": { "session_id": "s", "resume_gateway_url": "ws://unused" } });
                ws.send(Message::text(ready.to_string())).await.unwrap();
                ws.send(Message::text(interaction.to_string())).await.unwrap();
            }
        }
    });
    url
}

#[test]
fn a_button_in_a_discord_message_retries_the_blocked_task_for_the_listed_user() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = rt.block_on(MockServer::start());
    let mut sim = blocked_sim();
    let blocker = sim.task(1).open_blocker().cloned().unwrap();
    let press = |kind: &str| {
        json!({
            "op": 0, "t": "INTERACTION_CREATE", "s": 2,
            "d": {
                "id": format!("i-{kind}"), "token": "t", "type": 3, "channel_id": "998877",
                "member": { "user": { "id": "555" } },
                "data": { "custom_id": format!("{kind}:{}", blocker.id.0.simple()) },
                "message": { "content": "Blocked" }
            }
        })
    };
    let url = fake_gateway(&rt, press("r"));
    rt.block_on(async {
        Mock::given(method("GET"))
            .and(path("/gateway/bot"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "url": url })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/interactions/i-r/t/callback"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
    });
    let clock: Arc<dyn Clock> = Arc::new(Fixed);
    let chat = ChatNotifier::new(
        ChatConfig {
            language: Lang::En,
            public_url: None,
            channels: vec![ChannelSpec {
                name: "chat".into(),
                kind: ChannelKind::Discord,
                chat: "998877".into(),
                api_url: server.uri(),
                token: SecretText::new(TOKEN.into()),
                allowed_users: Some(vec!["555".into()]),
            }],
            user_agent: "saka/test".into(),
            poll: PollTiming::default(),
        },
        &clock,
    )
    .unwrap();
    let (stop, stopped) = watch::channel(false);
    let handler: Arc<dyn ChatActions> = sim.engine();
    let _guard = rt.enter();
    let listeners = chat.spawn_listeners(&handler, &stopped);

    wait("the press to be answered", || {
        rt.block_on(server.received_requests())
            .unwrap()
            .iter()
            .any(|r| r.url.path() == "/interactions/i-r/t/callback")
    });
    let answered = rt.block_on(server.received_requests()).unwrap();
    let callback = answered
        .iter()
        .find(|r| r.url.path() == "/interactions/i-r/t/callback")
        .unwrap();
    let body: Value = serde_json::from_slice(&callback.body).unwrap();
    assert_eq!(body["data"]["content"], "Blocked\n\nRetry started.");
    let resolved = sim.events_of("delivery.blocker.resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].actor, "operator:discord:555");
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));

    stop.send(true).unwrap();
    for listener in listeners {
        rt.block_on(listener).unwrap();
    }
}
