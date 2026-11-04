#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use saka_api::{ApiConfig, AppState, router};
use saka_core::vocab::{Stage, StagePhase, WorkItemState};
use saka_testkit::fixtures::{TWO_STEP_PATH, TWO_STEP_SPEC};
use saka_testkit::{
    AgentOutcome, AgentScript, CiScript, CommandScript, DOCS_REPO, Layout, SHOP_REPO, Sim,
    SimConfig, WEB_REPO, approval_spec, one_step_spec, repo_entry, saka_toml,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const STEPS: usize = 5_000;

struct Api {
    sim: Sim,
    state: AppState,
    app: Router,
    next_key: u32,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Api {
    fn new(cfg: SimConfig) -> Api {
        Api::with(Sim::new(cfg))
    }

    fn with(sim: Sim) -> Api {
        saka_testkit::logs::check_callsites_per_thread();
        let mut api_cfg = ApiConfig::new("0.1.0-test");
        api_cfg.keep_alive = Duration::from_millis(200);
        api_cfg.allowed_origins = vec!["http://localhost:5173".into()];
        let state = AppState::new(api_cfg);
        state.set_engine(sim.engine());
        let app = router(state.clone());
        Api {
            sim,
            state,
            app,
            next_key: 0,
        }
    }

    fn send(&self, req: Request<Body>) -> Reply {
        self.sim.block_on(async {
            let response = self.app.clone().oneshot(req).await.unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            Reply {
                status,
                headers,
                body,
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

    fn key(&mut self) -> String {
        self.next_key += 1;
        format!("key-{}", self.next_key)
    }

    fn post_with(
        &self,
        path: &str,
        key: Option<&str>,
        headers: &[(&str, &str)],
        body: &Value,
    ) -> Reply {
        let mut req = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("host", "127.0.0.1:7420")
            .header("content-type", "application/json");
        if let Some(k) = key {
            req = req.header("idempotency-key", k);
        }
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        self.send(req.body(Body::from(body.to_string())).unwrap())
    }

    fn post(&mut self, path: &str, headers: &[(&str, &str)], body: Value) -> Reply {
        let key = self.key();
        self.post_with(path, Some(&key), headers, &body)
    }

    fn last_seq(&self) -> i64 {
        self.sim.engine().last_seq().unwrap()
    }

    fn add_other_project(&mut self, spec_file: &str, spec: &str) -> String {
        self.sim.world.put_doc(
            "other/saka.toml",
            b"[project]\nname = \"other\"\n\n[[repo]]\nname = \"app\"\nurl = \"https://github.com/acme/tasks\"\n",
        );
        self.sim
            .world
            .put_doc(&format!("other/specs/{spec_file}"), spec.as_bytes());
        let r = self.post(
            "/api/v1/projects",
            &[],
            json!({ "workspace_url": "https://github.com/acme/tasks", "project_path": "other", "base_branch": null }),
        );
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        r.body["id"].as_str().unwrap().to_string()
    }
}

fn problem(r: &Reply, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.body);
    assert_eq!(
        r.headers["content-type"].to_str().unwrap(),
        "application/problem+json"
    );
    assert_eq!(r.body["code"], code, "{}", r.body);
    assert_eq!(r.body["title"], code);
    assert_eq!(r.body["type"], format!("urn:saka:problem:{code}"));
    assert_eq!(r.body["status"], status.as_u16());
    assert!(r.body["params"].is_object());
    assert!(r.body["correlation_id"].is_string());
}

fn ask_script() -> AgentScript {
    AgentScript {
        outcome: AgentOutcome::Commit,
        ask: Some((
            "Bash".into(),
            json!({ "command": "make deploy && echo ok" }),
        )),
        ..AgentScript::default()
    }
}

#[test]
fn a_spec_is_read_from_the_base_branch_linted_and_approved_with_if_match() {
    let mut api = Api::new(SimConfig::default());
    let project = api.add_other_project(
        "feature.md",
        &TWO_STEP_SPEC.replace("Mark tasks as done", "Feature"),
    );
    let created = api.get(&format!("/api/v1/projects/{project}"));
    assert_eq!(created.body["workspace"]["project_path"], "other");
    assert_eq!(created.body["config_commit"].as_str().unwrap().len(), 40);
    assert_eq!(created.body["repos"][0]["full_name"], "acme/tasks");

    let r = api.post(
        &format!("/api/v1/projects/{project}/specs"),
        &[],
        json!({ "path": "other/specs/feature.md" }),
    );
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let spec = r.body["id"].as_str().unwrap().to_string();
    let revision = r.body["revision"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        r.headers["etag"].to_str().unwrap(),
        format!("\"{revision}\"")
    );
    assert_eq!(
        r.body["revision"]["source_commit"],
        api.sim.world.base_head()
    );
    assert_eq!(r.body["lint"], json!({ "ok": true, "issues": [] }));
    assert_eq!(r.body["state"], "ready");
    assert_eq!(r.body["slug"], "feature");
    assert_eq!(r.body["acceptance_criteria"][1]["id"], "AC-02");
    let steps: Vec<(&str, &Value)> = r.body["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["id"].as_str().unwrap(), &s["commands"]))
        .collect();
    assert_eq!(steps.len(), 2);
    assert_eq!(
        steps[1],
        (
            "S2",
            &json!(["cd cli && go test ./...", "npm --prefix web test"])
        )
    );

    let again = api.post(
        &format!("/api/v1/projects/{project}/specs"),
        &[],
        json!({ "path": "other/specs/feature.md" }),
    );
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.body["revision"]["id"], revision.as_str());

    let approve = format!("/api/v1/specs/{spec}/approve");
    problem(
        &api.post(&approve, &[], json!({})),
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    );
    let stale = format!("\"{}\"", uuid::Uuid::now_v7());
    problem(
        &api.post(&approve, &[("if-match", &stale)], json!({})),
        StatusCode::PRECONDITION_FAILED,
        "revision_stale",
    );

    let before = api.last_seq();
    let tag = format!("\"{revision}\"");
    let r = api.post(
        &approve,
        &[("if-match", &tag)],
        json!({ "reason": "ready to go" }),
    );
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["state"], "approved");
    assert_eq!(r.body["approved_revision"]["id"], revision.as_str());
    assert_eq!(
        r.body["approved_revision"]["content_hash"],
        r.body["revision"]["content_hash"]
    );
    assert_eq!(
        r.body["approved_revision"]["source_commit"],
        r.body["revision"]["source_commit"]
    );
    assert!(r.body["steps"][0]["work_item_id"].is_string());

    let events = api.sim.engine().events_after(before, 100).unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        &kinds[..3],
        [
            "spec.spec.approved",
            "delivery.work_item.created",
            "delivery.work_item.created"
        ]
    );
    assert_eq!(events[2].seq - events[0].seq, 2);
    assert!(
        events[..3]
            .iter()
            .all(|e| e.correlation_id == events[0].correlation_id)
    );
    let items = api.get(&format!("/api/v1/work-items?spec_id={spec}"));
    assert_eq!(items.body["items"].as_array().unwrap().len(), 2);

    problem(
        &api.post(&approve, &[("if-match", &tag)], json!({})),
        StatusCode::CONFLICT,
        "spec_already_approved",
    );
}

#[test]
fn registration_refusals_carry_their_codes() {
    let mut api = Api::new(SimConfig::default());
    api.sim.world.put_doc(
        "docs/saka.toml",
        b"[project]\nname = \"tasks\"\n\n[[repo]]\nname = \"app\"\nurl = \"https://github.com/acme/tasks\"\n",
    );
    let r = api.post(
        "/api/v1/projects",
        &[],
        json!({ "workspace_url": "https://github.com/acme/tasks", "project_path": "docs" }),
    );
    problem(&r, StatusCode::CONFLICT, "project_exists");
    let r = api.post(
        "/api/v1/projects",
        &[],
        json!({ "workspace_url": "https://gitlab.example/acme/tasks", "project_path": "docs" }),
    );
    problem(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "scm_integration_unknown",
    );
    let r = api.post(
        "/api/v1/projects",
        &[],
        json!({ "workspace_url": "https://github.com/acme/tasks", "project_path": "missing" }),
    );
    problem(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "project_config_invalid",
    );
    assert_eq!(r.body["params"]["issues"][0]["code"], "file_missing");
    api.sim.world.put_doc(
        "two/saka.toml",
        b"[project]\nname = \"two\"\n\n[[repo]]\nname = \"a\"\nurl = \"https://github.com/acme/tasks\"\ndefault = true\n\n[[repo]]\nname = \"b\"\nurl = \"https://gitlab.example/acme/web\"\n",
    );
    let r = api.post(
        "/api/v1/projects",
        &[],
        json!({ "workspace_url": "https://github.com/acme/tasks", "project_path": "two" }),
    );
    problem(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "scm_integration_unknown",
    );
    assert_eq!(
        r.body["params"],
        json!({ "host": "gitlab.example", "repo": "b" })
    );
    api.sim.world.make_inaccessible("acme/gone");
    let r = api.post(
        "/api/v1/projects",
        &[],
        json!({ "workspace_url": "https://github.com/acme/gone", "project_path": "docs" }),
    );
    problem(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "scm_repo_inaccessible",
    );
    let r = api.post(
        "/api/v1/projects",
        &[],
        json!({ "workspace_url": "https://github.com/acme/tasks", "project_path": "../up" }),
    );
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    assert_eq!(
        r.body["params"]["errors"][0],
        json!({ "field": "project_path", "code": "invalid" })
    );
    let r = api.post(
        "/api/v1/projects",
        &[],
        json!({ "workspace_url": "x", "project_path": "docs", "colour": 1 }),
    );
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    assert_eq!(
        r.body["params"]["errors"][0],
        json!({ "field": "colour", "code": "unknown" })
    );
    let key = api.key();
    let bad = api.send(
        Request::post("/api/v1/projects")
            .header("host", "127.0.0.1")
            .header("idempotency-key", key)
            .body(Body::from("{not json"))
            .unwrap(),
    );
    problem(&bad, StatusCode::BAD_REQUEST, "malformed_json");

    let project = api.sim.project_id.0;
    let specs = format!("/api/v1/projects/{project}/specs");
    problem(
        &api.post(&specs, &[], json!({ "path": "docs/specs/nope.md" })),
        StatusCode::NOT_FOUND,
        "spec_file_not_found",
    );
    let r = api.post(&specs, &[], json!({ "path": "src/main.md" }));
    assert_eq!(r.body["params"]["errors"][0]["code"], "outside_project");
    api.sim
        .world
        .put_doc("docs/specs/tasks-done-copy.md", TWO_STEP_SPEC.as_bytes());
    let r = api.post(
        &specs,
        &[],
        json!({ "path": "docs/specs/tasks-done-copy.md" }),
    );
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "a different slug: {}",
        r.body
    );

    api.sim
        .world
        .put_doc("docs/specs/broken.md", b"---\nsaka: spec\n---\n# Broken\n");
    let r = api.post(&specs, &[], json!({ "path": "docs/specs/broken.md" }));
    assert_eq!(r.body["lint"]["ok"], false);
    assert_eq!(r.body["state"], "draft");
    let tag = format!("\"{}\"", r.body["revision"]["id"].as_str().unwrap());
    let r = api.post(
        &format!("/api/v1/specs/{}/approve", r.body["id"].as_str().unwrap()),
        &[("if-match", &tag)],
        json!({}),
    );
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "spec_not_ready");
    assert!(
        r.body["params"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "ac_section_missing")
    );
}

#[test]
fn a_task_branch_taken_by_another_project_in_the_repo_is_refused() {
    let mut api = Api::new(SimConfig::default());
    api.sim.approve();
    let project = api.add_other_project("tasks-done.md", TWO_STEP_SPEC);
    let r = api.post(
        &format!("/api/v1/projects/{project}/specs"),
        &[],
        json!({ "path": "other/specs/tasks-done.md" }),
    );
    let tag = format!("\"{}\"", r.body["revision"]["id"].as_str().unwrap());
    let r = api.post(
        &format!("/api/v1/specs/{}/approve", r.body["id"].as_str().unwrap()),
        &[("if-match", &tag)],
        json!({}),
    );
    problem(&r, StatusCode::CONFLICT, "task_branch_conflict");
    assert_eq!(
        r.body["params"]["branches"],
        json!(["saka/tasks-done/s1", "saka/tasks-done/s2"])
    );
}

#[test]
fn a_repeated_key_replays_the_response_and_a_reused_key_is_refused() {
    let mut api = Api::new(SimConfig::default());
    let project = api.add_other_project(
        "feature.md",
        &TWO_STEP_SPEC.replace("Mark tasks as done", "Feature"),
    );
    let path = format!("/api/v1/projects/{project}/specs");
    let body = json!({ "path": "other/specs/feature.md" });
    let first = api.post_with(&path, Some("same-key"), &[], &body);
    assert_eq!(first.status, StatusCode::CREATED);
    let seq = api.last_seq();
    let replay = api.post_with(&path, Some("same-key"), &[], &body);
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.body, first.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(api.last_seq(), seq, "a replay writes no event");

    let other = api.post_with(
        &path,
        Some("same-key"),
        &[],
        &json!({ "path": "other/specs/other.md" }),
    );
    problem(
        &other,
        StatusCode::UNPROCESSABLE_ENTITY,
        "idempotency_key_reused",
    );
    problem(
        &api.post_with(&path, None, &[], &body),
        StatusCode::BAD_REQUEST,
        "idempotency_key_required",
    );

    api.sim.approve();
    let id = api.sim.task(1).item.id.0;
    let retry = format!("/api/v1/work-items/{id}/actions/retry");
    let r1 = api.post_with(&retry, Some("retry-1"), &[], &json!({ "reason": "again" }));
    problem(&r1, StatusCode::CONFLICT, "invalid_state");
    let r2 = api.post_with(&retry, Some("retry-1"), &[], &json!({ "reason": "again" }));
    assert_eq!(
        (r2.status, &r2.body["code"]),
        (StatusCode::CONFLICT, &json!("invalid_state"))
    );
    assert_eq!(r2.headers["idempotent-replayed"], "true");
}

#[test]
fn a_failed_check_at_the_merge_commit_shows_the_gate_of_that_commit() {
    let mut cfg = SimConfig {
        ci: CiScript::Silent,
        ..SimConfig::default()
    };
    cfg.operator.merge = false;
    cfg.operator.retry = vec![];
    let mut api = Api::new(cfg);
    api.sim
        .world
        .set_recipe(Some("[gate]\nsource = \"saka\"\n"));
    for _ in 0..2 {
        api.sim
            .world
            .script_command("cd cli && go test ./...", CommandScript::Exit(1));
    }
    api.sim.approve();
    assert!(api.sim.run_until(STEPS, |s| {
        s.task(1).item.state == WorkItemState::Blocked
    }));
    let number = api.sim.task(1).change_request.unwrap().number;
    api.sim.world.merge_outside(number);
    let merge_commit = api.sim.world.base_head();
    assert!(api.sim.run_until(STEPS, |s| {
        s.task(1)
            .open_blocker()
            .is_some_and(|b| b.code.as_str() == "dod_failed")
    }));
    let id = api.sim.task(1).item.id.0;
    let detail = api.get(&format!("/api/v1/work-items/{id}"));
    assert_eq!(detail.body["blocker"]["code"], "dod_failed");
    assert_eq!(detail.body["blocker"]["params"]["reason"], "verify_failed");
    let gates = detail.body["gates"].as_array().unwrap();
    assert_eq!(gates.len(), 1, "{}", detail.body);
    assert_eq!(gates[0]["source"], "saka");
    assert_eq!(gates[0]["head_sha"], merge_commit.as_str());
    assert_eq!(gates[0]["outcome"], "failed");
    assert_eq!(detail.body["available_actions"], json!(["retry", "cancel"]));
}

#[test]
fn merge_waits_for_the_gate_and_actions_check_state_and_reason() {
    let cfg = SimConfig {
        ci: CiScript::Silent,
        ..SimConfig::default()
    };
    let mut cfg = cfg;
    cfg.operator.merge = false;
    let mut api = Api::new(cfg);
    api.sim.approve();
    assert!(api.sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingCi)
    }));
    let id = api.sim.task(1).item.id.0;
    let detail = api.get(&format!("/api/v1/work-items/{id}"));
    let etag = detail.headers["etag"].to_str().unwrap().to_string();
    assert_eq!(etag, format!("W/\"{}\"", detail.body["version"]));
    assert_eq!(detail.body["available_actions"], json!(["cancel"]));
    assert_eq!(detail.body["stage_runs"][0]["stage"], "dev");

    let merge = format!("/api/v1/work-items/{id}/actions/merge");
    problem(
        &api.post(&merge, &[], json!({})),
        StatusCode::PRECONDITION_REQUIRED,
        "precondition_required",
    );
    problem(
        &api.post(&merge, &[("if-match", "W/\"1\"")], json!({})),
        StatusCode::PRECONDITION_FAILED,
        "version_stale",
    );
    let r = api.post(&merge, &[("if-match", &etag)], json!({}));
    problem(&r, StatusCode::CONFLICT, "merge_not_ready");
    assert_eq!(r.body["params"]["gate_outcome"], Value::Null);
    assert_eq!(
        r.body["params"]["head_sha"],
        detail.body["change_request"]["head_sha"]
    );

    api.sim.world.set_ci(CiScript::Pass);
    assert!(api.sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    let detail = api.get(&format!("/api/v1/work-items/{id}"));
    assert_eq!(detail.body["available_actions"], json!(["merge", "cancel"]));
    let gates = detail.body["gates"].as_array().unwrap();
    let sources: Vec<(&str, &str, bool)> = gates
        .iter()
        .map(|g| {
            (
                g["source"].as_str().unwrap(),
                g["outcome"].as_str().unwrap(),
                g["required"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(sources, [("scm", "passed", true), ("saka", "passed", true)]);
    assert_eq!(detail.body["change_request"]["merged_at"], Value::Null);
    let etag = detail.headers["etag"].to_str().unwrap().to_string();
    let r = api.post(&merge, &[("if-match", &etag)], json!({ "reason": null }));
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
    assert_eq!(r.body["stage_phase"], "merging");
    assert!(
        api.sim
            .run_until(STEPS, |s| s.task(1).item.stage == Some(Stage::Sync)
                || s.task(1).item.state == WorkItemState::Done)
    );
    let merged = api.get(&format!("/api/v1/work-items/{id}"));
    let merged_at = merged.body["change_request"]["merged_at"].as_str().unwrap();
    assert!(merged_at.ends_with('Z'), "{merged_at}");

    let retry = format!("/api/v1/work-items/{id}/actions/retry");
    let r = api.post(&retry, &[], json!({}));
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    assert_eq!(
        r.body["params"]["errors"][0],
        json!({ "field": "reason", "code": "required" })
    );
    let r = api.post(&retry, &[], json!({ "reason": "" }));
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    problem(
        &api.post(&retry, &[], json!({ "reason": "why not" })),
        StatusCode::CONFLICT,
        "invalid_state",
    );

    let s2 = api.sim.task(2).item.id.0;
    let r = api.post(
        &format!("/api/v1/work-items/{s2}/actions/cancel"),
        &[],
        json!({ "reason": "not needed" }),
    );
    assert_eq!(
        (r.status, &r.body["state"]),
        (StatusCode::ACCEPTED, &json!("cancelled"))
    );
    problem(
        &api.post(
            &format!("/api/v1/work-items/{s2}/actions/cancel"),
            &[],
            json!({ "reason": "again" }),
        ),
        StatusCode::CONFLICT,
        "invalid_state",
    );
}

#[test]
fn a_permission_answer_is_delivered_once_and_a_second_answer_is_refused() {
    let mut api = Api::new(SimConfig {
        agent: ask_script(),
        ..SimConfig::default()
    });
    api.sim.approve();
    assert!(
        api.sim
            .run_until(STEPS, |s| !s.task(1).permissions.is_empty())
    );
    let list = api.get("/api/v1/permission-requests?state=pending");
    let items = list.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["summary"], "Bash: make deploy && echo ok");
    let id = items[0]["id"].as_str().unwrap().to_string();
    let work = api.get(&format!(
        "/api/v1/work-items/{}",
        items[0]["owner"]["work_item_id"].as_str().unwrap()
    ));
    assert_eq!(work.body["pending_permission_count"], 1);
    let session = api.get(&format!(
        "/api/v1/sessions/{}",
        items[0]["session_id"].as_str().unwrap()
    ));
    assert_eq!(session.body["state"], "waiting");

    let answer = format!("/api/v1/permission-requests/{id}/answer");
    let r = api.post(
        &answer,
        &[],
        json!({ "decision": "allow", "message": null }),
    );
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(
        (&r.body["state"], &r.body["answer_source"]),
        (&json!("allowed"), &json!("human:operator:local"))
    );
    problem(
        &api.post(&answer, &[], json!({ "decision": "deny" })),
        StatusCode::CONFLICT,
        "permission_already_answered",
    );
    problem(
        &api.post(&answer, &[], json!({ "decision": "maybe" })),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    assert!(api.sim.run_until(STEPS, |s| s.stats().answers_to_cli >= 1));
    for _ in 0..50 {
        api.sim.step();
    }
    assert_eq!(api.sim.stats().answer_frames, 1, "delivered exactly once");
}

#[test]
fn guards_errors_meta_and_pagination_follow_the_conventions() {
    let mut api = Api::new(SimConfig::default());
    let forbidden = api.send(
        Request::get("/api/v1/meta")
            .header("host", "saka.example.com")
            .body(Body::empty())
            .unwrap(),
    );
    problem(&forbidden, StatusCode::FORBIDDEN, "host_not_allowed");
    let r = api.post(
        "/api/v1/projects",
        &[("origin", "https://evil.example")],
        json!({}),
    );
    problem(&r, StatusCode::FORBIDDEN, "origin_not_allowed");
    let r = api.post(
        "/api/v1/projects",
        &[("origin", "http://127.0.0.1:7420")],
        json!({ "nope": 1 }),
    );
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    let r = api.post(
        "/api/v1/projects",
        &[("origin", "http://localhost:5173")],
        json!({ "nope": 1 }),
    );
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "configured origins pass"
    );

    problem(
        &api.get("/api/v1/nothing-here"),
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &api.get("/api/v1/specs/not-a-uuid"),
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &api.get("/api/v1/work-items?state=sleeping"),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    problem(
        &api.get("/api/v1/work-items?cursor=%%%"),
        StatusCode::UNPROCESSABLE_ENTITY,
        "cursor_invalid",
    );

    let meta = api.get("/api/v1/meta");
    assert_eq!(
        meta.body,
        json!({
            "server_version": "0.1.0-test",
            "api_version": "1",
            "min_client_api_version": "1",
            "capabilities": ["projects", "spec_approve", "work_item_merge", "work_item_retry", "work_item_cancel", "permission_answer", "events_sse", "spec_draft"],
        })
    );

    api.sim.approve();
    let first = api.get("/api/v1/work-items?limit=1");
    assert_eq!(first.body["items"].as_array().unwrap().len(), 1);
    let cursor = first.body["next_cursor"].as_str().unwrap().to_string();
    let second = api.get(&format!("/api/v1/work-items?limit=1&cursor={cursor}"));
    assert_eq!(second.body["items"].as_array().unwrap().len(), 1);
    assert_ne!(second.body["items"][0]["id"], first.body["items"][0]["id"]);
    assert!(second.body["next_cursor"].is_null());
    problem(
        &api.get(&format!(
            "/api/v1/work-items?limit=1&state=queued&cursor={cursor}"
        )),
        StatusCode::UNPROCESSABLE_ENTITY,
        "cursor_invalid",
    );
}

#[test]
fn the_committed_openapi_document_matches_the_code() {
    let committed =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/openapi.json")).unwrap();
    assert!(
        committed == saka_api::openapi_json(),
        "run `cargo run -p saka-api --example openapi`"
    );
    let doc: Value = serde_json::from_str(&committed).unwrap();
    assert_eq!(doc["openapi"], "3.1.0");
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            for (status, response) in op["responses"].as_object().unwrap() {
                if status.starts_with('4') || status.starts_with('5') {
                    if path == "/readyz" {
                        continue;
                    }
                    assert!(
                        response["content"]["application/problem+json"].is_object(),
                        "{method} {path} {status} is not problem+json"
                    );
                }
            }
        }
    }
}

#[test]
fn ops_endpoints_report_health_readiness_and_metrics() {
    let api = Api::new(SimConfig::default());
    let waiting = AppState::new(ApiConfig::new("0.1.0-test"));
    let early = router(waiting.clone());
    let r = api.sim.block_on(async {
        early
            .oneshot(
                Request::get("/readyz")
                    .header("host", "localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(api.get("/healthz").body, json!({ "status": "ok" }));

    let r = api.get("/readyz");
    assert_eq!(
        r.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "workers not started yet"
    );
    assert_eq!(r.body["checks"]["db"], "ok");
    assert_eq!(r.body["checks"]["worker"], "failing");

    let engine = api.sim.engine();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let ready = api.sim.block_on(async {
        let running = tokio::spawn(engine.run(1, rx));
        let mut status = StatusCode::SERVICE_UNAVAILABLE;
        for _ in 0..100 {
            let r = api
                .app
                .clone()
                .oneshot(
                    Request::get("/readyz")
                        .header("host", "localhost")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            status = r.status();
            if status == StatusCode::OK {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stop.send(true).unwrap();
        running.await.unwrap();
        status
    });
    assert_eq!(ready, StatusCode::OK);

    let text = api.sim.block_on(async {
        let r = api
            .app
            .clone()
            .oneshot(
                Request::get("/metrics")
                    .header("host", "localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.headers()["content-type"], "text/plain; version=0.0.4");
        String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
    });
    for family in [
        "saka_build_info",
        "saka_jobs",
        "saka_job_oldest_ready_age_seconds",
        "saka_job_runs_total",
        "saka_sessions_active",
        "saka_permission_requests_pending",
        "saka_environments_teardown_failed",
        "saka_work_items",
        "saka_stage_duration_seconds",
        "saka_adapter_requests_total",
        "saka_http_requests_total",
        "saka_sessions_resumed_total",
        "saka_budget_exhausted_total",
        "saka_usage_tokens_total",
        "saka_usage_cost_usd_micros_total",
        "saka_account_utilization",
        "saka_step_advance_seconds",
        "saka_dod_verifications_total",
    ] {
        assert!(
            text.contains(&format!("# TYPE {family} ")),
            "{family} missing:\n{text}"
        );
    }
    assert!(text.contains("saka_build_info{version=\"0.1.0-test\"} 1"));
    assert!(
        text.contains("saka_http_requests_total{route=\"/readyz\",status=\"200\"}"),
        "{text}"
    );
    let _ = &api.state;
}

#[test]
fn the_event_stream_resumes_after_the_last_event_id_with_keep_alives() {
    let api = Api::new(SimConfig::default());
    let engine = api.sim.engine();
    let start = engine.last_seq().unwrap();
    assert!(start >= 3, "registration wrote events");
    let read = |headers: &[(&str, &str)], until_events: usize| -> String {
        let mut req = Request::get("/api/v1/events").header("host", "127.0.0.1");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let req = req.body(Body::empty()).unwrap();
        api.sim.block_on(async {
            let r = api.app.clone().oneshot(req).await.unwrap();
            assert_eq!(r.status(), StatusCode::OK);
            assert_eq!(r.headers()["content-type"], "text/event-stream");
            let mut body = r.into_body();
            let mut text = String::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while text.matches("\nid: ").count() + usize::from(text.starts_with("id: "))
                < until_events
                || !text.contains(": keep-alive")
            {
                let frame = tokio::time::timeout_at(deadline, body.frame()).await;
                let Ok(Some(Ok(frame))) = frame else { break };
                if let Some(data) = frame.data_ref() {
                    text.push_str(std::str::from_utf8(data).unwrap());
                }
            }
            text
        })
    };
    let all = read(&[("last-event-id", "0")], usize::try_from(start).unwrap());
    let ids: Vec<i64> = all
        .lines()
        .filter_map(|l| l.strip_prefix("id: "))
        .map(|n| n.parse().unwrap())
        .collect();
    assert_eq!(
        ids,
        (1..=start).collect::<Vec<_>>(),
        "in order, no gap, no duplicate"
    );
    assert!(all.contains(": keep-alive"));
    assert!(all.contains("event: workspace.project.registered.v1"));
    let data = all.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
    let first: Value = serde_json::from_str(data).unwrap();
    assert_eq!(first["seq"], 1);
    assert!(first["occurred_at"].as_str().unwrap().ends_with('Z'));

    let after = read(
        &[("last-event-id", "2")],
        usize::try_from(start - 2).unwrap(),
    );
    let ids: Vec<i64> = after
        .lines()
        .filter_map(|l| l.strip_prefix("id: "))
        .map(|n| n.parse().unwrap())
        .collect();
    assert_eq!(ids, (3..=start).collect::<Vec<_>>());
    let both = read(&[("last-event-id", &start.to_string())], 0);
    assert!(!both.contains("id: "));
    problem(
        &api.get("/api/v1/events?after=abc"),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
}

impl Api {
    fn register_doc(&mut self, path: &str, text: &str) -> Reply {
        self.sim.world.put_doc(path, text.as_bytes());
        let project = self.sim.project_id.0.to_string();
        let r = self.post(
            &format!("/api/v1/projects/{project}/specs"),
            &[],
            json!({ "path": path }),
        );
        assert!(r.status.is_success(), "{}", r.body);
        r
    }

    fn approve_reply(&mut self, spec: &Reply) -> Reply {
        let id = spec.body["id"].as_str().unwrap().to_string();
        let tag = format!("\"{}\"", spec.body["revision"]["id"].as_str().unwrap());
        self.post(
            &format!("/api/v1/specs/{id}/approve"),
            &[("if-match", &tag)],
            json!({}),
        )
    }
}

const PARALLEL_WITH_WARNINGS: &str = "---
saka: spec
depends_on: [base-spec, ghost]
priority: 7
---
# Feature

## Acceptance criteria

- **AC-01**: one.
- **AC-02**: two.

## Steps

| Step | Work | AC | After | Done when |
|------|------|----|-------|-----------|
| S1 | First | AC-01 | | `make test` |
| S2 | Second | AC-01 | S1 | `make test` |
| S3 | Third | AC-01 | | `make test` |
";

#[test]
fn a_spec_shows_priority_dependencies_effective_steps_and_severities() {
    let mut api = Api::new(SimConfig::default());
    let base = api.register_doc("docs/specs/base-spec.md", &one_step_spec("Base", ""));
    let r = api.register_doc("docs/specs/feature.md", PARALLEL_WITH_WARNINGS);
    let spec = r.body["id"].as_str().unwrap().to_string();
    let detail = api.get(&format!("/api/v1/specs/{spec}"));
    let b = &detail.body;
    assert_eq!(b["priority"], 7);
    assert_eq!(
        b["depends_on"],
        json!([
            {"slug": "base-spec", "spec_id": base.body["id"], "state": "ready"},
            {"slug": "ghost", "spec_id": null, "state": null},
        ])
    );
    let after: Vec<&Value> = b["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| &s["after"])
        .collect();
    assert_eq!(after, [&json!([]), &json!(["S1"]), &json!([])]);
    assert_eq!(b["revision"]["parser_version"], 5);
    assert_eq!(b["lint"]["ok"], true);
    assert_eq!(
        b["lint"]["issues"],
        json!([
            {"code": "ac_uncovered", "severity": "warning", "line": 11, "params": {"ac": "AC-02"}},
            {"code": "step_parallel_start", "severity": "warning", "line": 19, "params": {"step": "S3"}},
        ])
    );

    let before = api.last_seq();
    let refused = api.approve_reply(&detail);
    problem(&refused, StatusCode::UNPROCESSABLE_ENTITY, "spec_not_ready");
    assert_eq!(
        refused.body["params"]["issues"],
        json!([{"code": "depends_on_unknown", "severity": "error", "line": 3, "params": {"slug": "ghost"}}])
    );
    assert_eq!(api.last_seq(), before);
}

#[test]
fn a_warnings_only_spec_is_ready_and_approves_and_tasks_show_what_they_wait_for() {
    let mut api = Api::new(SimConfig {
        max_active_tasks: 1,
        ..SimConfig::default()
    });
    let base = api.register_doc("docs/specs/base-spec.md", &one_step_spec("Base", ""));
    let text =
        PARALLEL_WITH_WARNINGS.replace("depends_on: [base-spec, ghost]", "depends_on: [base-spec]");
    let feature = api.register_doc("docs/specs/feature.md", &text);
    assert_eq!(feature.body["state"], "ready");
    let approved = api.approve_reply(&feature);
    assert_eq!(approved.status, StatusCode::OK, "{}", approved.body);
    assert_eq!(approved.body["depends_on"][0]["state"], "ready");
    let base_id = base.body["id"].as_str().unwrap().to_string();
    let spec = feature.body["id"].as_str().unwrap().to_string();

    let items = api.get(&format!("/api/v1/work-items?spec_id={spec}"));
    let items = items.body["items"].as_array().unwrap().clone();
    let by_step = |id: &str| items.iter().find(|i| i["step_id"] == id).unwrap().clone();
    let (s1, s2) = (by_step("S1"), by_step("S2"));
    let waits_on_base = json!({"kind": "spec", "slug": "base-spec", "spec_id": base_id});
    assert_eq!(s1["waiting_on"], json!([waits_on_base]));
    assert_eq!(
        s2["waiting_on"],
        json!([
            {"kind": "step", "step_id": "S1", "work_item_id": s1["id"]},
            waits_on_base,
        ])
    );
    let detail = api.get(&format!(
        "/api/v1/work-items/{}",
        s2["id"].as_str().unwrap()
    ));
    assert_eq!(detail.body["waiting_on"], s2["waiting_on"]);

    let base_approved = api.approve_reply(&base);
    assert_eq!(
        base_approved.status,
        StatusCode::OK,
        "{}",
        base_approved.body
    );
    let base_task = base_approved.body["steps"][0]["work_item_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(api.sim.run_until(STEPS, |s| {
        s.all_tasks().iter().any(|t| {
            t.item.id.0.to_string() == base_task
                && t.item.state != saka_core::vocab::WorkItemState::Queued
        })
    }));
    let running = api.get(&format!("/api/v1/work-items/{base_task}"));
    assert_ne!(running.body["state"], "queued");
    assert_eq!(running.body["waiting_on"], json!([]));
}

#[test]
fn a_revision_from_an_older_parser_must_be_read_again_before_approve() {
    let mut api = Api::new(SimConfig::default());
    let text = one_step_spec("Kept", "");
    let (spec, old) = api.sim.old_parser_spec("kept", &text);
    let before = api.last_seq();
    problem(
        &api.post(
            &format!("/api/v1/specs/{}/approve", spec.0),
            &[("if-match", &format!("\"{}\"", old.0))],
            json!({}),
        ),
        StatusCode::CONFLICT,
        "spec_reparse_required",
    );
    assert_eq!(api.last_seq(), before);

    let again = api.register_doc("docs/specs/kept.md", &text);
    assert_eq!(again.body["id"], spec.0.to_string());
    assert_ne!(again.body["revision"]["id"], old.0.to_string());
    assert_eq!(again.body["revision"]["parser_version"], 5);
    let versions: Vec<i64> = api
        .sim
        .engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT parser_version FROM spec_revision WHERE spec_id = ?1 ORDER BY parser_version",
            )?;
            let rows = stmt.query_map([spec.0.to_string()], |r| r.get(0))?;
            Ok(rows.collect::<Result<Vec<i64>, _>>()?)
        })
        .unwrap();
    assert_eq!(versions, vec![1, 5]);
    assert_eq!(api.approve_reply(&again).status, StatusCode::OK);
}

#[test]
fn an_open_decision_blocks_approve_until_one_option_is_checked() {
    let mut api = Api::new(SimConfig::default());
    let open = format!(
        "{}\n## Decisions\n\n### Q1: Where are uploads stored?\n\n- [ ] **Local disk** (recommended) — one node.\n- [ ] **Object storage** — scales out.\n",
        one_step_spec("Uploads", "")
    );
    let path = "docs/specs/uploads.md";
    let spec = api.register_doc(path, &open);
    assert_eq!(spec.body["state"], "draft");
    assert_eq!(spec.body["lint"]["ok"], false);
    let issues = spec.body["lint"]["issues"].as_array().unwrap();
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert_eq!(issues[0]["code"], "decision_unanswered");
    assert_eq!(issues[0]["params"], json!({"id": "Q1"}));
    assert_eq!(spec.body["decisions"][0]["chosen"], Value::Null);

    let before = api.last_seq();
    let refused = api.approve_reply(&spec);
    problem(&refused, StatusCode::UNPROCESSABLE_ENTITY, "spec_not_ready");
    assert_eq!(
        refused.body["params"]["issues"][0]["code"],
        "decision_unanswered"
    );
    assert_eq!(api.last_seq(), before);

    let answered = open.replacen("- [ ] **Local disk**", "- [x] **Local disk**", 1);
    assert_eq!(answered.len(), open.len());
    let spec = api.register_doc(path, &answered);
    assert_eq!(spec.body["state"], "ready");
    assert_eq!(spec.body["lint"]["ok"], true);
    assert_eq!(
        spec.body["decisions"],
        json!([{
            "id": "Q1",
            "question": "Where are uploads stored?",
            "line": 16,
            "options": [
                {"text": "**Local disk** (recommended) — one node.", "line": 18, "recommended": true, "chosen": true},
                {"text": "**Object storage** — scales out.", "line": 19, "recommended": false, "chosen": false},
            ],
            "chosen": 1,
        }])
    );
    let id = spec.body["id"].as_str().unwrap().to_string();
    let listed = api.get(&format!("/api/v1/projects/{}/specs", api.sim.project_id.0));
    let item = listed.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id.as_str())
        .unwrap()
        .clone();
    assert_eq!(item["decisions"], json!([]));
    assert_eq!(api.approve_reply(&spec).status, StatusCode::OK);
}

#[test]
fn the_spec_template_comes_in_english_and_indonesian() {
    let api = Api::new(SimConfig::default());
    let en = api.get("/api/v1/spec-template");
    assert_eq!(en.status, StatusCode::OK);
    assert_eq!(en.body["locale"], "en");
    assert_eq!(en.body["parser_version"], 5);
    let id = api.get("/api/v1/spec-template?locale=id");
    assert_eq!(id.body["locale"], "id");
    assert_ne!(id.body["content"], en.body["content"]);
    for reply in [&en, &id] {
        let content = reply.body["content"].as_str().unwrap();
        let parsed = saka_spec::parse("example-spec.md", content.as_bytes());
        assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
        assert!(parsed.is_ready());
    }
    let bad = api.get("/api/v1/spec-template?locale=fr");
    problem(&bad, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    assert_eq!(bad.body["params"]["errors"][0]["field"], "locale");
}

const APPROVAL: &str = "docs/specs/approval.md";

fn rows(api: &Api, sql: &str) -> Vec<String> {
    api.sim
        .engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(sql)?;
            let out = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(out)
        })
        .unwrap()
}

impl Api {
    fn spec_id(r: &Reply) -> saka_core::ids::SpecId {
        saka_core::ids::SpecId(r.body["id"].as_str().unwrap().parse().unwrap())
    }

    fn run_to(&mut self, spec: saka_core::ids::SpecId, step: usize, state: WorkItemState) {
        assert!(
            self.sim
                .run_until(STEPS, |s| s.tasks_of(spec)[step - 1].item.state == state),
            "S{step} never reached {state}"
        );
    }

    fn items(&self, spec: &str) -> Vec<Value> {
        let r = self.get(&format!("/api/v1/work-items?spec_id={spec}&limit=200"));
        let mut items = r.body["items"].as_array().unwrap().clone();
        items.sort_by_key(|i| i["step_index"].as_u64());
        items
    }

    fn approve_tag(&mut self, spec: &str, revision: &str) -> Reply {
        let tag = format!("\"{revision}\"");
        self.post(
            &format!("/api/v1/specs/{spec}/approve"),
            &[("if-match", &tag)],
            json!({}),
        )
    }

    fn get_text(&self, path: &str) -> String {
        self.sim.block_on(async {
            let r = self
                .app
                .clone()
                .oneshot(
                    Request::get(path)
                        .header("host", "localhost")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            String::from_utf8(r.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
        })
    }

    fn withdraw(&mut self, spec: &str, reason: &str) -> Reply {
        self.post(
            &format!("/api/v1/specs/{spec}/withdraw"),
            &[],
            json!({ "reason": reason }),
        )
    }
}

fn kinds(events: &[saka_engine::StoredEvent]) -> Vec<&str> {
    events.iter().map(|e| e.kind.as_str()).collect()
}

#[test]
fn re_approval_repins_supersedes_restores_and_adds_as_the_plan_said() {
    let mut api = Api::new(SimConfig {
        max_active_tasks: 1,
        ..SimConfig::default()
    });
    let base = api.register_doc("docs/specs/base-spec.md", &one_step_spec("Base", ""));
    let base_id = base.body["id"].as_str().unwrap().to_string();
    let v1 = api.register_doc(
        APPROVAL,
        &approval_spec(
            "",
            &[("One", "AC-01"), ("Two", "AC-02"), ("Three", "AC-02")],
        ),
    );
    let spec = v1.body["id"].as_str().unwrap().to_string();
    let first = api.approve_reply(&v1);
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["revision_plan"], Value::Null);
    let approved_at = first.body["approved_at"].clone();
    api.run_to(Api::spec_id(&v1), 1, WorkItemState::Done);
    let items = api.items(&spec);
    let ids: Vec<String> = items
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(items[1]["state"], "queued");
    assert_eq!(items[2]["state"], "queued");

    let v2 = api.register_doc(
        APPROVAL,
        &approval_spec(
            "depends_on: [base-spec]\npriority: 5\n",
            &[("One", "AC-01"), ("Two, rewritten", "AC-01, AC-02")],
        ),
    );
    let v1_rev = v1.body["revision"]["id"].as_str().unwrap().to_string();
    let v2_rev = v2.body["revision"]["id"].as_str().unwrap().to_string();
    assert_eq!(v2.body["state"], "approved");
    let plan = api.get(&format!("/api/v1/specs/{spec}")).body["revision_plan"].clone();
    assert_eq!(
        plan,
        json!({
            "from_revision_id": v1_rev,
            "to_revision_id": v2_rev,
            "ready": true,
            "steps": [
                {"step_id": "S1", "action": "keep", "work_item_id": ids[0], "reason": null, "fields": []},
                {"step_id": "S2", "action": "repin", "work_item_id": ids[1], "reason": null, "fields": []},
                {"step_id": "S3", "action": "supersede", "work_item_id": ids[2], "reason": null, "fields": []},
            ]
        })
    );

    let before = api.last_seq();
    let r = api.approve_tag(&spec, &v2_rev);
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["approved_revision"]["id"], v2_rev.as_str());
    assert_eq!(r.body["approved_at"], approved_at);
    assert_eq!(r.body["priority"], 5);
    assert_eq!(r.body["revision_plan"], Value::Null);
    let events = api.sim.engine().events_after(before, 100).unwrap();
    let mine: Vec<_> = events
        .iter()
        .filter(|e| e.correlation_id == events[0].correlation_id)
        .cloned()
        .collect();
    assert_eq!(
        kinds(&mine),
        [
            "spec.spec.approved",
            "delivery.work_item.repinned",
            "delivery.work_item.superseded"
        ]
    );
    assert_eq!(mine[0].payload["previous_revision_id"], v1_rev.as_str());
    let planned: Vec<(&str, &Value)> = plan["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["action"] != "keep")
        .map(|s| (s["action"].as_str().unwrap(), &s["work_item_id"]))
        .collect();
    let happened: Vec<(&str, &Value)> = mine[1..]
        .iter()
        .map(|e| {
            let action = match e.kind.as_str() {
                "delivery.work_item.repinned" => "repin",
                "delivery.work_item.superseded" => "supersede",
                other => other,
            };
            (action, &e.payload["work_item_id"])
        })
        .collect();
    assert_eq!(planned, happened);

    let items = api.items(&spec);
    assert_eq!(items[0]["spec_revision_id"], v1_rev.as_str());
    assert_eq!(items[1]["spec_revision_id"], v2_rev.as_str());
    assert_eq!(items[1]["work"], "Two, rewritten");
    assert_eq!(items[1]["priority"], 5);
    assert_eq!(items[2]["state"], "superseded");
    assert_eq!(items[2]["available_actions"], json!([]));
    assert_eq!(items[2]["waiting_on"], json!([]));
    assert_eq!(
        items[1]["waiting_on"],
        json!([{"kind": "spec", "slug": "base-spec", "spec_id": base_id}])
    );
    assert_eq!(
        rows(&api, "SELECT depends_on_slug FROM spec_dependency"),
        vec!["base-spec"]
    );
    assert_eq!(
        rows(
            &api,
            "SELECT w.step_id || '>' || p.step_id FROM work_item_dependency d
               JOIN work_item w ON w.id = d.work_item_id JOIN work_item p ON p.id = d.depends_on_work_item_id
             WHERE w.spec_id = (SELECT id FROM spec WHERE slug = 'approval') ORDER BY 1"
        ),
        vec!["S2>S1", "S3>S2"]
    );
    problem(
        &api.post(
            &format!("/api/v1/work-items/{}/actions/cancel", ids[2]),
            &[],
            json!({ "reason": "stop" }),
        ),
        StatusCode::CONFLICT,
        "invalid_state",
    );
    let metrics = api.get_text("/metrics");
    assert!(
        metrics.contains("saka_work_items{state=\"superseded\"} 1"),
        "{metrics}"
    );
    problem(
        &api.approve_tag(&spec, &v2_rev),
        StatusCode::CONFLICT,
        "spec_already_approved",
    );

    let v3 = api.register_doc(
        APPROVAL,
        &approval_spec(
            "",
            &[
                ("One", "AC-01"),
                ("Two, rewritten", "AC-01, AC-02"),
                ("Three, back", "AC-02"),
                ("Four", "AC-02"),
            ],
        ),
    );
    let v3_rev = v3.body["revision"]["id"].as_str().unwrap().to_string();
    let before = api.last_seq();
    let r = api.approve_tag(&spec, &v3_rev);
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["approved_at"], approved_at);
    let events = api.sim.engine().events_after(before, 100).unwrap();
    let mine: Vec<_> = events
        .iter()
        .filter(|e| e.correlation_id == events[0].correlation_id)
        .cloned()
        .collect();
    assert_eq!(
        kinds(&mine),
        [
            "spec.spec.approved",
            "delivery.work_item.repinned",
            "delivery.work_item.repinned",
            "delivery.work_item.created"
        ]
    );
    assert_eq!(mine[1].payload["restored"], false);
    assert_eq!(mine[2].payload["work_item_id"], ids[2].as_str());
    assert_eq!(mine[2].payload["restored"], true);
    let items = api.items(&spec);
    assert_eq!(items.len(), 4);
    assert_eq!(items[2]["state"], "queued");
    assert_eq!(items[2]["task_key"], "approval/s3");
    assert_eq!(items[2]["branch"], "saka/approval/s3");
    assert_eq!(items[2]["spec_revision_id"], v3_rev.as_str());
    assert_eq!(items[3]["task_key"], "approval/s4");
    assert_eq!(
        rows(&api, "SELECT depends_on_slug FROM spec_dependency"),
        Vec::<String>::new()
    );
    assert_eq!(
        rows(
            &api,
            "SELECT w.step_id || '>' || p.step_id FROM work_item_dependency d
               JOIN work_item w ON w.id = d.work_item_id JOIN work_item p ON p.id = d.depends_on_work_item_id
             WHERE w.spec_id = (SELECT id FROM spec WHERE slug = 'approval') ORDER BY 1"
        ),
        vec!["S2>S1", "S3>S2", "S4>S3"]
    );
}

#[test]
fn re_approval_refuses_changes_to_started_steps_without_writing() {
    let mut api = Api::new(SimConfig {
        max_active_tasks: 1,
        ..SimConfig::default()
    });
    let v1 = api.register_doc(
        APPROVAL,
        "---\nsaka: spec\n---\n# Approval\n\n## Acceptance criteria\n\n- **AC-01**: one.\n- **AC-02**: two.\n\n## Steps\n\n| Step | Work | AC | After | Done when |\n|------|------|----|-------|-----------|\n| S1 | One | AC-01 | | `make test` |\n| S2 | Two | AC-02 | S1 | `make test` |\n| S3 | Three | AC-02 | S2 | `make test` |\n",
    );
    let spec = v1.body["id"].as_str().unwrap().to_string();
    assert_eq!(api.approve_reply(&v1).status, StatusCode::OK);
    api.run_to(Api::spec_id(&v1), 1, WorkItemState::Done);
    let s1 = api.items(&spec)[0]["id"].as_str().unwrap().to_string();
    let table = |s1_row: &str| {
        format!(
            "---\nsaka: spec\n---\n# Approval\n\n## Acceptance criteria\n\n- **AC-01**: one.\n- **AC-02**: two.\n\n## Steps\n\n| Step | Work | AC | After | Done when |\n|------|------|----|-------|-----------|\n{s1_row}\n| S2 | Two | AC-01, AC-02 | | `make test` |\n| S3 | Three | AC-02 | S2 | `make test` |\n"
        )
    };
    let snapshot = |api: &Api| {
        (
            rows(
                api,
                "SELECT id || state || spec_revision_id || work || version FROM work_item ORDER BY id",
            ),
            rows(
                api,
                "SELECT state || approved_revision_id || approved_at FROM spec WHERE slug = 'approval'",
            ),
            api.last_seq(),
        )
    };
    let cases = [
        ("| S1 | One, changed | AC-01 | | `make test` |", "work"),
        ("| S1 | One | AC-01, AC-02 | | `make test` |", "ac"),
        ("| S1 | One | AC-01 | | `make check` |", "commands"),
        ("| S1 | One | AC-01 | S2 | `make test` |", "after"),
    ];
    for (row, field) in cases {
        let r = api.register_doc(APPROVAL, &table(row));
        let revision = r.body["revision"]["id"].as_str().unwrap().to_string();
        let before = snapshot(&api);
        let refused = api.approve_tag(&spec, &revision);
        problem(&refused, StatusCode::CONFLICT, "revision_conflict");
        assert_eq!(
            refused.body["params"]["steps"],
            json!([{"step_id": "S1", "reason": "changed", "fields": [field], "work_item_id": s1, "state": "done"}]),
            "{field}"
        );
        assert_eq!(snapshot(&api), before, "{field}");
        let plan = api.get(&format!("/api/v1/specs/{spec}")).body["revision_plan"].clone();
        assert_eq!(plan["ready"], false);
        assert_eq!(plan["steps"][0]["action"], "conflict");
    }

    let mut api = Api::new(SimConfig {
        max_active_tasks: 1,
        ..SimConfig::default()
    });
    let v1 = api.register_doc(
        APPROVAL,
        &approval_spec("", &[("One", "AC-01"), ("Two", "AC-02")]),
    );
    let spec = v1.body["id"].as_str().unwrap().to_string();
    assert_eq!(api.approve_reply(&v1).status, StatusCode::OK);
    api.run_to(Api::spec_id(&v1), 2, WorkItemState::Admitted);
    let s2 = api.items(&spec)[1]["id"].as_str().unwrap().to_string();
    let shorter = api.register_doc(APPROVAL, &approval_spec("", &[("One", "AC-01")]));
    let revision = shorter.body["revision"]["id"].as_str().unwrap().to_string();
    let before = snapshot(&api);
    let refused = api.approve_tag(&spec, &revision);
    problem(&refused, StatusCode::CONFLICT, "revision_conflict");
    assert_eq!(
        refused.body["params"]["steps"],
        json!([{"step_id": "S2", "reason": "removed", "fields": [], "work_item_id": s2, "state": "admitted"}])
    );
    assert_eq!(snapshot(&api), before);
}

#[test]
fn re_approval_runs_the_checks_of_the_first_approval() {
    let mut api = Api::new(SimConfig {
        max_active_tasks: 0,
        ..SimConfig::default()
    });
    let v1 = api.register_doc(
        APPROVAL,
        &approval_spec("", &[("One", "AC-01"), ("Two", "AC-02")]),
    );
    let spec = v1.body["id"].as_str().unwrap().to_string();
    let v1_rev = v1.body["revision"]["id"].as_str().unwrap().to_string();
    assert_eq!(api.approve_reply(&v1).status, StatusCode::OK);
    let unchanged = |api: &Api| {
        (
            rows(
                api,
                "SELECT state || approved_revision_id FROM spec WHERE slug = 'approval'",
            ),
            api.last_seq(),
        )
    };

    let bad = api.register_doc(
        APPROVAL,
        &approval_spec("", &[("One", "AC-01"), ("Two", "AC-09")]),
    );
    assert_eq!(bad.body["state"], "approved");
    assert_eq!(bad.body["lint"]["ok"], false);
    let before = unchanged(&api);
    problem(
        &api.approve_reply(&bad),
        StatusCode::UNPROCESSABLE_ENTITY,
        "spec_not_ready",
    );
    problem(
        &api.approve_tag(&spec, &v1_rev),
        StatusCode::PRECONDITION_FAILED,
        "revision_stale",
    );
    assert_eq!(unchanged(&api), before);

    let ghost = api.register_doc(
        APPROVAL,
        &approval_spec(
            "depends_on: [ghost]\n",
            &[("One", "AC-01"), ("Two", "AC-02"), ("Three", "AC-02")],
        ),
    );
    let before = unchanged(&api);
    let refused = api.approve_reply(&ghost);
    problem(&refused, StatusCode::UNPROCESSABLE_ENTITY, "spec_not_ready");
    assert_eq!(
        refused.body["params"]["issues"][0]["code"],
        "depends_on_unknown"
    );
    assert_eq!(unchanged(&api), before);

    let old = api
        .sim
        .old_parser_revision(Api::spec_id(&v1), "an old reading");
    let before = unchanged(&api);
    problem(
        &api.approve_tag(&spec, &old.0.to_string()),
        StatusCode::CONFLICT,
        "spec_reparse_required",
    );
    assert_eq!(unchanged(&api), before);

    let other = api.add_other_project("unused.md", &one_step_spec("Unused", ""));
    let other = saka_core::ids::ProjectId(other.parse().unwrap());
    api.sim.occupy_branch(other, "saka/approval/s3");
    let longer = api.register_doc(
        APPROVAL,
        &approval_spec(
            "",
            &[("One", "AC-01"), ("Two", "AC-02"), ("Three", "AC-02")],
        ),
    );
    let before = unchanged(&api);
    let refused = api.approve_reply(&longer);
    problem(&refused, StatusCode::CONFLICT, "task_branch_conflict");
    assert_eq!(
        refused.body["params"]["branches"],
        json!(["saka/approval/s3"])
    );
    assert_eq!(unchanged(&api), before);

    let mut api = Api::new(SimConfig::default());
    let solo = api.register_doc("docs/specs/solo.md", &one_step_spec("Solo", ""));
    let solo_id = solo.body["id"].as_str().unwrap().to_string();
    assert_eq!(api.approve_reply(&solo).status, StatusCode::OK);
    assert!(api.sim.run_until(STEPS, |s| {
        s.spec_of(Api::spec_id(&solo)).state == saka_core::vocab::SpecState::Shipped
    }));
    let later = api.register_doc("docs/specs/solo.md", &one_step_spec("Solo again", ""));
    assert_eq!(later.body["state"], "shipped");
    assert_eq!(later.body["revision_plan"], Value::Null);
    let refused = api.approve_reply(&later);
    problem(&refused, StatusCode::CONFLICT, "invalid_state");
    assert_eq!(refused.body["params"]["state"], "shipped");
    problem(
        &api.withdraw(&solo_id, "late"),
        StatusCode::CONFLICT,
        "invalid_state",
    );
}

#[test]
fn withdraw_closes_a_spec_that_has_no_active_tasks() {
    let mut api = Api::new(SimConfig {
        max_active_tasks: 1,
        ..SimConfig::default()
    });
    let draft = api.register_doc(
        "docs/specs/draft.md",
        &approval_spec("", &[("One", "AC-09")]),
    );
    assert_eq!(draft.body["state"], "draft");
    let r = api.withdraw(draft.body["id"].as_str().unwrap(), "not now");
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["state"], "withdrawn");
    let ready = api.register_doc("docs/specs/ready.md", &one_step_spec("Ready", ""));
    let r = api.withdraw(ready.body["id"].as_str().unwrap(), "not now");
    assert_eq!(r.body["state"], "withdrawn");
    assert_eq!(r.body["approved_revision"], Value::Null);

    let target = api.register_doc(
        APPROVAL,
        &approval_spec(
            "",
            &[("One", "AC-01"), ("Two", "AC-02"), ("Three", "AC-02")],
        ),
    );
    let spec = target.body["id"].as_str().unwrap().to_string();
    let later = api.register_doc(
        "docs/specs/later.md",
        &one_step_spec("Later", "depends_on: [approval]\n"),
    );
    assert_eq!(api.approve_reply(&target).status, StatusCode::OK);
    assert_eq!(api.approve_reply(&later).status, StatusCode::OK);
    for reason in [String::new(), "x".repeat(501)] {
        problem(
            &api.withdraw(&spec, &reason),
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
        );
    }

    api.run_to(Api::spec_id(&target), 1, WorkItemState::InStage);
    let items = api.items(&spec);
    let before = api.last_seq();
    let busy = api.withdraw(&spec, "not needed");
    problem(&busy, StatusCode::CONFLICT, "spec_has_active_tasks");
    assert_eq!(
        busy.body["params"]["tasks"],
        json!([{"work_item_id": items[0]["id"], "step_id": "S1", "state": "in_stage"}])
    );
    assert_eq!(api.last_seq(), before);

    api.run_to(Api::spec_id(&target), 1, WorkItemState::Done);
    let cancelled = api.post(
        &format!(
            "/api/v1/work-items/{}/actions/cancel",
            items[1]["id"].as_str().unwrap()
        ),
        &[],
        json!({ "reason": "not this one" }),
    );
    assert_eq!(cancelled.status, StatusCode::ACCEPTED, "{}", cancelled.body);

    let path = format!("/api/v1/specs/{spec}/withdraw");
    let body = json!({ "reason": "not needed" });
    let before = api.last_seq();
    let r = api.post_with(&path, Some("withdraw-once"), &[], &body);
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["state"], "withdrawn");
    assert_eq!(
        r.body["approved_revision"]["id"],
        target.body["revision"]["id"]
    );
    let events = api.sim.engine().events_after(before, 100).unwrap();
    assert_eq!(
        kinds(&events),
        ["spec.spec.withdrawn", "delivery.work_item.superseded"]
    );
    assert_eq!(events[0].payload["reason"], "not needed");
    assert_eq!(events[1].payload["reason"], "spec_withdrawn");
    let states: Vec<Value> = api
        .items(&spec)
        .iter()
        .map(|i| i["state"].clone())
        .collect();
    assert_eq!(states, ["done", "cancelled", "superseded"]);
    let replay = api.post_with(&path, Some("withdraw-once"), &[], &body);
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.body, r.body);
    problem(
        &api.withdraw(&spec, "again"),
        StatusCode::CONFLICT,
        "invalid_state",
    );

    let again = api.register_doc(
        APPROVAL,
        &approval_spec(
            "",
            &[
                ("One", "AC-01"),
                ("Two", "AC-02"),
                ("Three, changed", "AC-02"),
            ],
        ),
    );
    assert_eq!(again.body["state"], "withdrawn");
    problem(
        &api.approve_reply(&again),
        StatusCode::CONFLICT,
        "invalid_state",
    );

    let waiting = api.items(later.body["id"].as_str().unwrap());
    assert_eq!(waiting[0]["state"], "queued");
    assert_eq!(
        waiting[0]["waiting_on"],
        json!([{"kind": "spec", "slug": "approval", "spec_id": spec}])
    );
}

#[test]
fn metrics_count_the_environments_that_wait_for_cleanup() {
    let mut api = Api::new(SimConfig::default());
    let before = api.get_text("/metrics");
    assert!(
        before.contains("\nsaka_environments_teardown_failed 0\n"),
        "{before}"
    );
    api.sim.world.refuse_teardowns(true);
    api.sim.approve();
    assert!(api.sim.run_to_shipped(STEPS));
    let after = api.get_text("/metrics");
    assert!(
        after.contains("\nsaka_environments_teardown_failed 2\n"),
        "{after}"
    );
}

fn hang_script() -> AgentScript {
    AgentScript {
        outcome: AgentOutcome::Hang,
        ..AgentScript::default()
    }
}

fn running_session(api: &mut Api) -> (String, String) {
    api.sim.approve();
    assert!(api.sim.run_until(STEPS, |s| {
        s.task(1).session.is_some_and(|x| {
            x.state == saka_core::vocab::SessionState::Running && x.cli_session_id.is_some()
        })
    }));
    let t = api.sim.task(1);
    (t.item.id.to_string(), t.session.unwrap().id.to_string())
}

fn rejected(api: &Api, path: &str, field: &str, code: &str) {
    let r = api.get(path);
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    assert_eq!(
        r.body["params"]["errors"][0],
        json!({ "field": field, "code": code }),
        "{path}"
    );
}

#[test]
fn runtime_lists_filter_by_exactly_one_run_owner() {
    let mut api = Api::new(SimConfig {
        agent: hang_script(),
        ..SimConfig::default()
    });
    let (item, sid) = running_session(&mut api);
    let stage_run = api.sim.task(1).session.unwrap().owner.stage_run_id();
    let draft = uuid::Uuid::now_v7();

    let list = api.get(&format!("/api/v1/sessions?work_item_id={item}"));
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.body["items"][0]["id"], sid.as_str());
    assert_eq!(
        list.body["items"][0]["owner"],
        json!({ "kind": "stage_run", "work_item_id": item, "stage_run_id": stage_run })
    );
    assert!(list.body["items"][0].get("work_item_id").is_none());
    assert!(list.body["items"][0].get("stage_run_id").is_none());
    let drafts = api.get(&format!("/api/v1/sessions?spec_draft_id={draft}"));
    assert_eq!(drafts.status, StatusCode::OK);
    assert_eq!(drafts.body["items"], json!([]));

    rejected(&api, "/api/v1/sessions", "work_item_id", "required");
    for path in [
        "/api/v1/sessions",
        "/api/v1/permission-requests",
        "/api/v1/usage-records",
    ] {
        rejected(
            &api,
            &format!("{path}?work_item_id={item}&spec_draft_id={draft}"),
            "spec_draft_id",
            "exclusive",
        );
        rejected(
            &api,
            &format!("{path}?spec_draft_id=nope"),
            "spec_draft_id",
            "invalid",
        );
    }
    for path in ["/api/v1/permission-requests", "/api/v1/usage-records"] {
        let r = api.get(&format!("{path}?spec_draft_id={draft}"));
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        assert_eq!(r.body["items"], json!([]));
    }
}

#[test]
fn operators_steer_interrupt_and_stop_a_session_over_the_api() {
    let mut api = Api::new(SimConfig {
        agent: hang_script(),
        ..SimConfig::default()
    });
    let (item, sid) = running_session(&mut api);
    let list = api.get(&format!("/api/v1/sessions?work_item_id={item}"));
    assert_eq!(list.status, StatusCode::OK);
    let items = list.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0]["available_actions"],
        json!(["steer", "interrupt", "stop"])
    );
    assert_eq!(items[0]["resumed_from_session_id"], Value::Null);
    problem(
        &api.get("/api/v1/sessions"),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );

    let steer = format!("/api/v1/sessions/{sid}/steer");
    let r = api.post_with(
        &steer,
        Some("steer-1"),
        &[],
        &json!({ "text": "use the helper" }),
    );
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
    assert_eq!(r.body["state"], "running");
    let again = api.post_with(
        &steer,
        Some("steer-1"),
        &[],
        &json!({ "text": "use the helper" }),
    );
    assert_eq!((again.status, &again.body), (r.status, &r.body), "replayed");
    for bad in [
        json!({ "text": "" }),
        json!({ "text": "x".repeat(4001) }),
        json!({}),
    ] {
        problem(
            &api.post(&steer, &[], bad),
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
        );
    }
    assert!(api.sim.run_until(STEPS, |s| s.stats().steers_to_cli == 1));
    assert_eq!(
        rows(
            &api,
            "SELECT action || ':' || actor FROM action_request WHERE action = 'session.steer' AND response_status = 202"
        ),
        vec!["session.steer:operator:local"]
    );

    let interrupt = format!("/api/v1/sessions/{sid}/interrupt");
    let r = api.post(&interrupt, &[], json!({}));
    assert_eq!(
        (r.status, &r.body["state"]),
        (StatusCode::ACCEPTED, &json!("interrupted"))
    );
    assert!(api.sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == saka_core::vocab::SessionState::Idle)
    }));
    assert_eq!(api.sim.task(1).open_blocker(), None);
    problem(
        &api.post(&interrupt, &[], json!({})),
        StatusCode::CONFLICT,
        "invalid_state",
    );
    let r = api.post(&steer, &[], json!({ "text": "try the other way" }));
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);

    let stop = format!("/api/v1/sessions/{sid}/stop");
    problem(
        &api.post(&stop, &[], json!({ "reason": "" })),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    let r = api.post(&stop, &[], json!({ "reason": "wrong approach" }));
    assert_eq!(
        (r.status, &r.body["state"]),
        (StatusCode::ACCEPTED, &json!("failed"))
    );
    assert_eq!(
        r.body["end_reason"],
        json!({ "code": "operator_stop", "params": { "reason": "wrong approach" } })
    );
    let work = api.get(&format!("/api/v1/work-items/{item}"));
    assert_eq!(
        (
            &work.body["blocker"]["code"],
            &work.body["blocker"]["params"]
        ),
        (
            &json!("session_failed"),
            &json!({ "cause": "operator_stop", "reason": "wrong approach" })
        )
    );
    problem(
        &api.post(&stop, &[], json!({ "reason": "again" })),
        StatusCode::CONFLICT,
        "invalid_state",
    );
    let missing = format!("/api/v1/sessions/{}/steer", uuid::Uuid::nil());
    problem(
        &api.post(&missing, &[], json!({ "text": "x" })),
        StatusCode::NOT_FOUND,
        "not_found",
    );

    let timeline = api.get(&format!("/api/v1/sessions/{sid}/timeline"));
    assert_eq!(timeline.status, StatusCode::OK);
    let items = timeline.body["items"].as_array().unwrap();
    let offsets: Vec<u64> = items
        .iter()
        .map(|i| i["offset"].as_u64().unwrap())
        .collect();
    assert!(offsets.windows(2).all(|w| w[0] < w[1]), "{offsets:?}");
    let kinds: Vec<&str> = items.iter().map(|i| i["kind"].as_str().unwrap()).collect();
    for kind in ["system", "steer", "result"] {
        assert!(kinds.contains(&kind), "{kinds:?}");
    }
    let page = api.get(&format!("/api/v1/sessions/{sid}/timeline?limit=1"));
    assert_eq!(page.body["items"].as_array().unwrap().len(), 1);
    let next = page.body["next_after"].as_u64().unwrap();
    let rest = api.get(&format!("/api/v1/sessions/{sid}/timeline?after={next}"));
    assert_eq!(
        rest.body["items"].as_array().unwrap().len(),
        items.len() - 1
    );
    problem(
        &api.get(&format!("/api/v1/sessions/{sid}/timeline?limit=501")),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    problem(
        &api.get(&format!("/api/v1/sessions/{}/timeline", uuid::Uuid::nil())),
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[test]
fn a_remembered_answer_makes_a_project_rule_that_can_be_revoked() {
    let mut api = Api::new(SimConfig {
        agent: ask_script(),
        ..SimConfig::default()
    });
    api.sim.approve();
    assert!(
        api.sim
            .run_until(STEPS, |s| !s.task(1).permissions.is_empty())
    );
    let id = api.sim.task(1).permissions[0].id.to_string();
    let answer = format!("/api/v1/permission-requests/{id}/answer");
    let r = api.post(
        &answer,
        &[],
        json!({ "decision": "allow", "remember": true }),
    );
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let project = api.sim.project_id.to_string();
    let rules = api.get(&format!("/api/v1/projects/{project}/permission-rules"));
    let items = rules.body["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1);
    assert_eq!(
        (
            &items[0]["tool"],
            &items[0]["command"],
            &items[0]["created_by"]
        ),
        (
            &json!("Bash"),
            &json!("make deploy && echo ok"),
            &json!("human:operator:local")
        )
    );
    let rule = items[0]["id"].as_str().unwrap().to_string();
    let revoke = format!("/api/v1/permission-rules/{rule}/revoke");
    problem(
        &api.post(&revoke, &[], json!({ "reason": "" })),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );
    let r = api.post(&revoke, &[], json!({ "reason": "too broad" }));
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(r.body["revoked_at"].is_string());
    problem(
        &api.post(&revoke, &[], json!({ "reason": "again" })),
        StatusCode::CONFLICT,
        "invalid_state",
    );
    assert!(
        api.get(&format!("/api/v1/projects/{project}/permission-rules"))
            .body["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    problem(
        &api.get(&format!(
            "/api/v1/projects/{}/permission-rules",
            uuid::Uuid::nil()
        )),
        StatusCode::NOT_FOUND,
        "not_found",
    );

    let mut deny = Api::new(SimConfig {
        agent: ask_script(),
        ..SimConfig::default()
    });
    deny.sim.approve();
    assert!(
        deny.sim
            .run_until(STEPS, |s| !s.task(1).permissions.is_empty())
    );
    let id = deny.sim.task(1).permissions[0].id.to_string();
    let r = deny.post(
        &format!("/api/v1/permission-requests/{id}/answer"),
        &[],
        json!({ "decision": "deny", "remember": true }),
    );
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    assert_eq!(
        r.body["params"]["errors"],
        json!([{ "field": "remember", "code": "allow_only" }])
    );
}

#[test]
fn accounts_usage_records_and_budgets_are_readable() {
    let mut api = Api::new(SimConfig {
        agent: hang_script(),
        ..SimConfig::default()
    });
    let (item, sid) = running_session(&mut api);
    let work = api.get(&format!("/api/v1/work-items/{item}"));
    let budget = &work.body["budget"];
    assert_eq!(budget["warn_percent"], 80);
    assert_eq!(budget["turns"], json!({ "used": 1, "limit": 10 }));
    assert_eq!(budget["running_minutes"]["limit"], 60);
    assert_eq!(budget["cost_usd"]["limit"], "0.000000");

    let now = api.sim.engine().now().0;
    api.sim.world.set_script(AgentScript {
        rate_limit: Some(json!({ "unifiedWindows": {
            "seven_day": { "utilization": 0.95, "status": "allowed_warning", "resets_at": now + 7_200_000 },
        }})),
        ..AgentScript::default()
    });
    api.post(&format!("/api/v1/sessions/{sid}/interrupt"), &[], json!({}));
    assert!(api.sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == saka_core::vocab::SessionState::Idle)
    }));
    api.post(
        &format!("/api/v1/sessions/{sid}/steer"),
        &[],
        json!({ "text": "commit now" }),
    );
    assert!(
        api.sim
            .run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Done)
    );

    let accounts = api.get("/api/v1/agent-accounts");
    assert_eq!(accounts.status, StatusCode::OK);
    let a = &accounts.body["items"][0];
    assert_eq!(
        (&a["cli"], &a["name"], &a["paused"]),
        (&json!("claude"), &json!("default"), &json!(true))
    );
    assert_eq!(a["windows"][0]["window"], "seven_day");
    assert_eq!(a["windows"][0]["utilization"], 0.95);

    let usage = api.get(&format!("/api/v1/usage-records?work_item_id={item}"));
    let records = usage.body["items"].as_array().unwrap();
    assert_eq!(records.len(), 2, "{}", usage.body);
    assert_eq!(records[0]["account"], "claude:default");
    assert_eq!(records[1]["cost_usd"], "0.001000");
    let first = api.get(&format!("/api/v1/usage-records?session_id={sid}&limit=1"));
    let cursor = first.body["next_cursor"].as_str().unwrap().to_string();
    let second = api.get(&format!(
        "/api/v1/usage-records?session_id={sid}&limit=1&cursor={cursor}"
    ));
    assert_eq!(second.body["items"][0]["turn"], 2);
    problem(
        &api.get("/api/v1/usage-records"),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    );

    let s2 = api.sim.task(2).item.id.to_string();
    let waiting = api.get(&format!("/api/v1/work-items/{s2}"));
    assert_eq!(
        waiting.body["waiting_on"][0]["kind"], "quota",
        "{}",
        waiting.body
    );
    assert_eq!(waiting.body["waiting_on"][0]["window"], "seven_day");
    assert_eq!(
        api.get(&format!("/api/v1/work-items/{item}")).body["budget"],
        Value::Null
    );
}

#[test]
fn new_timeline_rows_are_announced_on_the_event_stream() {
    let mut api = Api::new(SimConfig {
        agent: hang_script(),
        ..SimConfig::default()
    });
    let (_, sid) = running_session(&mut api);
    let session = saka_core::ids::SessionId(sid.parse().unwrap());
    let engine = api.sim.engine();
    let offset = api.sim.world.session_log(session).len() as u64;
    let text = api.sim.block_on(async {
        let req = Request::get("/api/v1/events")
            .header("host", "127.0.0.1")
            .body(Body::empty())
            .unwrap();
        let r = api.app.clone().oneshot(req).await.unwrap();
        let mut body = r.into_body();
        engine
            .on_runner_event(saka_engine::RunnerEvent {
                session_id: session,
                offset,
                at: engine.now(),
                kind: saka_engine::RunnerEventKind::Activity {
                    category: "text".into(),
                    summary: "working on it".into(),
                    tool: None,
                    is_error: None,
                },
            })
            .unwrap();
        let mut text = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !text.contains("runtime.session.activity.v1") {
            let frame = tokio::time::timeout_at(deadline, body.frame()).await;
            let Ok(Some(Ok(frame))) = frame else { break };
            if let Some(data) = frame.data_ref() {
                text.push_str(std::str::from_utf8(data).unwrap());
            }
        }
        while !text.ends_with("\n\n") {
            let frame = tokio::time::timeout_at(deadline, body.frame()).await;
            let Ok(Some(Ok(frame))) = frame else { break };
            if let Some(data) = frame.data_ref() {
                text.push_str(std::str::from_utf8(data).unwrap());
            }
        }
        text
    });
    let block = text
        .split("\n\n")
        .find(|b| b.contains("event: runtime.session.activity.v1"))
        .unwrap();
    assert!(
        !block.contains("\nid: ") && !block.starts_with("id: "),
        "{block}"
    );
    let data: Value = serde_json::from_str(
        block
            .lines()
            .find_map(|l| l.strip_prefix("data: "))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        data,
        json!({
            "session_id": sid,
            "owner": {
                "kind": "stage_run",
                "work_item_id": api.sim.task(1).item.id,
                "stage_run_id": api.sim.task(1).session.unwrap().owner.stage_run_id(),
            },
            "offset": offset,
            "kind": "message",
        })
    );
}

#[derive(Clone, Default)]
struct LogBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn keyed(api: &Api, method: Method, path: &str, key: &str, body: Option<&Value>) -> Reply {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:7420")
        .header("content-type", "application/json")
        .header("idempotency-key", key);
    let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
    api.send(req.body(body).unwrap())
}

fn database_bytes(api: &Api) -> Vec<u8> {
    let db = api.sim.engine().store().path().to_path_buf();
    let mut bytes = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
        let mut path = db.clone().into_os_string();
        path.push(suffix);
        bytes.extend(std::fs::read(path).unwrap_or_default());
    }
    bytes
}

#[test]
fn secrets_are_write_only_and_their_values_never_leave_the_request() {
    const VALUE: &str = "npm-token-0123456789abcdef";
    let mut api = Api::new(SimConfig::default());
    let project = api.sim.project_id.0;
    let path = format!("/api/v1/projects/{project}/secrets/NPM_TOKEN");
    let logs = LogBuffer::default();
    let out = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || out.clone())
        .finish();
    let before = api.last_seq();
    tracing::subscriber::with_default(subscriber, || {
        let key = api.key();
        let set = keyed(
            &api,
            Method::PUT,
            &path,
            &key,
            Some(&json!({ "value": VALUE })),
        );
        assert_eq!(set.status, StatusCode::NO_CONTENT, "{:?}", set.body);
        assert_eq!(set.body, Value::Null);
        let replay = keyed(
            &api,
            Method::PUT,
            &path,
            &key,
            Some(&json!({ "value": VALUE })),
        );
        assert_eq!(replay.status, StatusCode::NO_CONTENT);
        assert_eq!(replay.headers["idempotent-replayed"], "true");
        let other = keyed(
            &api,
            Method::PUT,
            &path,
            &key,
            Some(&json!({ "value": "another-value-12345" })),
        );
        problem(
            &other,
            StatusCode::UNPROCESSABLE_ENTITY,
            "idempotency_key_reused",
        );
        let key = api.key();
        let rotated = keyed(
            &api,
            Method::PUT,
            &path,
            &key,
            Some(&json!({ "value": "rotated-value-12345" })),
        );
        assert_eq!(rotated.status, StatusCode::NO_CONTENT);
        let key = api.key();
        let second = format!("/api/v1/projects/{project}/secrets/API_KEY");
        let set = keyed(
            &api,
            Method::PUT,
            &second,
            &key,
            Some(&json!({ "value": VALUE })),
        );
        assert_eq!(set.status, StatusCode::NO_CONTENT);

        let list = api.get(&format!("/api/v1/projects/{project}/secrets"));
        assert_eq!(list.status, StatusCode::OK);
        let names: Vec<&str> = list.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["API_KEY", "NPM_TOKEN"]);
        assert!(list.body["items"][0]["updated_at"].is_string());
        assert!(!list.body.to_string().contains("value"));

        let key = api.key();
        let gone = keyed(&api, Method::DELETE, &second, &key, None);
        assert_eq!(gone.status, StatusCode::NO_CONTENT);
        let key = api.key();
        let again = keyed(&api, Method::DELETE, &second, &key, None);
        assert_eq!(again.status, StatusCode::NO_CONTENT);
    });

    let events = api.sim.engine().events_after(before, 100).unwrap();
    let kinds: Vec<(&str, &str)> = events
        .iter()
        .map(|e| (e.kind.as_str(), e.payload["name"].as_str().unwrap_or("")))
        .collect();
    assert_eq!(
        kinds,
        [
            ("workspace.secret.set", "NPM_TOKEN"),
            ("workspace.secret.set", "NPM_TOKEN"),
            ("workspace.secret.set", "API_KEY"),
            ("workspace.secret.removed", "API_KEY"),
        ],
        "a second delete records no event"
    );
    for e in &events {
        assert_eq!(e.payload.as_object().unwrap().len(), 2, "{:?}", e.payload);
    }
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    let db = database_bytes(&api);
    for value in [VALUE, "another-value-12345", "rotated-value-12345"] {
        assert!(!text.contains(value), "a value reached the logs");
        assert!(
            !db.windows(value.len()).any(|w| w == value.as_bytes()),
            "a value is stored in plain text"
        );
    }
    assert!(text.contains("secret set"), "{text}");
}

#[test]
fn secret_refusals_carry_their_codes() {
    let mut api = Api::new(SimConfig::default());
    let project = api.sim.project_id.0;
    let put = |api: &mut Api, project: String, name: &str, body: Value| {
        let key = api.key();
        keyed(
            api,
            Method::PUT,
            &format!("/api/v1/projects/{project}/secrets/{name}"),
            &key,
            Some(&body),
        )
    };
    let ok = json!({ "value": "long-enough-value" });
    let r = put(
        &mut api,
        project.to_string(),
        "NPM_TOKEN",
        json!({ "value": "short" }),
    );
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "secret_too_short");
    for name in ["lower", "GIT_TOKEN", "PATH", "SAKA_X", "1ABC"] {
        let r = put(&mut api, project.to_string(), name, ok.clone());
        problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "secret_name_invalid");
    }
    let r = put(
        &mut api,
        project.to_string(),
        "NPM_TOKEN",
        json!({ "value": "x".repeat(65_537) }),
    );
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    let r = put(
        &mut api,
        project.to_string(),
        "NPM_TOKEN",
        json!({ "value": "has\u{0}nul-byte" }),
    );
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    let r = put(
        &mut api,
        project.to_string(),
        "NPM_TOKEN",
        json!({ "value": "long-enough-value", "to": ["session"] }),
    );
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    let missing = uuid::Uuid::now_v7().to_string();
    let r = put(&mut api, missing.clone(), "NPM_TOKEN", ok.clone());
    problem(&r, StatusCode::NOT_FOUND, "not_found");
    problem(
        &api.get(&format!("/api/v1/projects/{missing}/secrets")),
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let no_key = api.send(
        Request::builder()
            .method(Method::PUT)
            .uri(format!("/api/v1/projects/{project}/secrets/NPM_TOKEN"))
            .header("host", "127.0.0.1:7420")
            .header("content-type", "application/json")
            .body(Body::from(ok.to_string()))
            .unwrap(),
    );
    assert_eq!(no_key.status.as_u16() / 100, 4);
    for i in 0..200 {
        let r = put(&mut api, project.to_string(), &format!("S{i}"), ok.clone());
        assert_eq!(r.status, StatusCode::NO_CONTENT);
    }
    let r = put(&mut api, project.to_string(), "S0", ok.clone());
    assert_eq!(r.status, StatusCode::NO_CONTENT, "replacing one is fine");
    let r = put(&mut api, project.to_string(), "ONE_MORE", ok);
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    assert_eq!(r.body["params"]["errors"][0]["code"], "too_many_secrets");
}

#[test]
fn a_spec_shows_its_pinned_context_and_context_issues_block_approve() {
    let mut api = Api::new(SimConfig::default());
    api.sim
        .world
        .put_doc("docs/architecture.md", b"# Architecture\n");
    api.sim
        .world
        .put_doc("docs/glossary.md", b"---\ncontext: always\n---\n# Terms\n");
    let text = one_step_spec("Linked", "").replace(
        "# Linked\n",
        "# Linked\n\nRead [the architecture](../architecture.md).\n",
    );
    let r = api.register_doc("docs/specs/linked.md", &text);
    assert_eq!(r.body["lint"]["ok"], true, "{}", r.body);
    let context = &r.body["revision"]["context"];
    assert_eq!(context.as_array().unwrap().len(), 2, "{context}");
    assert_eq!(context[0]["path"], "docs/architecture.md");
    assert_eq!(context[0]["origin"], "link");
    assert_eq!(context[0]["bytes"], 15);
    assert!(
        context[0]["content_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(context[1]["path"], "docs/glossary.md");
    assert_eq!(context[1]["origin"], "always");
    assert_eq!(context[1]["line"], 1);
    assert_eq!(
        r.body["revision"]["context_hash"].as_str().unwrap().len(),
        71
    );

    let broken = one_step_spec("Broken", "")
        .replace("# Broken\n", "# Broken\n\nRead [the plan](../plan.md).\n");
    let r = api.register_doc("docs/specs/broken.md", &broken);
    assert_eq!(r.body["lint"]["ok"], false);
    let issue = r.body["lint"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["code"] == "reference_missing")
        .cloned()
        .unwrap();
    assert_eq!(issue["severity"], "error");
    assert_eq!(issue["params"], json!({ "path": "docs/plan.md" }));
    let reply = api.approve_reply(&r);
    problem(&reply, StatusCode::UNPROCESSABLE_ENTITY, "spec_not_ready");
    assert!(
        reply.body["params"]["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "reference_missing")
    );
}

#[test]
fn a_task_shows_the_exact_work_packet_once_its_environment_is_ready() {
    let mut api = Api::new(SimConfig::default());
    let (spec, revision) = (api.sim.spec_id, api.sim.revision_id);
    api.sim.approve_spec(spec, revision).unwrap();
    let task = api.sim.task(1);
    let path = format!("/api/v1/work-items/{}/work-packet", task.item.id.0);

    let early = api.get(&path);
    problem(&early, StatusCode::CONFLICT, "work_packet_unavailable");
    assert_eq!(
        early.body["params"],
        json!({ "reason": "environment_not_ready" })
    );

    assert!(api.sim.run_until(8_000, |s| !s.world.prompts().is_empty()));
    let r = api.get(&path);
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let first = api.sim.world.prompts()[0].clone();
    let b = &r.body;
    assert_eq!(b["text"], first.as_str());
    assert_eq!(b["bytes"], first.len());
    assert_eq!(b["work_item_id"], task.item.id.0.to_string());
    assert_eq!(b["revision_id"], revision.0.to_string());
    assert_eq!(b["source_commit"].as_str().unwrap().len(), 40);
    assert!(b["environment_id"].as_str().is_some());
    assert!(b["sha256"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(
        b["documents"],
        json!([{
            "index": 1, "kind": "spec", "path": TWO_STEP_PATH, "origin": null,
            "bytes": TWO_STEP_SPEC.len(), "content_hash": b["documents"][0]["content_hash"]
        }])
    );

    let missing = api.get(&format!(
        "/api/v1/work-items/{}/work-packet",
        uuid::Uuid::now_v7()
    ));
    problem(&missing, StatusCode::NOT_FOUND, "not_found");
}

#[test]
fn a_fourth_manual_retry_in_a_stage_is_refused_and_cancel_still_works() {
    let mut cfg = SimConfig {
        ci: CiScript::Fail(vec!["test".into()]),
        ..SimConfig::default()
    };
    cfg.operator.retry = vec![];
    let mut api = Api::new(cfg);
    let (spec, revision) = (api.sim.spec_id, api.sim.revision_id);
    api.sim.approve_spec(spec, revision).unwrap();
    let id = api.sim.task(1).item.id.0;
    let retry = format!("/api/v1/work-items/{id}/actions/retry");
    let blocked = |s: &Sim| s.task(1).item.state == WorkItemState::Blocked;
    for n in 1..=3 {
        assert!(api.sim.run_until(STEPS, blocked), "block {n}");
        let r = api.post(&retry, &[], json!({ "reason": format!("try {n}") }));
        assert_eq!(r.status, StatusCode::ACCEPTED, "retry {n}: {}", r.body);
    }
    assert!(api.sim.run_until(STEPS, blocked));
    let before = api.last_seq();
    let detail = api.get(&format!("/api/v1/work-items/{id}"));
    assert_eq!(detail.body["available_actions"], json!(["cancel"]));
    let r = api.post(&retry, &[], json!({ "reason": "once more" }));
    problem(&r, StatusCode::CONFLICT, "retry_limit_reached");
    assert_eq!(r.body["params"], json!({ "stage": "dev", "limit": 3 }));
    assert_eq!(api.last_seq(), before);
    let cancel = api.post(
        &format!("/api/v1/work-items/{id}/actions/cancel"),
        &[],
        json!({ "reason": "giving up" }),
    );
    assert_eq!(cancel.status, StatusCode::ACCEPTED, "{}", cancel.body);
}

#[test]
fn a_timeline_is_read_from_its_end_and_backwards() {
    let mut api = Api::new(SimConfig {
        agent: hang_script(),
        ..SimConfig::default()
    });
    let (_, sid) = running_session(&mut api);
    let session = saka_core::ids::SessionId(sid.parse().unwrap());
    let engine = api.sim.engine();
    let base = api.sim.world.session_log(session).len() as u64;
    for i in 0..450 {
        engine
            .on_runner_event(saka_engine::RunnerEvent {
                session_id: session,
                offset: base + i,
                at: engine.now(),
                kind: saka_engine::RunnerEventKind::Activity {
                    category: "text".into(),
                    summary: format!("row {i}"),
                    tool: None,
                    is_error: None,
                },
            })
            .unwrap();
    }
    let url = format!("/api/v1/sessions/{sid}/timeline");
    let offsets = |body: &Value| -> Vec<u64> {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["offset"].as_u64().unwrap())
            .collect()
    };
    let all = offsets(&api.get(&format!("{url}?limit=500")).body);
    assert!(all.len() >= 450, "{}", all.len());
    let n = all.len();

    let tail = api.get(&format!("{url}?tail=true&limit=200"));
    assert_eq!(tail.status, StatusCode::OK);
    assert_eq!(offsets(&tail.body), all[n - 200..]);
    assert_eq!(tail.body["prev_before"], json!(all[n - 200]));
    assert_eq!(tail.body["next_after"], Value::Null);

    let before = api.get(&format!("{url}?before={}&limit=200", all[n - 200]));
    assert_eq!(offsets(&before.body), all[n - 400..n - 200]);
    assert_eq!(before.body["prev_before"], json!(all[n - 400]));
    assert_eq!(before.body["next_after"], json!(all[n - 201]));

    let after = api.get(&format!("{url}?after={}&limit=10", all[n - 400]));
    assert_eq!(offsets(&after.body), all[n - 399..n - 389]);
    assert_eq!(after.body["prev_before"], json!(all[n - 399]));
    assert_eq!(after.body["next_after"], json!(all[n - 390]));

    let first = api.get(&format!("{url}?limit=5"));
    assert_eq!(offsets(&first.body), all[..5]);
    assert_eq!(first.body["prev_before"], Value::Null);

    let empty = api.get(&format!("{url}?before={}", all[0]));
    assert_eq!(
        (
            &empty.body["items"],
            &empty.body["prev_before"],
            &empty.body["next_after"]
        ),
        (&json!([]), &Value::Null, &Value::Null)
    );

    let both = api.get(&format!("{url}?after=1&tail=true"));
    problem(&both, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    assert_eq!(
        both.body["params"],
        json!({ "errors": [{ "field": "before", "code": "exclusive" }] })
    );
    let bad = api.get(&format!("{url}?tail=yes"));
    problem(&bad, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    assert_eq!(
        bad.body["params"],
        json!({ "errors": [{ "field": "tail", "code": "invalid" }] })
    );
}

fn docs_only() -> Api {
    let layout = Layout {
        saka_toml: saka_toml("docs", &[repo_entry("docs", DOCS_REPO, "")]),
        ..Layout::default()
    };
    Api::with(Sim::multi_repo(SimConfig::default(), &layout))
}

fn register_at(api: &mut Api, folder: &str, toml: &str) -> Reply {
    api.sim
        .world
        .put_doc(&format!("{folder}/saka.toml"), toml.as_bytes());
    api.post(
        "/api/v1/projects",
        &[],
        json!({ "workspace_url": format!("https://github.com/{DOCS_REPO}"), "project_path": folder }),
    )
}

fn project_count(api: &Api) -> usize {
    api.get("/api/v1/projects").body["items"]
        .as_array()
        .unwrap()
        .len()
}

#[test]
fn a_separate_docs_repo_registers_its_code_repos_as_bindings() {
    let mut api = docs_only();
    api.sim.world.create_branch(WEB_REPO, "develop");
    let toml = saka_toml(
        "shop",
        &[
            repo_entry(
                "shop",
                SHOP_REPO,
                "default = true\npath_scope = [\"services/api/**\", \"pnpm-lock.yaml\"]\n",
            ),
            repo_entry("web", WEB_REPO, "base_branch = \"develop\"\n"),
        ],
    );
    let r = register_at(&mut api, "shop", &toml);
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let id = r.body["id"].as_str().unwrap().to_string();
    let get = api.get(&format!("/api/v1/projects/{id}"));
    assert_eq!(get.body, r.body);
    let repos = &r.body["repos"];
    let pick = |i: usize| {
        let b = &repos[i];
        json!([
            b["name"],
            b["full_name"],
            b["base_branch"],
            b["is_default"],
            b["is_workspace"],
            b["path_scope"],
            b["effective_path_scope"],
        ])
    };
    assert_eq!(repos.as_array().unwrap().len(), 2);
    assert_eq!(
        pick(0),
        json!([
            "shop",
            SHOP_REPO,
            "main",
            true,
            false,
            ["services/api/**", "pnpm-lock.yaml"],
            ["services/api/**", "pnpm-lock.yaml"],
        ])
    );
    assert_eq!(
        pick(1),
        json!(["web", WEB_REPO, "develop", false, false, ["**"], ["**"]])
    );
    assert_eq!(r.body["workspace"]["full_name"], DOCS_REPO);
    let sha = r.body["config_sha256"].as_str().unwrap();
    assert!(sha.starts_with("sha256:") && sha.len() == 71, "{sha}");
    assert_eq!(r.body["config_error"], Value::Null);
    assert_eq!(r.body["warnings"], json!([]));

    let projects = project_count(&api);
    let unknown_host = saka_toml(
        "a",
        &[
            repo_entry("shop", SHOP_REPO, "default = true\n"),
            "[[repo]]\nname = \"web\"\nurl = \"https://gitlab.example/acme/web\"\n".into(),
        ],
    );
    let r = register_at(&mut api, "a", &unknown_host);
    problem(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "scm_integration_unknown",
    );
    assert_eq!(
        r.body["params"],
        json!({ "host": "gitlab.example", "repo": "web" })
    );

    api.sim.world.make_inaccessible("acme/private");
    let private = saka_toml(
        "b",
        &[
            repo_entry("shop", SHOP_REPO, "default = true\n"),
            repo_entry("private", "acme/private", ""),
        ],
    );
    let r = register_at(&mut api, "b", &private);
    problem(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "scm_repo_inaccessible",
    );
    assert_eq!(r.body["params"], json!({ "repo": "private" }));

    let missing_branch = saka_toml(
        "c",
        &[
            repo_entry("shop", SHOP_REPO, "default = true\n"),
            repo_entry("web", WEB_REPO, "base_branch = \"release\"\n"),
        ],
    );
    let r = register_at(&mut api, "c", &missing_branch);
    problem(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "project_config_invalid",
    );
    assert_eq!(
        r.body["params"]["issues"],
        json!([{ "code": "base_branch_not_found", "field": "repo[1].base_branch", "line": 12 }])
    );
    assert_eq!(project_count(&api), projects, "a refusal registers nothing");

    let openapi = include_str!("../openapi.json");
    let en = include_str!("../../../web/src/i18n/en.json");
    let id_catalog = include_str!("../../../web/src/i18n/id.json");
    for text in [openapi, en, id_catalog] {
        assert!(!text.contains("multi_repo_unsupported"));
    }
}

#[test]
fn projects_bound_to_one_repo_warn_when_their_scopes_overlap() {
    let mut api = docs_only();
    let scoped = |name: &str, scope: &str| {
        saka_toml(
            name,
            &[repo_entry(
                "shop",
                SHOP_REPO,
                &format!("path_scope = [\"{scope}\"]\n"),
            )],
        )
    };
    let api_id = register_at(&mut api, "api", &scoped("api", "services/api/**")).body["id"].clone();
    let all_id = register_at(&mut api, "all", &scoped("all", "services/**")).body["id"].clone();
    let web_id = register_at(&mut api, "web", &scoped("web", "services/web/**")).body["id"].clone();
    let warnings = |id: &Value| {
        let r = api.get(&format!("/api/v1/projects/{}", id.as_str().unwrap()));
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        r.body["warnings"].clone()
    };
    let overlap = |other_id: &Value, other: &str| {
        json!({
            "code": "path_scope_overlap",
            "params": {
                "repo": "shop",
                "repo_id": warnings_repo(&api),
                "other_project_id": other_id,
                "other_project": other,
                "other_repo": "shop",
            },
        })
    };
    assert_eq!(warnings(&api_id), json!([overlap(&all_id, "all")]));
    assert_eq!(
        warnings(&all_id),
        json!([overlap(&api_id, "api"), overlap(&web_id, "web")])
    );
    assert_eq!(warnings(&web_id), json!([overlap(&all_id, "all")]));
}

fn warnings_repo(api: &Api) -> Value {
    let items = api.get("/api/v1/projects").body["items"].clone();
    items
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| p["repos"].as_array().unwrap().clone())
        .find(|r| r["full_name"] == SHOP_REPO)
        .map(|r| r["repo_id"].clone())
        .unwrap()
}

#[test]
fn reload_reports_its_outcome_refusals_and_replays_by_key() {
    let layout = Layout {
        saka_toml: saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]),
        ..Layout::default()
    };
    let mut api = Api::with(Sim::multi_repo(SimConfig::default(), &layout));
    let id = api.sim.project_id.0;
    let path = format!("/api/v1/projects/{id}/actions/reload");

    let r = api.post_with(&path, None, &[], &json!({}));
    problem(&r, StatusCode::BAD_REQUEST, "idempotency_key_required");
    let r = api.post(&path, &[], json!({ "force": true }));
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");
    let r = api.post(&path, &[], json!({}));
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["outcome"], "unchanged");
    assert_eq!(r.body["project"]["id"], id.to_string());
    let r = api.post(
        &format!("/api/v1/projects/{}/actions/reload", uuid::Uuid::nil()),
        &[],
        json!({}),
    );
    problem(&r, StatusCode::NOT_FOUND, "not_found");

    let two = saka_toml(
        "shop",
        &[
            repo_entry("shop", SHOP_REPO, "default = true\n"),
            repo_entry("web", WEB_REPO, ""),
        ],
    );
    api.sim.world.put_doc("docs/saka.toml", two.as_bytes());
    let r = api.post_with(&path, Some("reload-1"), &[], &json!({}));
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["outcome"], "changed");
    assert_eq!(r.body["project"]["repos"][1]["name"], "web");
    let replay = api.post_with(&path, Some("reload-1"), &[], &json!({}));
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(replay.body, r.body);
    let metrics = api.get_text("/metrics");
    assert!(metrics.contains("saka_project_config_reloads_total{outcome=\"changed\"} 1"));

    api.sim
        .world
        .put_doc("docs/saka.toml", b"[project]\nname = \"shop\"\n");
    let r = api.post(&path, &[], json!({}));
    problem(
        &r,
        StatusCode::UNPROCESSABLE_ENTITY,
        "project_config_invalid",
    );
    let error = &api.get(&format!("/api/v1/projects/{id}")).body["config_error"];
    assert_eq!(error["code"], "project_config_invalid");
    assert_eq!(error["commit"], api.sim.world.base_head());
    assert!(error["observed_at"].is_string());

    api.sim.world.put_doc("docs/saka.toml", two.as_bytes());
    api.sim.approve();
    let items = api.get(&format!("/api/v1/work-items?project_id={id}")).body["items"].clone();
    assert!(
        items
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["repo_name"] == "shop"),
        "{items}"
    );
    let only_web = saka_toml("shop", &[repo_entry("web", WEB_REPO, "")]);
    api.sim.world.put_doc("docs/saka.toml", only_web.as_bytes());
    let r = api.post(&path, &[], json!({}));
    problem(&r, StatusCode::CONFLICT, "project_repo_in_use");
    let repos = r.body["params"]["repos"].as_array().unwrap();
    assert_eq!(repos[0]["name"], "shop");
    assert_eq!(repos[0]["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(
        api.get(&format!("/api/v1/projects/{id}")).body["config_error"]["code"],
        "project_repo_in_use"
    );
}

const REPO_COLUMN_SPEC: &str = "---
saka: spec
---
# Multi

## Acceptance criteria

- **AC-01**: every part works.

## Steps

| Step | Work | AC | Repo | Done when |
|------|------|----|------|-----------|
| S1 | Part 1 | AC-01 |  | `make test` |
| S2 | Part 2 | AC-01 | mobile | `make test` |
";

#[test]
fn approve_refuses_a_repo_the_project_does_not_bind_until_a_reload_binds_it() {
    let layout = Layout {
        saka_toml: saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]),
        ..Layout::default()
    };
    let mut api = Api::with(Sim::multi_repo(SimConfig::default(), &layout));
    let project = api.sim.project_id.0;
    let repo_named = |api: &Api, name: &str| {
        api.get(&format!("/api/v1/projects/{project}")).body["repos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .map(|r| r["repo_id"].clone())
            .unwrap()
    };
    let shop = repo_named(&api, "shop");

    let spec = api.register_doc("docs/specs/multi.md", REPO_COLUMN_SPEC);
    let id = spec.body["id"].as_str().unwrap().to_string();
    let steps = |api: &Api| {
        api.get(&format!("/api/v1/specs/{id}")).body["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["repo"].clone(), s["repo_id"].clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        steps(&api),
        [(Value::Null, shop.clone()), (json!("mobile"), Value::Null)]
    );

    let before = api.last_seq();
    let refused = api.approve_reply(&spec);
    problem(&refused, StatusCode::UNPROCESSABLE_ENTITY, "spec_not_ready");
    assert_eq!(
        refused.body["params"]["issues"],
        json!([{"code": "step_repo_unknown", "severity": "error", "line": 15, "params": {"step": "S2", "repo": "mobile"}}])
    );
    assert_eq!(api.last_seq(), before);

    let bound = saka_toml(
        "shop",
        &[
            repo_entry("shop", SHOP_REPO, "default = true\n"),
            repo_entry("mobile", WEB_REPO, ""),
        ],
    );
    api.sim.world.put_doc("docs/saka.toml", bound.as_bytes());
    let r = api.post(
        &format!("/api/v1/projects/{project}/actions/reload"),
        &[],
        json!({}),
    );
    assert_eq!(r.body["outcome"], "changed", "{}", r.body);
    let mobile = repo_named(&api, "mobile");
    assert_eq!(
        steps(&api),
        [
            (Value::Null, shop.clone()),
            (json!("mobile"), mobile.clone())
        ]
    );

    let approved = api.approve_reply(&spec);
    assert_eq!(approved.status, StatusCode::OK, "{}", approved.body);
    assert_eq!(approved.body["revision"]["id"], spec.body["revision"]["id"]);
    let created = api.sim.events_of("delivery.work_item.created");
    let placed: Vec<_> = created
        .iter()
        .filter(|e| {
            e.payload["task_key"]
                .as_str()
                .unwrap()
                .starts_with("multi/")
        })
        .map(|e| e.payload["repo_id"].clone())
        .collect();
    assert_eq!(placed, [shop.clone(), mobile.clone()]);
    assert_eq!(
        steps(&api),
        [(Value::Null, shop), (json!("mobile"), mobile)]
    );
}

#[test]
fn metrics_count_path_scope_violations_by_stage_and_the_blocker_names_them() {
    let layout = Layout {
        saka_toml: saka_toml(
            "shop",
            &[repo_entry(
                "shop",
                SHOP_REPO,
                "path_scope = [\"services/api/**\"]\n",
            )],
        ),
        ..Layout::default()
    };
    let cfg = SimConfig {
        max_active_tasks: 1,
        ..SimConfig::default()
    };
    let mut api = Api::with(Sim::multi_repo(cfg, &layout));
    let metrics = api.get_text("/metrics");
    assert!(metrics.contains("# TYPE saka_project_config_reloads_total counter"));
    assert!(metrics.contains("# TYPE saka_path_scope_violations_total counter"));

    api.sim.world.script_edits(vec![vec![(
        "services/web/a.ts".into(),
        Some("export {}\n".into()),
    )]]);
    api.sim.approve();
    assert!(
        api.sim
            .run_until(5_000, |s| s.task(1).item.state == WorkItemState::Blocked)
    );
    let id = api.sim.task(1).item.id;
    let detail = api.get(&format!("/api/v1/work-items/{id}"));
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.body);
    let blocker = &detail.body["blocker"];
    assert_eq!(blocker["code"], "path_scope_violation");
    assert_eq!(blocker["params"]["stage"], "dev");
    assert_eq!(blocker["params"]["paths"], json!(["services/web/a.ts"]));
    assert_eq!(blocker["params"]["scope"], json!(["services/api/**"]));
    let metrics = api.get_text("/metrics");
    assert!(
        metrics.contains("saka_path_scope_violations_total{stage=\"dev\"} 2"),
        "{metrics}"
    );
}

const LINK: &str = "/api/v1/scm/github/user-link";
const AUTHORIZE: &str = "/api/v1/scm/github/user-link/authorizations";

fn table_text(api: &Api, table: &str) -> String {
    let columns = rows(
        api,
        &format!("SELECT name FROM pragma_table_info('{table}')"),
    );
    let all = columns
        .iter()
        .map(|c| format!("COALESCE(CAST({c} AS TEXT), '')"))
        .collect::<Vec<_>>()
        .join(" || '|' || ");
    rows(api, &format!("SELECT {all} FROM {table}")).join("\n")
}

#[test]
fn linking_an_account_never_shows_a_token_in_events_jobs_actions_logs_or_responses() {
    let mut api = Api::new(SimConfig::default());
    let logs = LogBuffer::default();
    let out = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || out.clone())
        .finish();
    let mut bodies = Vec::new();
    tracing::subscriber::with_default(subscriber, || {
        let key = api.key();
        let started = api.post_with(AUTHORIZE, Some(&key), &[], &json!({}));
        assert_eq!(started.status, StatusCode::ACCEPTED, "{}", started.body);
        let id = started.body["id"].as_str().unwrap().to_string();
        assert_eq!(
            started.headers["location"].to_str().unwrap(),
            format!("{AUTHORIZE}/{id}")
        );
        assert_eq!(started.body["state"], "pending");
        assert_eq!(started.body["integration_id"], "github");
        assert_eq!(started.body["user_code"], "FAKE-0001");
        assert_eq!(
            started.body["verification_uri"],
            "https://github.com/login/device"
        );
        assert_eq!(started.body["interval_secs"], 5);
        assert_eq!(started.body["failure"], Value::Null);
        assert_eq!(started.body["link"], Value::Null);
        let replay = api.post_with(AUTHORIZE, Some(&key), &[], &json!({}));
        assert_eq!(replay.status, StatusCode::ACCEPTED);
        assert_eq!(replay.headers["idempotent-replayed"], "true");
        let again = api.post(AUTHORIZE, &[], json!({}));
        assert_eq!(
            again.body["id"], started.body["id"],
            "the pending one is reused"
        );
        let missing = api.get(LINK);
        problem(&missing, StatusCode::NOT_FOUND, "user_link_not_found");
        bodies.extend([started.body, replay.body, again.body, missing.body]);

        api.sim
            .world
            .accounts()
            .answer(saka_testkit::DeviceAnswer::Approve);
        assert!(api.sim.run_until(STEPS, |s| {
            s.engine()
                .user_link("github", saka_engine::OPERATOR)
                .is_ok()
        }));
        let done = api.get(&format!("{AUTHORIZE}/{id}"));
        assert_eq!(done.status, StatusCode::OK);
        assert_eq!(done.body["state"], "completed");
        assert_eq!(done.body["link"]["login"], "octo");
        let link = api.get(LINK);
        assert_eq!(link.status, StatusCode::OK, "{}", link.body);
        assert_eq!(link.body["state"], "active");
        assert_eq!(link.body["relink_cause"], Value::Null);
        assert_eq!(link.body["provider_user_id"], 1);
        assert_eq!(link.body["name"], "Octo Cat");
        assert_eq!(link.body["email"], "1+octo@users.noreply.github.com");
        assert_eq!(link.body["refreshed_at"], Value::Null);
        assert!(
            link.body["access_expires_at"]
                .as_str()
                .unwrap()
                .ends_with('Z')
        );
        bodies.extend([done.body, link.body]);

        let engine = api.sim.engine();
        let current = engine.user_link("github", saka_engine::OPERATOR).unwrap();
        api.sim
            .clock
            .set(current.access_expires_at.plus_ms(-60_000));
        let used = api
            .sim
            .block_on(engine.refresh_user_link(current.id, saka_core::vocab::RefreshReason::Use));
        assert!(matches!(used, Ok(saka_engine::LinkCredential::Ready(_))));
        api.sim
            .clock
            .set(current.refresh_expires_at.plus_ms(-3 * 86_400_000));
        let refreshes = api.sim.world.accounts().calls_of("refresh").len();
        assert!(api.sim.run_until(STEPS, |s| {
            s.world.accounts().calls_of("refresh").len() > refreshes
        }));
        let refreshed = api.get(LINK);
        assert!(refreshed.body["refreshed_at"].is_string());
        bodies.push(refreshed.body);
        let metrics = api.get_text("/metrics");
        for line in [
            "saka_user_link_authorizations_total{integration=\"github\",state=\"pending\"} 1",
            "saka_user_link_authorizations_total{integration=\"github\",state=\"completed\"} 1",
            "saka_user_link_refresh_total{integration=\"github\",outcome=\"refreshed\"} 2",
        ] {
            assert!(metrics.contains(line), "{line}\n{metrics}");
        }

        let key = api.key();
        let gone = keyed(&api, Method::DELETE, LINK, &key, None);
        assert_eq!(gone.status, StatusCode::NO_CONTENT, "{}", gone.body);
        let key = api.key();
        let again = keyed(&api, Method::DELETE, LINK, &key, None);
        assert_eq!(again.status, StatusCode::NO_CONTENT);
        problem(&api.get(LINK), StatusCode::NOT_FOUND, "user_link_not_found");
    });
    assert_eq!(api.sim.events_of("scm.user_link.removed").len(), 1);

    let mut secrets: Vec<String> = api.sim.world.accounts().device_codes();
    secrets.extend(api.sim.world.accounts().issued_tokens());
    assert!(secrets.iter().any(|s| s.starts_with("ghu_")));
    assert!(secrets.iter().any(|s| s.starts_with("ghr_")));
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("scm.user_link.linked"), "{logs}");
    let mut haystacks = vec![
        ("logs", logs),
        ("responses", bodies.iter().map(Value::to_string).collect()),
    ];
    for table in ["event", "action_request", "job"] {
        haystacks.push((table, table_text(&api, table)));
    }
    for (name, text) in &haystacks {
        for marker in ["ghu_", "ghr_", "github_pat_"] {
            assert!(!text.contains(marker), "{marker} in {name}");
        }
        for secret in &secrets {
            assert!(!text.contains(secret.as_str()), "a secret in {name}");
        }
    }
}

#[test]
fn account_link_refusals_carry_their_codes() {
    let mut api = Api::new(SimConfig::default());
    for (method, path) in [
        (Method::POST, "/api/v1/scm/gitlab/user-link/authorizations"),
        (Method::DELETE, "/api/v1/scm/gitlab/user-link"),
    ] {
        let key = api.key();
        let r = keyed(&api, method, path, &key, Some(&json!({})));
        problem(&r, StatusCode::NOT_FOUND, "not_found");
    }
    problem(
        &api.get("/api/v1/scm/gitlab/user-link"),
        StatusCode::NOT_FOUND,
        "not_found",
    );
    problem(
        &api.get(&format!("{AUTHORIZE}/{}", uuid::Uuid::now_v7())),
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let r = api.post_with(AUTHORIZE, None, &[], &json!({}));
    problem(&r, StatusCode::BAD_REQUEST, "idempotency_key_required");
    let r = api.post(AUTHORIZE, &[], json!({ "scope": "repo" }));
    problem(&r, StatusCode::UNPROCESSABLE_ENTITY, "validation_failed");

    api.sim.world.accounts().set_device_flow(false);
    let r = api.post(AUTHORIZE, &[], json!({}));
    problem(&r, StatusCode::CONFLICT, "device_flow_unconfigured");
    assert_eq!(r.body["params"]["integration_id"], "github");

    api.sim.world.set_capabilities(saka_ports::ScmCapabilities {
        user_authorship: false,
        ..saka_testkit::FAKE_CAPABILITIES
    });
    let r = api.post(AUTHORIZE, &[], json!({}));
    problem(&r, StatusCode::CONFLICT, "user_authorship_unsupported");
    assert_eq!(r.body["params"]["integration_id"], "github");
    assert!(api.sim.world.accounts().calls_of("device_code").is_empty());
}
