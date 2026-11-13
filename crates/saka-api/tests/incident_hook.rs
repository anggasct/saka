#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use saka_api::{ApiConfig, AppState, hooks_router};
use saka_core::incident::{IncidentSignal, RawSource, Source};
use saka_core::vocab::{IncidentSeverity, SignalStatus};
use saka_testkit::{Sim, SimConfig, delivery};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Hook {
    sim: Sim,
    app: Router,
}

struct Reply {
    status: StatusCode,
    retry_after: Option<String>,
    body: Value,
}

fn source() -> Source {
    Source::validate(&RawSource {
        name: "prod-alerts".into(),
        project: "tasks".into(),
        kind: "generic".into(),
        secret_file: "/etc/saka/incident.secret".into(),
        ..RawSource::default()
    })
    .unwrap()
}

fn alert(fingerprint: &str, status: SignalStatus) -> IncidentSignal {
    IncidentSignal {
        fingerprint: fingerprint.into(),
        status,
        severity: IncidentSeverity::High,
        title: "Checkout down".into(),
        summary: String::new(),
        environment: None,
        url: None,
        started_at: None,
        labels: Default::default(),
    }
}

impl Hook {
    fn new() -> Hook {
        let sim = Sim::new(SimConfig {
            incident_sources: vec![source()],
            ..SimConfig::default()
        });
        let state = AppState::new(ApiConfig::new("0.1.0-test"));
        state.set_engine(sim.engine());
        Hook {
            app: hooks_router(state),
            sim,
        }
    }

    fn post(&self, source: &str, body: Vec<u8>) -> Reply {
        let req = Request::post(format!("/hooks/incident/{source}"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        self.sim.block_on(async {
            let response = self.app.clone().oneshot(req).await.unwrap();
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .map(|v| v.to_str().unwrap().to_string());
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            Reply {
                status,
                retry_after,
                body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            }
        })
    }

    fn rows(&self, table: &str) -> i64 {
        self.sim
            .engine()
            .store()
            .read(|c| {
                c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap()
    }
}

#[test]
fn a_valid_delivery_is_accepted_with_its_counts_and_a_repeat_has_no_effect() {
    let hook = Hook::new();
    hook.sim.signals.deliver(delivery(
        "d-1",
        vec![
            alert("a", SignalStatus::Firing),
            alert("b", SignalStatus::Firing),
            alert("never-seen", SignalStatus::Resolved),
        ],
    ));
    let first = hook.post("prod-alerts", b"{}".to_vec());
    assert_eq!(first.status, StatusCode::ACCEPTED);
    assert_eq!(
        first.body,
        json!({
            "delivery_id": "d-1", "outcome": "accepted", "signals": 3,
            "opened": 2, "updated": 0, "limited": 0, "truncated": 0
        })
    );
    assert_eq!(hook.rows("incident"), 2);
    assert_eq!(hook.rows("incident_delivery"), 1);

    hook.sim
        .signals
        .deliver(delivery("d-1", vec![alert("c", SignalStatus::Firing)]));
    let again = hook.post("prod-alerts", b"{}".to_vec());
    assert_eq!(again.status, StatusCode::ACCEPTED);
    assert_eq!(again.body["outcome"], "duplicate");
    assert_eq!(again.body["opened"], 0);
    assert_eq!(hook.rows("incident"), 2);
}

#[test]
fn a_delivery_the_decoder_refuses_writes_nothing_and_says_why() {
    let hook = Hook::new();
    hook.sim.signals.refuse("webhook_signature_invalid");
    let unauthorized = hook.post("prod-alerts", b"{}".to_vec());
    assert_eq!(unauthorized.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unauthorized.body["code"], "webhook_signature_invalid");

    let malformed = hook.post("prod-alerts", b"not json".to_vec());
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    assert_eq!(malformed.body["code"], "malformed_payload");
    assert_eq!(malformed.body["params"]["field"], "body");

    assert_eq!(hook.rows("incident"), 0);
    assert_eq!(hook.rows("incident_delivery"), 0);
}

#[test]
fn an_unknown_source_is_not_found_and_a_large_body_is_refused_before_it_is_decoded() {
    let hook = Hook::new();
    let unknown = hook.post("nowhere", b"{}".to_vec());
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.body["code"], "not_found");

    let big = hook.post("prod-alerts", vec![b'x'; 1024 * 1024 + 1]);
    assert_eq!(big.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(big.body["code"], "payload_too_large");
    assert!(hook.sim.signals.received().is_empty());
}

#[test]
fn a_source_past_its_rate_limit_gets_429_with_retry_after_until_the_minute_passes() {
    let hook = Hook::new();
    for _ in 0..120 {
        let reply = hook.post("prod-alerts", b"{}".to_vec());
        assert_ne!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    }
    let limited = hook.post("prod-alerts", b"{}".to_vec());
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.body["code"], "rate_limited");
    let wait: u64 = limited.retry_after.unwrap().parse().unwrap();
    assert!((1..=60).contains(&wait));
    assert_eq!(hook.sim.signals.received().len(), 120);

    hook.sim.clock.advance(61_000);
    hook.sim
        .signals
        .deliver(delivery("later", vec![alert("a", SignalStatus::Firing)]));
    assert_eq!(
        hook.post("prod-alerts", b"{}".to_vec()).status,
        StatusCode::ACCEPTED
    );
}

#[test]
fn the_public_listener_serves_nothing_else_of_the_control_api() {
    let hook = Hook::new();
    let req = Request::get("/api/v1/incidents/00000000-0000-0000-0000-000000000001")
        .body(Body::empty())
        .unwrap();
    let status = hook
        .sim
        .block_on(async { hook.app.clone().oneshot(req).await.unwrap().status() });
    assert_eq!(status, StatusCode::NOT_FOUND);
}
