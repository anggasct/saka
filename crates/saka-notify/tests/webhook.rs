#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests assert by panicking"
)]

use saka_core::secret::SecretText;
use saka_notify::{HttpWebhooks, UnconfiguredNotifier, signature};
use saka_ports::{Notification, Notifier, WebhookPost, Webhooks};
use serde_json::json;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "s3cr3t-signing-key";

fn post(url: String) -> WebhookPost {
    WebhookPost {
        url,
        event: "merged".into(),
        delivery_id: "run-1".into(),
        body: br#"{"id":"run-1"}"#.to_vec(),
        secret: SecretText::new(SECRET.into()),
        timeout_ms: 2_000,
        timestamp_secs: 1_793_786_400,
    }
}

#[test]
fn the_signature_covers_the_timestamp_and_the_body() {
    let sig = signature(SECRET, 1_793_786_400, br#"{"id":"run-1"}"#).unwrap();
    assert!(sig.starts_with("sha256="));
    assert_eq!(sig.len(), "sha256=".len() + 64);
    assert_ne!(
        sig,
        signature(SECRET, 1_793_786_401, br#"{"id":"run-1"}"#).unwrap()
    );
    assert_ne!(
        sig,
        signature(SECRET, 1_793_786_400, br#"{"id":"run-2"}"#).unwrap()
    );
    assert_ne!(
        sig,
        signature("other-key", 1_793_786_400, br#"{"id":"run-1"}"#).unwrap()
    );
}

#[test]
fn the_signature_matches_a_fixed_vector() {
    assert_eq!(
        signature("key", 1, b"body").unwrap(),
        "sha256=91b5374b153842ad05b2c4eab9349b8321b14703165bd3fb8b034dfb8be98ae5"
    );
}

#[tokio::test]
async fn a_delivery_carries_the_signed_headers_and_the_body() {
    let server = MockServer::start().await;
    let expected = signature(SECRET, 1_793_786_400, br#"{"id":"run-1"}"#).unwrap();
    Mock::given(method("POST"))
        .and(path("/saka"))
        .and(header("X-Saka-Event", "merged"))
        .and(header("X-Saka-Delivery", "run-1"))
        .and(header("X-Saka-Timestamp", "1793786400"))
        .and(header("X-Saka-Signature-256", expected.as_str()))
        .and(header("Content-Type", "application/json"))
        .and(header_exists("User-Agent"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let hooks = HttpWebhooks::new("saka/test").unwrap();
    let receipt = hooks
        .post(&post(format!("{}/saka", server.uri())))
        .await
        .unwrap();
    assert_eq!(receipt.status, 204);
    let received = server.received_requests().await.unwrap();
    assert_eq!(received[0].body, br#"{"id":"run-1"}"#);
    assert!(!String::from_utf8_lossy(&received[0].body).contains(SECRET));
}

#[tokio::test]
async fn statuses_are_classified_as_retryable_or_permanent() {
    let server = MockServer::start().await;
    for (status, retryable) in [
        (429, true),
        (500, true),
        (503, true),
        (400, false),
        (404, false),
        (410, false),
    ] {
        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        let hooks = HttpWebhooks::new("saka/test").unwrap();
        let err = hooks.post(&post(server.uri())).await.unwrap_err();
        assert_eq!(err.retryable, retryable, "{status}");
        assert_eq!(
            err.code,
            if retryable {
                "hook_webhook_unavailable"
            } else {
                "hook_webhook_rejected"
            }
        );
        assert_eq!(err.params["status"], status);
    }
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/a"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/b"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/b"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let err = HttpWebhooks::new("saka/test")
        .unwrap()
        .post(&post(format!("{}/a", server.uri())))
        .await
        .unwrap_err();
    assert_eq!(err.code, "hook_webhook_rejected");
    assert_eq!(err.params["status"], 302);
}

#[tokio::test]
async fn a_slow_receiver_times_out_as_a_retryable_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_millis(600)))
        .mount(&server)
        .await;
    let mut request = post(server.uri());
    request.timeout_ms = 100;
    let err = HttpWebhooks::new("saka/test")
        .unwrap()
        .post(&request)
        .await
        .unwrap_err();
    assert!(err.retryable);
    assert_eq!(err.params["reason"], "timeout");
}

#[tokio::test]
async fn an_unreachable_receiver_is_retryable() {
    let mut request = post("http://127.0.0.1:1/".into());
    request.timeout_ms = 500;
    let err = HttpWebhooks::new("saka/test")
        .unwrap()
        .post(&request)
        .await
        .unwrap_err();
    assert!(err.retryable);
}

#[tokio::test]
async fn the_default_notifier_refuses_every_channel_visibly() {
    let err = UnconfiguredNotifier
        .send(&Notification {
            channel: "ops".into(),
            code: "hook.event".into(),
            params: json!({}),
            dedupe_key: "k".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, "notify_channel_unconfigured");
    assert_eq!(err.params["channel"], "ops");
    assert!(!err.retryable);
}
