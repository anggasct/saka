#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use saka_api::{ApiConfig, AppState, router};
use saka_core::hook::{self, RawHook};
use saka_testkit::{Sim, SimConfig, WebhookScript};
use serde_json::{Value, json};
use tower::ServiceExt;

const STEPS: usize = 8_000;

struct Api {
    sim: Sim,
    app: Router,
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Value,
}

impl Api {
    fn new() -> Api {
        let raw = |name: &str, on: &str| RawHook {
            name: name.into(),
            project: "tasks".into(),
            on: on.into(),
            action: "webhook".into(),
            url: Some("https://ci.example.com/saka?token=hunter2".into()),
            secret: Some("KEY_NAME".into()),
            ..RawHook::default()
        };
        let mut sim = Sim::new(SimConfig {
            hooks: hook::parse_all(&[raw("announce", "merged"), raw("done", "task_done")]).unwrap(),
            ..SimConfig::default()
        });
        sim.hooks.script_webhook(WebhookScript::Status(200));
        sim.approve();
        assert!(sim.run_to_shipped(STEPS));
        assert!(
            sim.run_until(STEPS, |s| s.events_of("delivery.hook_run.finished").len()
                == 4)
        );
        let state = AppState::new(ApiConfig::new("0.1.0-test"));
        state.set_engine(sim.engine());
        let app = router(state);
        Api { sim, app }
    }

    fn send(&self, req: Request<Body>) -> Reply {
        self.sim.block_on(async {
            let response = self.app.clone().oneshot(req).await.unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            Reply {
                status,
                headers,
                body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            }
        })
    }

    fn get(&self, path: &str) -> Reply {
        self.send(
            Request::get(path)
                .header("host", "127.0.0.1:7420")
                .body(Body::empty())
                .unwrap(),
        )
    }

    fn post(&self, path: &str, key: Option<&str>, body: &Value) -> Reply {
        let mut req = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("host", "127.0.0.1:7420")
            .header("content-type", "application/json");
        if let Some(k) = key {
            req = req.header("idempotency-key", k);
        }
        self.send(req.body(Body::from(body.to_string())).unwrap())
    }

    fn project(&self) -> String {
        self.sim.project_id.to_string()
    }
}

fn code(r: &Reply, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.body);
    assert_eq!(r.body["code"], code, "{}", r.body);
}

#[test]
fn the_effective_hooks_of_a_project_are_listed_without_secrets_or_url_queries() {
    let api = Api::new();
    let r = api.get(&format!("/api/v1/projects/{}/hooks", api.project()));
    assert_eq!(r.status, StatusCode::OK);
    let items = r.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["name"], "announce");
    assert_eq!(items[0]["on"], "merged");
    assert_eq!(items[0]["mode"], "after");
    assert_eq!(items[0]["action"], "webhook");
    assert_eq!(items[0]["timeout_secs"], 10);
    assert_eq!(items[0]["target"]["url"], "https://ci.example.com/saka");
    assert_eq!(items[0]["target"]["secret"], "KEY_NAME");
    assert!(!r.body.to_string().contains("hunter2"));

    code(
        &api.get("/api/v1/projects/0193f0c4-0000-7000-8000-000000000000/hooks"),
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[test]
fn runs_are_listed_newest_first_with_filters_and_a_cursor() {
    let api = Api::new();
    let base = format!("/api/v1/hook-runs?project_id={}", api.project());
    let all = api.get(&format!("{base}&limit=3"));
    assert_eq!(all.status, StatusCode::OK);
    let items = all.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    let times: Vec<&str> = items
        .iter()
        .map(|i| i["queued_at"].as_str().unwrap())
        .collect();
    assert!(times.windows(2).all(|w| w[0] >= w[1]), "{times:?}");
    let cursor = all.body["next_cursor"].as_str().unwrap();
    let rest = api.get(&format!("{base}&limit=3&cursor={cursor}"));
    assert_eq!(rest.body["items"].as_array().unwrap().len(), 1);
    assert!(rest.body["next_cursor"].is_null());

    let only = api.get(&format!("{base}&hook=done&state=failed"));
    let done = only.body["items"].as_array().unwrap();
    assert_eq!(done.len(), 2);
    assert!(
        done.iter()
            .all(|r| r["hook"] == "done" && r["error"]["code"] == "hook_secret_missing")
    );
    assert!(done[0]["task_key"].is_string());
    assert_eq!(done[0]["context"]["event"], "task_done");

    code(
        &api.get("/api/v1/hook-runs"),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    code(
        &api.get(&format!("{base}&state=zzz")),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    code(
        &api.get(&format!("{base}&cursor=%25%25")),
        StatusCode::UNPROCESSABLE_ENTITY,
        "cursor_invalid",
    );
    code(
        &api.get(&format!("{base}&limit=0")),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
}

#[test]
fn one_run_is_read_by_id() {
    let api = Api::new();
    let list = api.get(&format!(
        "/api/v1/hook-runs?project_id={}&limit=1",
        api.project()
    ));
    let id = list.body["items"][0]["id"].as_str().unwrap().to_string();
    let one = api.get(&format!("/api/v1/hook-runs/{id}"));
    assert_eq!(one.status, StatusCode::OK);
    assert_eq!(one.body["id"], id);
    assert_eq!(one.body["state"], "failed");
    assert!(one.body["result"]["output_tail"].is_null());
    code(
        &api.get("/api/v1/hook-runs/0193f0c4-0000-7000-8000-000000000000"),
        StatusCode::NOT_FOUND,
        "not_found",
    );
    code(
        &api.get("/api/v1/hook-runs/nope"),
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[test]
fn a_rerun_is_accepted_once_per_key_and_refused_while_pending() {
    let api = Api::new();
    let list = api.get(&format!(
        "/api/v1/hook-runs?project_id={}&limit=1",
        api.project()
    ));
    let id = list.body["items"][0]["id"].as_str().unwrap().to_string();
    let path = format!("/api/v1/hook-runs/{id}/actions/rerun");

    code(
        &api.post(&path, None, &json!({ "reason": "again" })),
        StatusCode::BAD_REQUEST,
        "idempotency_key_required",
    );
    code(
        &api.post(&path, Some("k0"), &json!({})),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    code(
        &api.post(&path, Some("k0b"), &json!({ "reason": "  " })),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );

    let first = api.post(&path, Some("k1"), &json!({ "reason": "secret is set" }));
    assert_eq!(first.status, StatusCode::ACCEPTED, "{}", first.body);
    assert_eq!(first.body["state"], "pending");
    assert_eq!(first.body["rerun_of"], id);
    let replay = api.post(&path, Some("k1"), &json!({ "reason": "secret is set" }));
    assert_eq!(replay.status, StatusCode::ACCEPTED);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.body["id"], first.body["id"]);
    code(
        &api.post(&path, Some("k1"), &json!({ "reason": "different" })),
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_reused",
    );

    let new_id = first.body["id"].as_str().unwrap();
    code(
        &api.post(
            &format!("/api/v1/hook-runs/{new_id}/actions/rerun"),
            Some("k2"),
            &json!({ "reason": "too early" }),
        ),
        StatusCode::CONFLICT,
        "hook_run_pending",
    );
    code(
        &api.post(
            "/api/v1/hook-runs/0193f0c4-0000-7000-8000-000000000000/actions/rerun",
            Some("k3"),
            &json!({ "reason": "x" }),
        ),
        StatusCode::NOT_FOUND,
        "not_found",
    );
}
