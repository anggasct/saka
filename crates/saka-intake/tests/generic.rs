#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

mod support;

use saka_core::time::Timestamp;
use saka_intake::GenericDecoder;
use saka_ports::{IncidentSeverity, SignalDecoder, SignalStatus, TIMESTAMP_WINDOW_SECS};
use support::{NOW, SECRET, fixture, headers, hmac_hex, request};

fn signed_at(timestamp_secs: i64, delivery: &str, body: &[u8]) -> Vec<(String, String)> {
    let timestamp = timestamp_secs.to_string();
    let signature = hmac_hex(SECRET, &[timestamp.as_bytes(), b".", body]);
    headers(&[
        ("X-Saka-Timestamp", &timestamp),
        ("X-Saka-Delivery", delivery),
        ("X-Saka-Signature-256", &format!("sha256={signature}")),
    ])
}

fn now_secs() -> i64 {
    NOW.0 / 1000
}

#[test]
fn a_signed_body_decodes_to_one_signal() {
    let body = fixture("generic_firing.json");
    let given = signed_at(now_secs(), "delivery-1", &body);
    let delivery = GenericDecoder::new()
        .decode(&request(&given, &body))
        .unwrap();

    assert_eq!(delivery.delivery_key, "delivery-1");
    assert_eq!(delivery.dedup_window_secs, None);
    assert!(!delivery.ignored);
    let signal = &delivery.signals[0];
    assert_eq!(signal.fingerprint, "queue-depth-orders");
    assert_eq!(signal.status, SignalStatus::Firing);
    assert_eq!(signal.severity, IncidentSeverity::High);
    assert_eq!(signal.title, "Orders queue depth above 10k");
    assert_eq!(signal.environment.as_deref(), Some("prod"));
    assert_eq!(signal.started_at, Some(Timestamp(1_794_563_100_000)));
    assert_eq!(signal.labels.len(), 2);
}

#[test]
fn optional_fields_may_be_left_out() {
    let body = br#"{"fingerprint":"f","status":"resolved","severity":"info","title":"t"}"#;
    let given = signed_at(now_secs(), "d", body);
    let delivery = GenericDecoder::new()
        .decode(&request(&given, body))
        .unwrap();
    let signal = &delivery.signals[0];
    assert_eq!(signal.status, SignalStatus::Resolved);
    assert_eq!(signal.summary, "");
    assert_eq!(signal.environment, None);
    assert_eq!(signal.url, None);
    assert_eq!(signal.started_at, None);
    assert!(signal.labels.is_empty());
}

#[test]
fn the_timestamp_must_be_within_the_window() {
    let body = fixture("generic_firing.json");
    for offset in [TIMESTAMP_WINDOW_SECS, -TIMESTAMP_WINDOW_SECS] {
        let given = signed_at(now_secs() + offset, "d", &body);
        assert!(
            GenericDecoder::new()
                .decode(&request(&given, &body))
                .is_ok()
        );
    }
    for offset in [TIMESTAMP_WINDOW_SECS + 1, -TIMESTAMP_WINDOW_SECS - 1] {
        let given = signed_at(now_secs() + offset, "d", &body);
        let error = GenericDecoder::new()
            .decode(&request(&given, &body))
            .unwrap_err();
        assert_eq!(error.code, "webhook_signature_invalid");
    }
}

#[test]
fn a_signature_over_another_timestamp_body_or_secret_is_refused() {
    let body = fixture("generic_firing.json");
    let mut tampered_timestamp = signed_at(now_secs(), "d", &body);
    tampered_timestamp[0].1 = (now_secs() + 1).to_string();
    let other_body = signed_at(now_secs(), "d", b"{}");
    let timestamp = now_secs().to_string();
    let wrong_secret = headers(&[
        ("X-Saka-Timestamp", &timestamp),
        ("X-Saka-Delivery", "d"),
        (
            "X-Saka-Signature-256",
            &format!(
                "sha256={}",
                hmac_hex(b"x", &[timestamp.as_bytes(), b".", &body])
            ),
        ),
    ]);
    let no_prefix = headers(&[
        ("X-Saka-Timestamp", &timestamp),
        ("X-Saka-Delivery", "d"),
        (
            "X-Saka-Signature-256",
            &hmac_hex(SECRET, &[timestamp.as_bytes(), b".", &body]),
        ),
    ]);
    for given in [
        tampered_timestamp,
        other_body,
        wrong_secret,
        no_prefix,
        Vec::new(),
    ] {
        let error = GenericDecoder::new()
            .decode(&request(&given, &body))
            .unwrap_err();
        assert_eq!(error.code, "webhook_signature_invalid");
    }
}

#[test]
fn the_delivery_id_must_be_present_and_well_formed() {
    let body = fixture("generic_firing.json");
    for bad in ["", "has space", "slash/id", &"x".repeat(129)] {
        let given = signed_at(now_secs(), bad, &body);
        let error = GenericDecoder::new()
            .decode(&request(&given, &body))
            .unwrap_err();
        assert_eq!(error.code, "webhook_signature_invalid", "{bad:?}");
    }
    let given = signed_at(now_secs(), &"x".repeat(128), &body);
    assert!(
        GenericDecoder::new()
            .decode(&request(&given, &body))
            .is_ok()
    );
    let given = signed_at(now_secs(), "a.b_c-1", &body);
    assert!(
        GenericDecoder::new()
            .decode(&request(&given, &body))
            .is_ok()
    );
}

#[test]
fn foreign_fields_and_bad_values_are_malformed() {
    let bodies: [&[u8]; 8] = [
        br#"{"fingerprint":"f","status":"firing","severity":"high","title":"t","extra":1}"#,
        br#"{"fingerprint":"f","status":"firing","severity":"high"}"#,
        br#"{"fingerprint":"f","status":"open","severity":"high","title":"t"}"#,
        br#"{"fingerprint":"f","status":"firing","severity":"urgent","title":"t"}"#,
        br#"{"fingerprint":"","status":"firing","severity":"high","title":"t"}"#,
        br#"{"fingerprint":"f","status":"firing","severity":"high","title":"  "}"#,
        br#"{"fingerprint":"f","status":"firing","severity":"high","title":"t","started_at":"yesterday"}"#,
        b"not json",
    ];
    for body in bodies {
        let given = signed_at(now_secs(), "d", body);
        let error = GenericDecoder::new()
            .decode(&request(&given, body))
            .unwrap_err();
        assert_eq!(
            error.code,
            "malformed_payload",
            "{}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn urls_other_than_web_urls_are_dropped() {
    let body = br#"{"fingerprint":"f","status":"firing","severity":"low","title":"t","url":"file:///etc/passwd"}"#;
    let given = signed_at(now_secs(), "d", body);
    let delivery = GenericDecoder::new()
        .decode(&request(&given, body))
        .unwrap();
    assert_eq!(delivery.signals[0].url, None);
}
