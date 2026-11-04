#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use saka_api::{ApiConfig, AppState, router};
use saka_core::diagnostics::AgentLogin;
use saka_core::vocab::Tool;
use saka_engine::DiagnosticLimits;
use saka_testkit::diagnostics::HostPort;
use saka_testkit::{Sim, SimConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Reply {
    status: StatusCode,
    cache_control: Option<String>,
    text: String,
    body: Value,
}

fn api(sim: &Sim) -> axum::Router {
    saka_testkit::logs::check_callsites_per_thread();
    let state = AppState::new(ApiConfig::new("0.1.0-test"));
    state.set_engine(sim.engine());
    router(state)
}

fn get(sim: &Sim, app: &axum::Router, path: &str) -> Reply {
    sim.block_on(async {
        let req = Request::get(path)
            .header("host", "127.0.0.1:7420")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        let status = response.status();
        let cache_control = response
            .headers()
            .get("cache-control")
            .map(|v| v.to_str().unwrap().to_string());
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Reply {
            status,
            cache_control,
            body: serde_json::from_str(&text).unwrap_or(Value::Null),
            text,
        }
    })
}

fn ids(body: &Value) -> Vec<(String, String)> {
    body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap().to_string(),
                c["subject"]["label"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn check<'a>(body: &'a Value, id: &str) -> &'a Value {
    body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no {id} in {body}"))
}

fn rows(sim: &Sim) -> (i64, i64, i64) {
    sim.engine()
        .store()
        .read(|c| {
            let n = |t: &str| {
                c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
                    .unwrap()
            };
            Ok((n("event"), n("job"), n("action_request")))
        })
        .unwrap()
}

#[test]
fn the_report_holds_every_instance_and_agent_check_in_catalog_order_and_changes_nothing() {
    let sim = Sim::new(SimConfig::default());
    let app = api(&sim);
    let before = rows(&sim);
    let r = get(&sim, &app, "/api/v1/diagnostics");
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.cache_control.as_deref(), Some("no-store"));
    let expected: Vec<(String, String)> = [
        ("server.ready", "instance"),
        ("db.migrations", "instance"),
        ("runtime.supervisor", "instance"),
        ("agent.binary", "claude:default"),
        ("agent.login", "claude:default"),
        ("agent.quota", "claude:default"),
        ("toolchain.cache", "instance"),
        ("toolchain.sources", "instance"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(ids(&r.body), expected);
    assert_eq!(r.body["mode"], "online");
    assert_eq!(r.body["server_version"], "0.1.0-test");
    let login = check(&r.body, "agent.login");
    assert_eq!(
        login["subject"],
        json!({ "kind": "agent_account", "id": "claude:default", "label": "claude:default" })
    );
    assert_eq!(login["status"], "pass");
    assert_eq!(
        login["params"],
        json!({ "account": "claude:default", "auth_method": "claude.ai" })
    );
    assert_eq!(login["fix"], Value::Null);
    assert_eq!(
        check(&r.body, "db.migrations")["code"],
        "migrations_current"
    );
    assert_eq!(check(&r.body, "agent.quota")["code"], "quota_unknown");
    let ready = check(&r.body, "server.ready");
    assert_eq!(
        ready["code"], "server_not_ready",
        "the sim runs no worker loop"
    );
    assert_eq!(ready["params"]["failing"], json!(["worker", "reconciler"]));
    assert_eq!(
        r.body["summary"],
        json!({ "pass": 6, "warn": 0, "fail": 1, "skip": 1 })
    );
    assert_eq!(
        rows(&sim),
        before,
        "a diagnosis writes no event, job, or action"
    );
}

#[test]
fn the_check_filter_and_the_project_are_validated() {
    let sim = Sim::new(SimConfig::default());
    let app = api(&sim);
    let only = get(&sim, &app, "/api/v1/diagnostics?check=agent.login");
    assert_eq!(only.status, StatusCode::OK);
    assert_eq!(
        ids(&only.body),
        [("agent.login".to_string(), "claude:default".to_string())]
    );
    let two = get(
        &sim,
        &app,
        "/api/v1/diagnostics?check=toolchain.cache&check=agent.login",
    );
    assert_eq!(ids(&two.body).len(), 2);
    for (query, field, code) in [
        ("check=nope", "check", "unknown"),
        ("check=cli.config", "check", "unknown"),
        ("project_id=not-a-uuid", "project_id", "invalid"),
        (
            "project_id=01900000-0000-7000-8000-000000000000",
            "project_id",
            "unknown",
        ),
    ] {
        let r = get(&sim, &app, &format!("/api/v1/diagnostics?{query}"));
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{query}: {}",
            r.text
        );
        assert_eq!(r.body["code"], "validation_failed");
        assert_eq!(
            r.body["params"]["errors"],
            json!([{ "field": field, "code": code }]),
            "{query}"
        );
    }
    let project = sim.project_id.0;
    let scoped = get(
        &sim,
        &app,
        &format!("/api/v1/diagnostics?project_id={project}"),
    );
    assert_eq!(scoped.status, StatusCode::OK, "{}", scoped.text);
}

fn limited() -> SimConfig {
    SimConfig {
        diagnostics: DiagnosticLimits {
            concurrency: 8,
            check: Duration::from_millis(100),
            report: Duration::from_millis(300),
        },
        ..SimConfig::default()
    }
}

#[test]
fn a_hung_port_times_out_its_check_and_never_holds_the_report() {
    let sim = Sim::new(limited());
    let app = api(&sim);
    sim.world.host().script(|s| {
        s.hang.insert(HostPort::AgentLogin);
    });
    let started = Instant::now();
    let r = get(&sim, &app, "/api/v1/diagnostics");
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );
    let login = check(&r.body, "agent.login");
    assert_eq!(login["status"], "fail");
    assert_eq!(login["code"], "check_timed_out");
    assert_eq!(login["params"], json!({ "timeout_ms": 100 }));
    assert_eq!(check(&r.body, "agent.binary")["status"], "pass");
}

#[test]
fn checks_still_queued_at_the_report_limit_time_out_too() {
    let mut cfg = limited();
    cfg.diagnostics.concurrency = 1;
    let sim = Sim::new(cfg);
    let app = api(&sim);
    sim.world.host().script(|s| {
        s.hang.extend([
            HostPort::Supervisor,
            HostPort::AgentCli,
            HostPort::AgentLogin,
            HostPort::ToolchainCache,
            HostPort::Releases,
        ]);
    });
    let started = Instant::now();
    let r = get(&sim, &app, "/api/v1/diagnostics");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
    let timed_out: Vec<&Value> = r.body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["code"] == "check_timed_out")
        .collect();
    assert_eq!(timed_out.len(), 5, "{}", r.body);
    assert!(
        timed_out.iter().any(|c| c["params"]["timeout_ms"] == 300),
        "a check that starts after the report limit names it: {}",
        r.body
    );
}

#[test]
fn findings_from_the_host_reach_the_report_with_their_fix() {
    let sim = Sim::new(SimConfig::default());
    let app = api(&sim);
    sim.world.host().script(|s| {
        s.login = AgentLogin::NotLoggedIn;
        s.failing_sources.insert(Tool::Node);
        s.cache.writable = false;
    });
    let r = get(&sim, &app, "/api/v1/diagnostics");
    let login = check(&r.body, "agent.login");
    assert_eq!(login["code"], "agent_not_logged_in");
    assert_eq!(
        login["fix"],
        json!({ "code": "run_agent_login", "params": { "command": "saka agent login" } })
    );
    let sources: Vec<&Value> = r.body["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["id"] == "toolchain.sources")
        .collect();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["status"], "warn");
    assert_eq!(
        sources[0]["params"],
        json!({ "tool": "node", "url": "https://node.mirror.test/", "status": 500 })
    );
    assert_eq!(
        check(&r.body, "toolchain.cache")["code"],
        "toolchain_cache_unwritable"
    );
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
fn a_report_is_counted_in_metrics_and_logged() {
    let sim = Sim::new(SimConfig::default());
    let app = api(&sim);
    let logs = LogBuffer::default();
    let out = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || out.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        get(&sim, &app, "/api/v1/diagnostics");
    });
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("diagnostics.ran"), "{text}");
    for field in [
        "\"pass\":6",
        "\"warn\":0",
        "\"fail\":1",
        "\"skip\":1",
        "\"duration_ms\":",
    ] {
        assert!(text.contains(field), "{field} in {text}");
    }
    assert!(text.contains("diagnostics.check_failed"), "{text}");
    assert!(text.contains("\"code\":\"server_not_ready\""), "{text}");
    let metrics = get(&sim, &app, "/metrics").text;
    for line in [
        "saka_diagnostic_checks_total{id=\"agent.login\",status=\"pass\"} 1",
        "saka_diagnostic_checks_total{id=\"server.ready\",status=\"fail\"} 1",
        "saka_diagnostic_checks_total{id=\"agent.quota\",status=\"skip\"} 1",
    ] {
        assert!(metrics.contains(line), "{line}\n{metrics}");
    }
}
