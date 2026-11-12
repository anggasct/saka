#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert by panicking"
)]

use std::sync::Arc;

use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_notify::{
    ChannelKind, ChannelSpec, ChatConfig, ChatNotifier, Lang, PollTiming, TEXT_MAX_CHARS, render,
};
use saka_ports::{Clock, Notification, Notifier};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "123456:SECRET-bot-token";
const NOW: i64 = 1_793_786_400_000;

struct Fixed;

impl Clock for Fixed {
    fn now(&self) -> Timestamp {
        Timestamp(NOW)
    }
}

fn hook_event(event: &str) -> Value {
    json!({
        "id": "0193f0c4-0000-7000-8000-000000000001",
        "hook": "tell-ops",
        "event": event,
        "project": { "id": "0193f0c4-0000-7000-8000-000000000002", "name": "tasks" },
        "repo": { "id": "0193f0c4-0000-7000-8000-000000000003", "name": "api", "full_name": "acme/api" },
        "task": { "id": "0193f0c4-0000-7000-8000-000000000004", "key": "tasks/s1" },
        "spec": { "id": "0193f0c4-0000-7000-8000-000000000005", "path": "specs/auth.md" },
        "change": { "number": 42, "url": "https://github.com/acme/api/pull/42" },
        "branch": "main",
        "sha": null,
        "blocker": if event == "blocked" {
            json!({ "id": "0193f0c4-0000-7000-8000-000000000006", "code": "ci_red", "action": "retry" })
        } else { Value::Null },
        "review": if event == "review_done" {
            json!({ "id": "0193f0c4-0000-7000-8000-000000000007", "outcome": "approved", "head_sha": "a1b2", "findings": 0, "summary": "Fine." })
        } else { Value::Null },
        "occurred_at": "2026-11-04T10:00:00.123Z"
    })
}

fn notification(channel: &str, code: &str, params: Value) -> Notification {
    Notification {
        channel: channel.into(),
        code: code.into(),
        params,
        dedupe_key: "0193f0c4-0000-7000-8000-000000000001".into(),
    }
}

fn notifier(kind: ChannelKind, server: &MockServer, public_url: Option<&str>) -> ChatNotifier {
    notifier_at(kind, &server.uri(), public_url)
}

fn notifier_at(kind: ChannelKind, api_url: &str, public_url: Option<&str>) -> ChatNotifier {
    let chat = match kind {
        ChannelKind::Telegram => "-100123",
        ChannelKind::Discord => "998877",
    };
    let clock: Arc<dyn Clock> = Arc::new(Fixed);
    ChatNotifier::new(
        ChatConfig {
            language: Lang::En,
            public_url: public_url.map(str::to_string),
            channels: vec![ChannelSpec {
                name: "ops".into(),
                kind,
                chat: chat.into(),
                api_url: api_url.into(),
                token: SecretText::new(TOKEN.into()),
                allowed_users: None,
            }],
            user_agent: "saka/test".into(),
            poll: PollTiming::default(),
        },
        &clock,
    )
    .unwrap()
}

#[test]
fn every_hook_event_renders_in_both_languages() {
    let link = Some("https://saka.example.com/");
    let cases = [
        (
            "blocked",
            "Blocked: tasks/s1 in tasks\nReason: ci_red\nhttps://saka.example.com/delivery/tasks/0193f0c4-0000-7000-8000-000000000004",
            "Terhenti: tasks/s1 di tasks\nPenyebab: ci_red\nhttps://saka.example.com/delivery/tasks/0193f0c4-0000-7000-8000-000000000004",
        ),
        (
            "merged",
            "Merged: tasks/s1 in tasks\nhttps://saka.example.com/delivery/tasks/0193f0c4-0000-7000-8000-000000000004",
            "Di-merge: tasks/s1 di tasks\nhttps://saka.example.com/delivery/tasks/0193f0c4-0000-7000-8000-000000000004",
        ),
        (
            "task_done",
            "Done: tasks/s1 in tasks\nhttps://saka.example.com/delivery/tasks/0193f0c4-0000-7000-8000-000000000004",
            "Selesai: tasks/s1 di tasks\nhttps://saka.example.com/delivery/tasks/0193f0c4-0000-7000-8000-000000000004",
        ),
        (
            "review_done",
            "Review done: tasks/s1 in tasks\nVerdict: approved\nhttps://saka.example.com/delivery/tasks/0193f0c4-0000-7000-8000-000000000004",
            "Review selesai: tasks/s1 di tasks\nPutusan: approved\nhttps://saka.example.com/delivery/tasks/0193f0c4-0000-7000-8000-000000000004",
        ),
        (
            "change_opened",
            "Pull request opened: tasks/s1 in tasks\nhttps://saka.example.com/delivery/tasks/0193f0c4-0000-7000-8000-000000000004",
            "Pull request dibuka: tasks/s1 di tasks\nhttps://saka.example.com/delivery/tasks/0193f0c4-0000-7000-8000-000000000004",
        ),
    ];
    for (event, en, id) in cases {
        let n = notification("ops", "hook.event", hook_event(event));
        assert_eq!(render(Lang::En, &n, link), en, "{event} en");
        assert_eq!(render(Lang::Id, &n, link), id, "{event} id");
    }
    let mut regression = hook_event("blocked");
    regression["blocker"]["code"] = json!("regression_not_reproduced");
    let n = notification("ops", "hook.event", regression);
    assert_eq!(
        render(Lang::En, &n, link),
        "Blocked: tasks/s1 in tasks\nReason: regression_not_reproduced\nThe agent did not prove its regression test fails without the fix. Retry to make it write the test first.\nhttps://saka.example.com/delivery/tasks/0193f0c4-0000-7000-8000-000000000004"
    );
    assert!(render(Lang::Id, &n, link).contains("Coba lagi agar ia menulis test lebih dulu."),);
    let mut spec = hook_event("spec_shipped");
    spec["task"] = Value::Null;
    let n = notification("ops", "hook.event", spec);
    assert_eq!(
        render(Lang::En, &n, link),
        "Spec shipped: specs/auth.md in tasks\nhttps://saka.example.com/delivery"
    );
}

#[test]
fn there_is_no_link_without_a_public_url_and_a_test_message_has_none() {
    let n = notification("ops", "hook.event", hook_event("merged"));
    assert_eq!(render(Lang::En, &n, None), "Merged: tasks/s1 in tasks");
    let test = notification("ops", "notify.test", json!({}));
    assert_eq!(
        render(Lang::En, &test, Some("https://x.test")),
        "Saka: test message"
    );
    assert_eq!(render(Lang::Id, &test, None), "Saka: pesan uji");
    let other = notification("ops", "something.new", json!({}));
    assert_eq!(render(Lang::En, &other, None), "Saka: something.new");
}

#[test]
fn outside_text_is_cleaned_and_bounded() {
    let mut params = hook_event("merged");
    params["task"]["key"] = json!(format!("a\nb\u{7}{}", "x".repeat(500)));
    params["project"]["name"] = json!("@everyone\r\nhi");
    let text = render(Lang::En, &notification("ops", "hook.event", params), None);
    assert_eq!(text.lines().count(), 1);
    assert!(text.chars().count() <= TEXT_MAX_CHARS);
    assert!(text.contains("ab"));
    assert!(text.ends_with("@everyonehi"));
    assert!(text.contains('…'));
}

#[tokio::test]
async fn telegram_gets_the_text_for_the_chat_without_markup() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/sendMessage")))
        .and(body_partial_json(json!({
            "chat_id": -100_123,
            "text": "Merged: tasks/s1 in tasks\nhttps://saka.example.com/delivery/tasks/0193f0c4-0000-7000-8000-000000000004",
            "link_preview_options": { "is_disabled": true }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "result": {} })))
        .expect(1)
        .mount(&server)
        .await;
    let n = notifier(
        ChannelKind::Telegram,
        &server,
        Some("https://saka.example.com"),
    );
    n.send(&notification("ops", "hook.event", hook_event("merged")))
        .await
        .unwrap();
    let body: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert!(body.get("parse_mode").is_none());
}

#[tokio::test]
async fn telegram_failures_are_classified_once_and_never_carry_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/sendMessage")))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "ok": false, "error_code": 429, "parameters": { "retry_after": 7 }
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string(format!("bad {TOKEN}")))
        .mount(&server)
        .await;
    let n = notifier(ChannelKind::Telegram, &server, None);
    let msg = notification("ops", "notify.test", json!({}));

    let limited = n.send(&msg).await.unwrap_err();
    assert!(limited.retryable);
    assert_eq!(limited.code, "notify_unavailable");
    assert_eq!(limited.retry_at, Some(Timestamp(NOW + 7_000)));

    let down = n.send(&msg).await.unwrap_err();
    assert!(down.retryable);
    assert_eq!(down.params, json!({ "status": 502 }));

    let rejected = n.send(&msg).await.unwrap_err();
    assert!(!rejected.retryable);
    assert_eq!(rejected.code, "notify_channel_rejected");
    assert_eq!(rejected.params, json!({ "status": 401 }));
    for e in [&limited, &down, &rejected] {
        assert!(!format!("{e:?} {}", e.to_json()).contains("SECRET"));
    }
}

#[tokio::test]
async fn an_unreachable_telegram_is_retryable_and_the_error_has_no_url() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let n = notifier_at(ChannelKind::Telegram, &url, None);
    let e = n
        .send(&notification("ops", "notify.test", json!({})))
        .await
        .unwrap_err();
    assert!(e.retryable);
    assert_eq!(e.code, "notify_unavailable");
    assert!(!format!("{e:?} {}", e.to_json()).contains("SECRET"));
}

#[tokio::test]
async fn discord_posts_with_the_bot_token_no_mentions_and_a_nonce_from_the_dedupe_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/channels/998877/messages"))
        .and(header("Authorization", format!("Bot {TOKEN}").as_str()))
        .and(body_partial_json(json!({
            "content": "Saka: test message",
            "allowed_mentions": { "parse": [] },
            "enforce_nonce": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "1" })))
        .expect(2)
        .mount(&server)
        .await;
    let discord = notifier(ChannelKind::Discord, &server, None);
    let msg = notification("ops", "notify.test", json!({}));
    discord.send(&msg).await.unwrap();
    discord.send(&msg).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let nonce = |i: usize| -> String {
        let body: Value = serde_json::from_slice(&requests[i].body).unwrap();
        body["nonce"].as_str().unwrap().to_string()
    };
    assert_eq!(nonce(0), nonce(1));
    assert!(nonce(0).len() <= 25);
}

#[tokio::test]
async fn discord_rate_limits_and_rejections_are_classified() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_json(json!({ "message": "slow", "retry_after": 1.5 })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let n = notifier(ChannelKind::Discord, &server, None);
    let msg = notification("ops", "notify.test", json!({}));
    let limited = n.send(&msg).await.unwrap_err();
    assert!(limited.retryable);
    assert_eq!(limited.retry_at, Some(Timestamp(NOW + 1_500)));
    let rejected = n.send(&msg).await.unwrap_err();
    assert!(!rejected.retryable);
    assert_eq!(rejected.code, "notify_channel_rejected");
    assert_eq!(rejected.params, json!({ "status": 403 }));
}

#[tokio::test]
async fn an_unknown_channel_is_unconfigured() {
    let server = MockServer::start().await;
    let n = notifier(ChannelKind::Telegram, &server, None);
    let e = n
        .send(&notification("elsewhere", "notify.test", json!({})))
        .await
        .unwrap_err();
    assert_eq!(e.code, "notify_channel_unconfigured");
    assert!(!e.retryable);
    assert_eq!(e.params, json!({ "channel": "elsewhere" }));
}

#[test]
fn an_opened_inbox_item_names_what_waits_and_links_to_it_only_with_a_public_url() {
    let mut params = hook_event("inbox_item_opened");
    params["inbox"] = json!({
        "id": "0193f0c4-0000-7000-8000-000000000009",
        "kind": "permission",
        "subject": "permission"
    });
    let n = notification("ops", "hook.event", params);
    assert_eq!(
        render(Lang::En, &n, Some("https://saka.example.com/")),
        "Needs you: tasks/s1 in tasks\nWaiting: permission / permission\nhttps://saka.example.com/inbox/0193f0c4-0000-7000-8000-000000000009"
    );
    assert_eq!(
        render(Lang::Id, &n, None),
        "Butuh kamu: tasks/s1 di tasks\nMenunggu: permission / permission"
    );
}
