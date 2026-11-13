#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

mod support;

use saka_core::time::Timestamp;
use saka_intake::SentryDecoder;
use saka_ports::{BODY_DEDUP_WINDOW_SECS, SignalDecoder, SignalSeverity, SignalStatus};
use support::{SECRET, fixture, headers, hmac_hex, request};

fn signed(resource: &str, body: &[u8], request_id: Option<&str>) -> Vec<(String, String)> {
    let mut pairs = vec![
        ("Sentry-Hook-Resource", resource.to_string()),
        ("Sentry-Hook-Signature", hmac_hex(SECRET, &[body])),
    ];
    if let Some(id) = request_id {
        pairs.push(("Request-ID", id.to_string()));
    }
    let borrowed: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    headers(&borrowed)
}

#[test]
fn a_created_issue_fires_with_the_issue_id_as_fingerprint() {
    let body = fixture("sentry_issue_created.json");
    let given = signed("issue", &body, Some("req-1"));
    let delivery = SentryDecoder::new()
        .decode(&request(&given, &body))
        .unwrap();

    assert!(!delivery.ignored);
    assert_eq!(delivery.delivery_key, "req-1");
    assert_eq!(delivery.dedup_window_secs, None);
    let signal = &delivery.signals[0];
    assert_eq!(signal.fingerprint, "4510233781");
    assert_eq!(signal.status, SignalStatus::Firing);
    assert_eq!(signal.severity, SignalSeverity::High);
    assert!(signal.title.starts_with("TypeError"));
    assert_eq!(signal.summary, "app/checkout/summary in renderTotals");
    assert_eq!(
        signal.url.as_deref(),
        Some("https://sentry.example.com/organizations/acme/issues/4510233781/")
    );
    assert_eq!(signal.started_at, Some(Timestamp(1_794_563_892_000)));
    assert_eq!(signal.environment, None);
    assert_eq!(
        signal.labels.get("project").map(String::as_str),
        Some("checkout")
    );
}

#[test]
fn resolved_and_unresolved_issues_map_to_their_status() {
    for (name, expected) in [
        ("sentry_issue_resolved.json", SignalStatus::Resolved),
        ("sentry_issue_unresolved.json", SignalStatus::Firing),
    ] {
        let body = fixture(name);
        let given = signed("issue", &body, Some("req-2"));
        let delivery = SentryDecoder::new()
            .decode(&request(&given, &body))
            .unwrap();
        assert_eq!(delivery.signals.len(), 1, "{name}");
        assert_eq!(delivery.signals[0].status, expected, "{name}");
        assert_eq!(delivery.signals[0].fingerprint, "4510233781");
    }
}

#[test]
fn other_issue_actions_are_ignored() {
    let body = fixture("sentry_issue_assigned.json");
    let given = signed("issue", &body, Some("req-3"));
    let delivery = SentryDecoder::new()
        .decode(&request(&given, &body))
        .unwrap();
    assert!(delivery.ignored);
    assert!(delivery.signals.is_empty());
}

#[test]
fn other_resources_are_ignored_once_authenticated() {
    let body = fixture("sentry_issue_created.json");
    let given = signed("comment", &body, Some("req-4"));
    let delivery = SentryDecoder::new()
        .decode(&request(&given, &body))
        .unwrap();
    assert!(delivery.ignored);
}

#[test]
fn an_event_alert_fires_with_its_environment_on_the_same_fingerprint() {
    let body = fixture("sentry_event_alert.json");
    let given = signed("event_alert", &body, Some("req-5"));
    let delivery = SentryDecoder::new()
        .decode(&request(&given, &body))
        .unwrap();
    let signal = &delivery.signals[0];
    assert_eq!(signal.fingerprint, "4510233781");
    assert_eq!(signal.status, SignalStatus::Firing);
    assert_eq!(signal.severity, SignalSeverity::Critical);
    assert_eq!(signal.environment.as_deref(), Some("production"));
    assert_eq!(
        signal.labels.get("rule").map(String::as_str),
        Some("Checkout errors in production")
    );
    assert!(
        signal
            .url
            .as_deref()
            .is_some_and(|url| url.contains("/events/"))
    );
}

#[test]
fn a_missing_request_id_falls_back_to_the_body_digest() {
    let body = fixture("sentry_issue_created.json");
    let given = signed("issue", &body, None);
    let delivery = SentryDecoder::new()
        .decode(&request(&given, &body))
        .unwrap();
    assert_eq!(delivery.delivery_key.len(), 64);
    assert_eq!(delivery.dedup_window_secs, Some(BODY_DEDUP_WINDOW_SECS));
}

#[test]
fn a_bad_or_missing_signature_is_refused() {
    let body = fixture("sentry_issue_created.json");
    let other = fixture("sentry_issue_resolved.json");
    let wrong_body = signed("issue", &other, Some("r"));
    let wrong_secret = headers(&[
        ("Sentry-Hook-Resource", "issue"),
        (
            "Sentry-Hook-Signature",
            &hmac_hex(b"another-secret", &[&body]),
        ),
    ]);
    let not_hex = headers(&[
        ("Sentry-Hook-Resource", "issue"),
        ("Sentry-Hook-Signature", "zz"),
    ]);
    let missing = headers(&[("Sentry-Hook-Resource", "issue")]);
    for given in [wrong_body, wrong_secret, not_hex, missing] {
        let error = SentryDecoder::new()
            .decode(&request(&given, &body))
            .unwrap_err();
        assert_eq!(error.code, "webhook_signature_invalid");
    }
}

#[test]
fn a_signed_body_of_the_wrong_shape_is_malformed() {
    for (resource, body) in [
        ("issue", br#"{"action":"created","data":{}}"#.as_slice()),
        ("issue", br#"not json"#.as_slice()),
        (
            "event_alert",
            br#"{"data":{"event":{"title":"x"}}}"#.as_slice(),
        ),
    ] {
        let given = signed(resource, body, Some("r"));
        let error = SentryDecoder::new()
            .decode(&request(&given, body))
            .unwrap_err();
        assert_eq!(error.code, "malformed_payload");
    }
}
