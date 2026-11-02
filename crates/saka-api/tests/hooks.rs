#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use saka_api::{ApiConfig, AppState, MAX_WEBHOOK_BYTES, hooks_router, router};
use saka_core::vocab::{Stage, StagePhase};
use saka_testkit::{CiScript, FAKE_SIGNATURE_HEADER, Sim, SimConfig, fake_signature};
use serde_json::{Value, json};
use tower::ServiceExt;

const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

struct Hooks {
    sim: Sim,
    hooks: Router,
    api: Router,
}

impl Hooks {
    fn new(ci: CiScript) -> Hooks {
        saka_testkit::logs::check_callsites_per_thread();
        let sim = Sim::new(SimConfig {
            ci,
            ..SimConfig::default()
        });
        let state = AppState::new(ApiConfig::new("0.1.0-test"));
        state.set_engine(sim.engine());
        Hooks {
            hooks: hooks_router(state.clone()),
            api: router(state),
            sim,
        }
    }

    fn send(&self, app: &Router, req: Request<Body>) -> (StatusCode, Value) {
        self.sim.block_on(async {
            let response = app.clone().oneshot(req).await.unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        })
    }

    fn deliver(
        &self,
        integration: &str,
        body: &[u8],
        signature: Option<String>,
    ) -> (StatusCode, Value) {
        let mut req = Request::post(format!("/hooks/{integration}"))
            .header("content-type", "application/json");
        if let Some(sig) = signature {
            req = req.header(FAKE_SIGNATURE_HEADER, sig);
        }
        self.send(&self.hooks, req.body(Body::from(body.to_vec())).unwrap())
    }

    fn signed(&self, payload: &Value) -> (StatusCode, Value) {
        let body = serde_json::to_vec(payload).unwrap();
        self.deliver("github", &body, Some(fake_signature(SECRET, &body)))
    }

    fn metrics(&self) -> String {
        let req = Request::get("/metrics")
            .header("host", "127.0.0.1:7420")
            .body(Body::empty())
            .unwrap();
        self.sim.block_on(async {
            let response = self.api.clone().oneshot(req).await.unwrap();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            String::from_utf8(bytes.to_vec()).unwrap()
        })
    }

    fn count(&self, sql: &str) -> i64 {
        self.sim
            .engine()
            .store()
            .read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }
}

fn code(reply: &(StatusCode, Value)) -> (StatusCode, &str) {
    (reply.0, reply.1["code"].as_str().unwrap_or_default())
}

#[test]
fn only_signed_deliveries_reach_the_engine() {
    let h = Hooks::new(CiScript::Pass);
    let other = Request::get("/api/v1/meta").body(Body::empty()).unwrap();
    assert_eq!(
        code(&h.send(&h.hooks, other)),
        (StatusCode::NOT_FOUND, "not_found")
    );
    let get = Request::builder()
        .method(Method::GET)
        .uri("/hooks/github")
        .body(Body::empty())
        .unwrap();
    assert_eq!(h.send(&h.hooks, get).0, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        code(&h.deliver("nope", b"{}", None)),
        (StatusCode::NOT_FOUND, "not_found")
    );
    assert_eq!(
        code(&h.deliver("github", b"{}", None)),
        (StatusCode::NOT_FOUND, "webhook_not_configured")
    );

    h.sim.world.set_webhook_secret(SECRET);
    let big = vec![b' '; MAX_WEBHOOK_BYTES + 1];
    assert_eq!(
        code(&h.deliver("github", &big, None)),
        (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
    );
    let body = br#"{"delivery_id":"d-1","event":"ping"}"#;
    for signature in [
        None,
        Some("00".to_string()),
        Some(fake_signature(b"another secret 0123456789abcdef", body)),
    ] {
        assert_eq!(
            code(&h.deliver("github", body, signature)),
            (StatusCode::UNAUTHORIZED, "webhook_signature_invalid")
        );
    }
    let not_json = b"{not json";
    assert_eq!(
        code(&h.deliver("github", not_json, Some(fake_signature(SECRET, not_json)))),
        (StatusCode::BAD_REQUEST, "malformed_json")
    );
    assert_eq!(h.count("SELECT COUNT(*) FROM webhook_delivery"), 0);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM job WHERE idempotency_key LIKE '%:hook:%'"),
        0
    );

    let ping = h.signed(&json!({ "delivery_id": "d-ping", "event": "ping", "repo": "acme/tasks" }));
    assert_eq!(ping.0, StatusCode::ACCEPTED);
    assert_eq!(
        ping.1,
        json!({ "delivery_id": "d-ping", "outcome": "ignored", "change_requests": 0 })
    );
}

#[test]
fn a_ci_event_refreshes_its_change_once_and_is_counted() {
    let mut h = Hooks::new(CiScript::Silent);
    h.sim.world.set_webhook_secret(SECRET);
    h.sim
        .approve_spec(h.sim.spec_id, h.sim.revision_id)
        .unwrap();
    assert!(h.sim.run_until(5_000, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingCi)
    }));
    let cr = h.sim.task(1).change_request.unwrap();

    let payload = json!({
        "delivery_id": "d-ci", "event": "check_run", "action": "completed",
        "repo": "acme/tasks", "targets": [{ "head": cr.head_sha }],
    });
    let first = h.signed(&payload);
    assert_eq!(first.0, StatusCode::ACCEPTED);
    assert_eq!(
        first.1,
        json!({ "delivery_id": "d-ci", "outcome": "refreshed", "change_requests": 1 })
    );
    let again = h.signed(&payload);
    assert_eq!(
        again.1,
        json!({ "delivery_id": "d-ci", "outcome": "duplicate", "change_requests": 1 })
    );
    assert_eq!(h.count("SELECT COUNT(*) FROM webhook_delivery"), 1);
    assert_eq!(
        h.count("SELECT COUNT(*) FROM job WHERE idempotency_key LIKE 'scm.poll:%:hook:%'"),
        1
    );
    let bad = h.deliver("github", b"{}", Some("00".into()));
    assert_eq!(bad.0, StatusCode::UNAUTHORIZED);

    let raw = h.metrics();
    for outcome in ["refreshed", "duplicate"] {
        let line = format!(
            "saka_webhook_deliveries_total{{integration=\"github\",event=\"check_run\",outcome=\"{outcome}\"}} 1"
        );
        assert!(raw.contains(&line), "{line}\n{raw}");
    }
    assert!(raw.contains(
        "saka_webhook_deliveries_total{integration=\"github\",event=\"unknown\",outcome=\"rejected\"} 1"
    ));
}

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn webhook_logs_name_the_delivery_but_never_the_body_or_secret() {
    let h = Hooks::new(CiScript::Pass);
    h.sim.world.set_webhook_secret(SECRET);
    let logs = LogBuffer::default();
    let out = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || out.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        h.signed(&json!({ "delivery_id": "d-log", "event": "ping", "repo": "acme/tasks", "note": "body-marker" }));
        h.deliver(
            "github",
            br#"{"note":"rejected-marker"}"#,
            Some("00".into()),
        );
    });
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("scm.webhook.received"), "{text}");
    assert!(text.contains("\"delivery_id\":\"d-log\""), "{text}");
    assert!(text.contains("scm.webhook.rejected"), "{text}");
    for secret in [
        "body-marker",
        "rejected-marker",
        std::str::from_utf8(SECRET).unwrap(),
    ] {
        assert!(!text.contains(secret), "{secret} reached the logs");
    }
}
