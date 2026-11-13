#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

mod support;

use saka_core::time::Timestamp;
use saka_intake::{AlertmanagerDecoder, AlertmanagerOptions};
use saka_ports::{
    BODY_DEDUP_WINDOW_SECS, IncidentSeverity, MAX_SIGNALS_PER_DELIVERY, SignalDecoder, SignalStatus,
};
use serde_json::json;
use support::{SECRET, fixture, headers, request};

fn bearer() -> Vec<(String, String)> {
    headers(&[(
        "Authorization",
        &format!("Bearer {}", String::from_utf8_lossy(SECRET)),
    )])
}

fn decoder() -> AlertmanagerDecoder {
    AlertmanagerDecoder::new(AlertmanagerOptions {
        environment_label: "env".into(),
        severity_label: "severity".into(),
    })
}

#[test]
fn a_firing_group_becomes_one_signal_per_alert() {
    let body = fixture("alertmanager_firing.json");
    let delivery = decoder().decode(&request(&bearer(), &body)).unwrap();

    assert!(!delivery.ignored);
    assert_eq!(delivery.truncated, 0);
    assert_eq!(delivery.dedup_window_secs, Some(BODY_DEDUP_WINDOW_SECS));
    assert_eq!(delivery.signals.len(), 2);

    let first = &delivery.signals[0];
    assert_eq!(first.fingerprint, "3f2c9a1b7d5e4c60");
    assert_eq!(first.status, SignalStatus::Firing);
    assert_eq!(first.severity, IncidentSeverity::Critical);
    assert_eq!(first.title, "Checkout error rate above 5%");
    assert!(first.summary.starts_with("5xx responses on checkout"));
    assert!(first.summary.contains('\n'));
    assert_eq!(first.environment.as_deref(), Some("prod"));
    assert_eq!(
        first.url.as_deref(),
        Some("https://prometheus.example.com/graph?g0.expr=rate")
    );
    assert_eq!(first.started_at, Some(Timestamp(1_794_563_700_000)));
    assert_eq!(
        first.labels.get("service").map(String::as_str),
        Some("checkout")
    );

    let second = &delivery.signals[1];
    assert_eq!(second.title, "DiskAlmostFull");
    assert_eq!(second.severity, IncidentSeverity::Medium);
    assert_eq!(second.environment.as_deref(), Some("staging"));
    assert_eq!(second.url, None);
}

#[test]
fn a_resolved_group_yields_resolved_signals() {
    let body = fixture("alertmanager_resolved.json");
    let delivery = decoder().decode(&request(&bearer(), &body)).unwrap();
    assert_eq!(delivery.signals.len(), 1);
    assert_eq!(delivery.signals[0].status, SignalStatus::Resolved);
    assert_eq!(delivery.signals[0].fingerprint, "3f2c9a1b7d5e4c60");
}

#[test]
fn the_same_body_has_the_same_delivery_key() {
    let body = fixture("alertmanager_firing.json");
    let other = fixture("alertmanager_resolved.json");
    let first = decoder().decode(&request(&bearer(), &body)).unwrap();
    let again = decoder().decode(&request(&bearer(), &body)).unwrap();
    let different = decoder().decode(&request(&bearer(), &other)).unwrap();
    assert_eq!(first.delivery_key, again.delivery_key);
    assert_ne!(first.delivery_key, different.delivery_key);
}

#[test]
fn custom_label_names_and_default_names_are_honoured() {
    let body = json!({
        "version": "4",
        "alerts": [{
            "status": "firing",
            "fingerprint": "abc",
            "labels": { "alertname": "X", "environment": "prod", "severity": "page" }
        }]
    })
    .to_string();
    let delivery = AlertmanagerDecoder::default()
        .decode(&request(&bearer(), body.as_bytes()))
        .unwrap();
    assert_eq!(delivery.signals[0].environment.as_deref(), Some("prod"));
    assert_eq!(delivery.signals[0].severity, IncidentSeverity::Critical);

    let by_other_label = decoder()
        .decode(&request(&bearer(), body.as_bytes()))
        .unwrap();
    assert_eq!(by_other_label.signals[0].environment, None);
}

#[test]
fn severity_labels_map_to_the_scale() {
    let cases = [
        ("critical", IncidentSeverity::Critical),
        ("page", IncidentSeverity::Critical),
        ("high", IncidentSeverity::High),
        ("error", IncidentSeverity::High),
        ("warning", IncidentSeverity::Medium),
        ("low", IncidentSeverity::Low),
        ("info", IncidentSeverity::Info),
        ("whatever", IncidentSeverity::Info),
    ];
    for (label, expected) in cases {
        let body = json!({
            "version": "4",
            "alerts": [{ "status": "firing", "fingerprint": "f", "labels": { "severity": label } }]
        })
        .to_string();
        let delivery = decoder()
            .decode(&request(&bearer(), body.as_bytes()))
            .unwrap();
        assert_eq!(delivery.signals[0].severity, expected, "{label}");
    }
}

#[test]
fn more_than_the_limit_of_alerts_are_truncated_and_counted() {
    let alerts: Vec<_> = (0..MAX_SIGNALS_PER_DELIVERY + 7)
        .map(|n| json!({ "status": "firing", "fingerprint": format!("fp-{n}"), "labels": {} }))
        .collect();
    let body = json!({ "version": "4", "alerts": alerts }).to_string();
    let delivery = decoder()
        .decode(&request(&bearer(), body.as_bytes()))
        .unwrap();
    assert_eq!(delivery.signals.len(), MAX_SIGNALS_PER_DELIVERY);
    assert_eq!(delivery.truncated, 7);
}

#[test]
fn a_wrong_missing_or_malformed_token_is_refused() {
    let body = fixture("alertmanager_firing.json");
    let wrong = headers(&[("Authorization", "Bearer nope")]);
    let basic = headers(&[("Authorization", "Basic czNjcjN0LXNpZ25hbC10b2tlbg==")]);
    let bare = headers(&[("Authorization", "s3cr3t-signal-token")]);
    for given in [wrong, basic, bare, Vec::new()] {
        let error = decoder().decode(&request(&given, &body)).unwrap_err();
        assert_eq!(error.code, "webhook_signature_invalid");
    }
}

#[test]
fn an_empty_secret_never_authenticates() {
    let body = fixture("alertmanager_firing.json");
    let given = headers(&[("Authorization", "Bearer ")]);
    let mut asked = request(&given, &body);
    asked.secret = b"";
    let error = decoder().decode(&asked).unwrap_err();
    assert_eq!(error.code, "webhook_signature_invalid");
}

#[test]
fn bad_bodies_are_malformed_only_after_authentication() {
    let given = bearer();
    let unauthenticated = headers(&[]);
    let broken = [
        b"not json".as_slice(),
        br#"{"version":"3","alerts":[]}"#,
        br#"{"version":"4"}"#,
        br#"{"version":"4","alerts":[{"status":"firing","labels":{}}]}"#,
        br#"{"version":"4","alerts":[{"status":"odd","fingerprint":"f"}]}"#,
        br#"{"version":"4","alerts":[{"status":"firing","fingerprint":""}]}"#,
    ];
    for body in broken {
        let error = decoder().decode(&request(&given, body)).unwrap_err();
        assert_eq!(
            error.code,
            "malformed_payload",
            "{}",
            String::from_utf8_lossy(body)
        );
        let error = decoder()
            .decode(&request(&unauthenticated, body))
            .unwrap_err();
        assert_eq!(error.code, "webhook_signature_invalid");
    }
}

#[test]
fn text_is_cleaned_of_control_characters() {
    let body = json!({
        "version": "4",
        "alerts": [{
            "status": "firing",
            "fingerprint": "f",
            "labels": { "alertname": "x" },
            "annotations": { "summary": "line one\nline two\u{202e}", "description": "a\u{0}b" }
        }]
    })
    .to_string();
    let delivery = decoder()
        .decode(&request(&bearer(), body.as_bytes()))
        .unwrap();
    assert_eq!(delivery.signals[0].title, "line one line two");
    assert_eq!(delivery.signals[0].summary, "ab");
}
