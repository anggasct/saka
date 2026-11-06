#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use saka_api::{ApiConfig, AppState, router};
use saka_core::decision::{DecisionOption, Question};
use saka_core::vocab::DecisionState;
use saka_testkit::{AgentScript, Sim, SimConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

const STEPS: usize = 8_000;

struct Api {
    sim: Sim,
    app: Router,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Api {
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
        if let Some(key) = key {
            req = req.header("idempotency-key", key);
        }
        self.send(req.body(Body::from(body.to_string())).unwrap())
    }
}

fn problem(r: &Reply, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.body);
    assert_eq!(r.body["code"], code, "{}", r.body);
}

fn question(text: &str) -> Question {
    let option = |label: &str, detail: Option<&str>| DecisionOption {
        label: label.into(),
        detail: detail.map(String::from),
    };
    Question {
        question: text.into(),
        options: vec![option("Local disk", Some("Simple")), option("R2", None)],
        context: Some("The recorder needs a place.".into()),
    }
}

fn asking() -> Api {
    let mut sim = Sim::new(SimConfig {
        agent: AgentScript {
            decisions: vec![vec![question("Where do we store audio?")]].into(),
            ..AgentScript::default()
        },
        ..SimConfig::default()
    });
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .decisions
            .iter()
            .any(|d| d.state == DecisionState::Asked)
    }));
    let app = router({
        let state = AppState::new(ApiConfig::new("0.1.0-test"));
        state.set_engine(sim.engine());
        state
    });
    Api { sim, app }
}

fn id_of(api: &Api) -> String {
    api.sim.task(1).decisions[0].id.to_string()
}

#[test]
fn a_question_is_read_by_id_and_listed_by_run_and_state() {
    let api = asking();
    let id = id_of(&api);
    let one = api.get(&format!("/api/v1/decision-requests/{id}"));
    assert_eq!(one.status, StatusCode::OK, "{}", one.body);
    assert_eq!(one.body["id"], id);
    assert_eq!(one.body["state"], "asked");
    assert_eq!(one.body["question"], "Where do we store audio?");
    assert_eq!(one.body["context"], "The recorder needs a place.");
    assert_eq!(
        one.body["options"],
        json!([
            { "label": "Local disk", "detail": "Simple" },
            { "label": "R2", "detail": null },
        ])
    );
    assert!(one.body["answer"].is_null());
    assert_eq!(one.body["owner"]["kind"], "stage_run");
    assert_eq!(
        one.body["owner"]["work_item_id"],
        api.sim.task(1).item.id.to_string()
    );
    assert!(one.body["project_id"].is_string());
    assert!(one.body["asked_at"].as_str().unwrap().ends_with('Z'));
    assert!(one.body["expires_at"].as_str().unwrap() > one.body["asked_at"].as_str().unwrap());

    let work_item = api.sim.task(1).item.id;
    let listed = api.get(&format!(
        "/api/v1/decision-requests?work_item_id={work_item}"
    ));
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(listed.body["items"][0]["id"], id);
    assert!(listed.body["next_cursor"].is_null());
    let none = api.get(&format!(
        "/api/v1/decision-requests?work_item_id={work_item}&state=answered,delivered"
    ));
    assert!(none.body["items"].as_array().unwrap().is_empty());
    let all = api.get("/api/v1/decision-requests");
    assert_eq!(all.body["items"].as_array().unwrap().len(), 1);

    problem(
        &api.get("/api/v1/decision-requests?state=nope"),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    problem(
        &api.get("/api/v1/decision-requests/00000000-0000-0000-0000-0000000000aa"),
        StatusCode::NOT_FOUND,
        "decision_not_found",
    );
    problem(
        &api.get(&format!(
            "/api/v1/decision-requests?work_item_id={work_item}&spec_draft_id={work_item}"
        )),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
}

#[test]
fn an_answer_is_stored_replays_with_its_key_and_is_refused_the_second_time() {
    let api = asking();
    let id = id_of(&api);
    let path = format!("/api/v1/decision-requests/{id}/answer");
    let body = json!({ "option": 1, "note": "R2 please" });

    let first = api.post(&path, Some("answer-1"), &body);
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["state"], "answered");
    assert_eq!(first.body["answer"]["option"], 1);
    assert_eq!(first.body["answer"]["note"], "R2 please");
    assert_eq!(first.body["answer"]["answered_by"], "operator:local");
    assert!(first.body["answer"]["answered_at"].is_string());

    let replay = api.post(&path, Some("answer-1"), &body);
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.body);
    assert_eq!(replay.body, first.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");

    problem(
        &api.post(&path, Some("answer-2"), &json!({ "option": 0 })),
        StatusCode::CONFLICT,
        "decision_already_answered",
    );
    problem(
        &api.post(&path, Some("answer-1"), &json!({ "option": 0 })),
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_reused",
    );
    let after = api.get(&format!("/api/v1/decision-requests/{id}"));
    assert_eq!(after.body["answer"]["note"], "R2 please");
}

#[test]
fn a_bad_answer_is_refused_and_leaves_the_question_open() {
    let api = asking();
    let id = id_of(&api);
    let path = format!("/api/v1/decision-requests/{id}/answer");
    for (n, body) in [
        json!({}),
        json!({ "note": "   " }),
        json!({ "option": 2 }),
        json!({ "option": -1 }),
        json!({ "option": 0, "note": "x".repeat(2001) }),
    ]
    .into_iter()
    .enumerate()
    {
        problem(
            &api.post(&path, Some(&format!("bad-{n}")), &body),
            StatusCode::UNPROCESSABLE_ENTITY,
            "decision_answer_invalid",
        );
    }
    problem(
        &api.post(
            &path,
            Some("extra"),
            &json!({ "option": 0, "urgent": true }),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    problem(
        &api.post(&path, None, &json!({ "option": 0 })),
        StatusCode::BAD_REQUEST,
        "idempotency_key_required",
    );
    problem(
        &api.post(
            "/api/v1/decision-requests/00000000-0000-0000-0000-0000000000aa/answer",
            Some("missing"),
            &json!({ "option": 0 }),
        ),
        StatusCode::NOT_FOUND,
        "decision_not_found",
    );
    assert_eq!(
        api.get(&format!("/api/v1/decision-requests/{id}")).body["state"],
        "asked"
    );
    let note_only = api.post(&path, Some("note"), &json!({ "note": "either" }));
    assert_eq!(note_only.status, StatusCode::OK, "{}", note_only.body);
    assert!(note_only.body["answer"]["option"].is_null());
}

#[test]
fn the_inbox_item_names_this_endpoint_and_closes_when_the_answer_is_delivered() {
    let mut api = asking();
    let id = id_of(&api);
    let items = api.get("/api/v1/inbox?kind=decision");
    assert_eq!(items.status, StatusCode::OK, "{}", items.body);
    let item = &items.body["items"][0];
    assert_eq!(item["action"]["kind"], "answer_decision");
    assert_eq!(item["action"]["params"]["decision_request_id"], id);
    assert_eq!(item["params"]["question"], "Where do we store audio?");

    let answered = api.post(
        &format!("/api/v1/decision-requests/{id}/answer"),
        Some("inbox"),
        &json!({ "option": 0 }),
    );
    assert_eq!(answered.status, StatusCode::OK, "{}", answered.body);
    assert!(
        api.get("/api/v1/inbox?kind=decision").body["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(api.sim.run_until(STEPS, |s| {
        s.task(1).decisions[0].state == DecisionState::Delivered
    }));
    let closed = api.get("/api/v1/inbox?kind=decision&state=closed");
    assert_eq!(closed.status, StatusCode::OK, "{}", closed.body);
}

#[test]
fn an_open_question_counts_in_the_pending_numbers_of_the_active_session() {
    let api = asking();
    let list = api.get("/api/v1/sessions?active=true");
    assert_eq!(list.status, StatusCode::OK, "{}", list.body);
    assert_eq!(
        list.body["items"][0]["pending"],
        json!({ "permissions": 0, "decisions": 1 })
    );
}
