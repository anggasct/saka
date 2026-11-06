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
use saka_testkit::{AgentOutcome, AgentScript, Operator, Sim, SimConfig, one_step_spec};
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
    fn new(sim: Sim) -> Api {
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

    fn get_with(&self, path: &str, headers: &[(&str, &str)]) -> Reply {
        let mut req = Request::get(path).header("host", "127.0.0.1:7420");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        self.send(req.body(Body::empty()).unwrap())
    }

    fn get(&self, path: &str) -> Reply {
        self.get_with(path, &[])
    }

    fn post(&self, path: &str, key: &str, body: &Value) -> Reply {
        self.send(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("host", "127.0.0.1:7420")
                .header("content-type", "application/json")
                .header("idempotency-key", key)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
    }
}

fn problem(r: &Reply, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.body);
    assert_eq!(r.body["code"], code, "{}", r.body);
}

fn waiting() -> Api {
    let mut sim = Sim::new(SimConfig {
        agent: AgentScript {
            outcome: AgentOutcome::Commit,
            ask: Some(("Bash".into(), json!({ "command": "make deploy" }))),
            ..AgentScript::default()
        },
        operator: Operator {
            start: true,
            merge: true,
            retry: vec![],
            answer: None,
        },
        ..SimConfig::default()
    });
    let second = sim.register("specs/second.md", &one_step_spec("Second", ""));
    assert!(second.lint_ok, "{:?}", second.issues);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| !s.task(1).permissions.is_empty()));
    Api::new(sim)
}

fn kinds(r: &Reply) -> Vec<String> {
    r.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_list_is_newest_first_filters_by_project_and_kind_and_pages_with_a_cursor() {
    let api = waiting();
    let all = api.get("/api/v1/inbox");
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    let mut got = kinds(&all);
    got.sort();
    assert_eq!(got, vec!["approval", "permission"]);
    let opened: Vec<&str> = all.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["opened_at"].as_str().unwrap())
        .collect();
    assert!(opened.windows(2).all(|w| w[0] >= w[1]), "{opened:?}");

    let permission = &api.get("/api/v1/inbox?kind=permission").body["items"][0];
    assert_eq!(permission["subject"], "permission");
    assert_eq!(permission["state"], "open");
    assert_eq!(permission["project_name"], "tasks");
    assert_eq!(permission["title"]["task_key"], "tasks-done/s1");
    let approval = &api.get("/api/v1/inbox?kind=approval").body["items"][0];
    assert!(approval["title"]["spec_path"].is_string());
    assert!(approval["title"]["task_key"].is_null());
    assert_eq!(permission["action"]["kind"], "answer_permission");
    assert_eq!(permission["dismissible"], false);
    assert_eq!(permission["params"]["tool"], "Bash");

    let project = api.sim.project_id;
    assert_eq!(
        api.get(&format!("/api/v1/inbox?project_id={project}")).body["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let other = uuid::Uuid::from_u128(77);
    assert!(
        api.get(&format!("/api/v1/inbox?project_id={other}")).body["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let first = api.get("/api/v1/inbox?limit=1");
    assert_eq!(first.body["items"].as_array().unwrap().len(), 1);
    let cursor = first.body["next_cursor"].as_str().unwrap();
    let second = api.get(&format!("/api/v1/inbox?limit=1&cursor={cursor}"));
    assert_eq!(second.body["items"].as_array().unwrap().len(), 1);
    assert!(second.body["next_cursor"].is_null());
    assert_ne!(first.body["items"][0]["id"], second.body["items"][0]["id"]);

    problem(
        &api.get("/api/v1/inbox?kind=gossip"),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    problem(
        &api.get("/api/v1/inbox?state=everything"),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
}

#[test]
fn an_unchanged_list_answers_not_modified_to_its_etag() {
    let api = waiting();
    let first = api.get("/api/v1/inbox");
    let tag = first.headers["etag"].to_str().unwrap().to_string();
    let again = api.get_with("/api/v1/inbox", &[("if-none-match", &tag)]);
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);
    assert_eq!(again.headers["etag"].to_str().unwrap(), tag);

    let approval = first.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["dismissible"] == true)
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let done = api.post(
        &format!("/api/v1/inbox/{approval}/dismiss"),
        "k-1",
        &json!({ "reason": "not now" }),
    );
    assert_eq!(done.status, StatusCode::OK, "{}", done.body);
    let changed = api.get_with("/api/v1/inbox", &[("if-none-match", &tag)]);
    assert_eq!(changed.status, StatusCode::OK);
    assert_ne!(changed.headers["etag"].to_str().unwrap(), tag);
}

#[test]
fn the_summary_counts_what_waits_what_runs_and_what_merged_today() {
    let api = waiting();
    let s = api.get("/api/v1/inbox/summary");
    assert_eq!(s.status, StatusCode::OK, "{}", s.body);
    assert_eq!(s.body["open"], 2);
    assert_eq!(
        s.body["by_kind"],
        json!({ "approval": 1, "permission": 1, "blocker": 0, "spec_unclear": 0, "decision": 0 })
    );
    assert_eq!(s.body["running"], 1);
    assert_eq!(s.body["merged_today"], 0);
    let tag = s.headers["etag"].to_str().unwrap();
    assert_eq!(
        api.get_with("/api/v1/inbox/summary", &[("if-none-match", tag)])
            .status,
        StatusCode::NOT_MODIFIED
    );
}

#[test]
fn the_summary_counts_changes_merged_since_midnight() {
    let mut sim = Sim::new(SimConfig::default());
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    let api = Api::new(sim);
    let s = api.get("/api/v1/inbox/summary");
    assert_eq!(s.body["merged_today"], 2, "{}", s.body);
    assert_eq!(s.body["open"], 0);
    assert_eq!(s.body["running"], 0);
}

#[test]
fn only_a_spec_approval_can_be_dismissed_and_the_answer_replays() {
    let api = waiting();
    let list = api.get("/api/v1/inbox");
    let find = |dismissible: bool| {
        list.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["dismissible"] == dismissible)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (approval, permission) = (find(true), find(false));

    problem(
        &api.post(
            &format!("/api/v1/inbox/{permission}/dismiss"),
            "k-1",
            &json!({ "reason": "go away" }),
        ),
        StatusCode::CONFLICT,
        "inbox_item_not_dismissible",
    );
    problem(
        &api.post(
            &format!("/api/v1/inbox/{approval}/dismiss"),
            "k-2",
            &json!({ "reason": "" }),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    problem(
        &api.post(
            &format!("/api/v1/inbox/{}/dismiss", uuid::Uuid::from_u128(5)),
            "k-3",
            &json!({ "reason": "x" }),
        ),
        StatusCode::NOT_FOUND,
        "inbox_item_not_found",
    );

    let path = format!("/api/v1/inbox/{approval}/dismiss");
    let body = json!({ "reason": "I will write it myself" });
    let first = api.post(&path, "k-4", &body);
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["state"], "dismissed");
    assert!(first.body["closed_at"].is_string());
    let seq = api.sim.engine().last_seq().unwrap();
    let replay = api.post(&path, "k-4", &body);
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.body, first.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(api.sim.engine().last_seq().unwrap(), seq);

    problem(
        &api.post(&path, "k-5", &body),
        StatusCode::CONFLICT,
        "inbox_item_closed",
    );
    let closed = api.get("/api/v1/inbox?state=closed");
    let mut states: Vec<&str> = closed.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["state"].as_str().unwrap())
        .collect();
    states.sort_unstable();
    assert_eq!(states, vec!["dismissed", "resolved"]);
    assert_eq!(kinds(&api.get("/api/v1/inbox")), vec!["permission"]);
}
