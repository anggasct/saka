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
use saka_core::ids::SpecDraftId;
use saka_core::model::Draft;
use saka_core::vocab::{SessionState, SpecDraftState};
use saka_testkit::{AgentOutcome, AgentScript, DraftTurn, Sim, SimConfig};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const STEPS: usize = 4_000;
const PATH: &str = "docs/specs/export.md";
const IDEA: &str = "Let people export their task list as a CSV file.";

struct Api {
    sim: Sim,
    app: Router,
    project: String,
    next_key: u32,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Reply {
    fn code(&self) -> &str {
        self.body["code"].as_str().unwrap_or_default()
    }

    fn error(&self) -> (String, String) {
        let e = &self.body["params"]["errors"][0];
        (
            e["field"].as_str().unwrap_or_default().to_string(),
            e["code"].as_str().unwrap_or_default().to_string(),
        )
    }
}

fn good() -> Vec<u8> {
    b"---
saka: spec
---
# Export tasks

## Acceptance criteria

- **AC-01**: `tasks export` writes a CSV file.

## Done when

- `make test`
"
    .to_vec()
}

fn bad_crlf() -> Vec<u8> {
    b"---\r\nsaka: spec\r\n---\r\n# Export tasks\r\n\r\nNo criteria yet.\r\n".to_vec()
}

fn write(bytes: Vec<u8>) -> DraftTurn {
    DraftTurn::Write {
        path: PATH.into(),
        bytes,
    }
}

impl Api {
    fn new() -> Api {
        Api::with(SimConfig::default())
    }

    fn with(cfg: SimConfig) -> Api {
        let sim = Sim::new(cfg);
        let state = AppState::new(ApiConfig::new("0.1.0-test"));
        state.set_engine(sim.engine());
        let app = router(state);
        let project = sim.project_id.0.to_string();
        Api {
            sim,
            app,
            project,
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

    fn post_keyed(&self, path: &str, key: &str, body: &Value) -> Reply {
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

    fn post(&mut self, path: &str, body: &Value) -> Reply {
        self.next_key += 1;
        let key = format!("key-{}", self.next_key);
        self.post_keyed(path, &key, body)
    }

    fn drafts_url(&self) -> String {
        format!("/api/v1/projects/{}/spec-drafts", self.project)
    }

    fn create(&mut self, body: Value) -> Reply {
        let url = self.drafts_url();
        self.post(&url, &body)
    }

    fn draft_at(&mut self, path: &str) -> SpecDraftId {
        let created = self.create(json!({ "idea": IDEA, "path": path }));
        assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
        id_of(&created.body)
    }

    fn scripted(&mut self, turns: Vec<DraftTurn>) -> SpecDraftId {
        self.sim.world.script_draft(turns);
        self.draft_at(PATH)
    }

    fn run_to(&mut self, id: SpecDraftId, done: impl Fn(&Draft) -> bool) {
        assert!(
            self.sim.run_until(STEPS, |s| done(&s.draft(id))),
            "draft never reached the state: {:?}",
            self.sim.draft(id).draft
        );
    }

    fn run_to_state(&mut self, id: SpecDraftId, state: SpecDraftState) {
        self.run_to(id, |d| d.draft.state == state);
    }

    fn retry(&mut self, id: SpecDraftId, body: Value) -> Reply {
        self.post(
            &format!("/api/v1/spec-drafts/{}/actions/retry", id.0),
            &body,
        )
    }

    fn cancel(&mut self, id: SpecDraftId) -> Reply {
        self.post(
            &format!("/api/v1/spec-drafts/{}/actions/cancel", id.0),
            &json!({ "reason": "not needed" }),
        )
    }
}

fn id_of(body: &Value) -> SpecDraftId {
    SpecDraftId(body["id"].as_str().unwrap().parse().unwrap())
}

#[test]
fn a_valid_body_creates_a_queued_draft_and_a_replay_returns_the_same_response() {
    let mut api = Api::new();
    api.sim.world.put_doc("docs/prd.md", b"# PRD\n");
    let url = api.drafts_url();
    let body =
        json!({ "idea": IDEA, "path": PATH, "context_paths": ["docs/prd.md"], "locale": "id" });
    let created = api.post_keyed(&url, "create-1", &body);
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let b = &created.body;
    assert_eq!(b["state"], "queued");
    assert_eq!(b["path"], PATH);
    assert_eq!(b["slug"], "export");
    assert_eq!(b["idea"], IDEA);
    assert_eq!(b["locale"], "id");
    assert_eq!(b["context_paths"], json!(["docs/prd.md"]));
    assert_eq!(b["attempt"], Value::Null);
    assert_eq!(b["last_round"], Value::Null);
    assert_eq!(b["session"], Value::Null);
    assert_eq!(b["budget"], Value::Null);
    assert_eq!(b["failure"], Value::Null);
    assert_eq!(b["primary_action"], Value::Null);
    assert_eq!(b["available_actions"], json!(["cancel"]));
    assert_eq!(b["retries"], 0);
    let tag = format!("W/\"{}\"", b["version"]);
    assert_eq!(created.headers["etag"], tag.as_str());

    let replay = api.post_keyed(&url, "create-1", &body);
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.body, created.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");

    let id = id_of(&created.body);
    let detail = api.get(&format!("/api/v1/spec-drafts/{}", id.0));
    assert_eq!(detail.status, StatusCode::OK);
    assert_eq!(detail.headers["etag"], tag.as_str());
    assert_eq!(detail.body["idea"], IDEA);
    assert_eq!(detail.body["waiting_on"], json!([]));

    let missing = api.get(&format!("/api/v1/spec-drafts/{}", uuid::Uuid::now_v7()));
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let unknown = api.post(
        &format!("/api/v1/projects/{}/spec-drafts", uuid::Uuid::now_v7()),
        &json!({ "idea": IDEA, "path": PATH }),
    );
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    let keyless = api.send(
        Request::builder()
            .method(Method::POST)
            .uri(&url)
            .header("host", "127.0.0.1:7420")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    );
    assert_eq!(keyless.status, StatusCode::BAD_REQUEST);
    assert_eq!(keyless.code(), "idempotency_key_required");
}

#[test]
fn every_taken_target_is_a_conflict_with_its_reason() {
    let mut api = Api::new();
    let commit = api
        .sim
        .world
        .put_doc("docs/specs/existing.md", b"# Existing\n");
    let file = api.create(json!({ "idea": IDEA, "path": "docs/specs/existing.md" }));
    assert_eq!(file.status, StatusCode::CONFLICT, "{}", file.body);
    assert_eq!(file.code(), "spec_draft_target_taken");
    assert_eq!(file.body["params"]["reason"], "file_exists");
    assert_eq!(file.body["params"]["path"], "docs/specs/existing.md");
    assert_eq!(file.body["params"]["commit"], commit.as_str());
    assert_eq!(
        file.body["params"]["blob"],
        saka_testkit::git_blob(b"# Existing\n").as_str()
    );

    let spec = api.sim.spec();
    api.sim.world.remove_doc(&spec.path);
    let registered = api.create(json!({ "idea": IDEA, "path": spec.path }));
    assert_eq!(
        registered.status,
        StatusCode::CONFLICT,
        "{}",
        registered.body
    );
    assert_eq!(registered.body["params"]["reason"], "spec_registered");
    assert_eq!(registered.body["params"]["spec_id"], spec.id.0.to_string());

    let slug = api.create(json!({ "idea": IDEA, "path": format!("docs/later/{}.md", spec.slug) }));
    assert_eq!(slug.status, StatusCode::CONFLICT, "{}", slug.body);
    assert_eq!(slug.body["params"]["reason"], "slug_taken");
    assert_eq!(slug.body["params"]["spec_id"], spec.id.0.to_string());

    let first = api.draft_at(PATH);
    let again = api.create(json!({ "idea": "Another idea.", "path": PATH }));
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);
    assert_eq!(again.body["params"]["reason"], "draft_active");
    assert_eq!(again.body["params"]["spec_draft_id"], first.0.to_string());
}

#[test]
fn invalid_bodies_name_the_field_and_the_code() {
    let mut api = Api::new();
    api.sim.world.put_doc("docs/prd.md", b"# PRD\n");
    let big = vec![b'a'; 200 * 1024];
    api.sim.world.put_doc("docs/big-1.md", &big);
    api.sim.world.put_doc("docs/big-2.md", &big);
    let many: Vec<String> = (0..17).map(|i| format!("docs/c{i}.md")).collect();
    let cases = [
        (
            json!({ "idea": IDEA, "path": "docs/../x.md" }),
            "path",
            "invalid",
        ),
        (
            json!({ "idea": IDEA, "path": "other/x.md" }),
            "path",
            "outside_project",
        ),
        (
            json!({ "idea": IDEA, "path": "docs/specs/x.txt" }),
            "path",
            "not_markdown",
        ),
        (json!({ "idea": IDEA, "path": "" }), "path", "required"),
        (json!({ "idea": IDEA }), "path", "required"),
        (json!({ "idea": "  \n ", "path": PATH }), "idea", "required"),
        (json!({ "path": PATH }), "idea", "required"),
        (
            json!({ "idea": "x".repeat(8001), "path": PATH }),
            "idea",
            "too_long",
        ),
        (
            json!({ "idea": IDEA, "path": PATH, "context_paths": many }),
            "context_paths",
            "too_many",
        ),
        (
            json!({ "idea": IDEA, "path": PATH, "context_paths": ["docs/prd.md", "docs/prd.md"] }),
            "context_paths",
            "duplicate",
        ),
        (
            json!({ "idea": IDEA, "path": PATH, "context_paths": [PATH] }),
            "context_paths",
            "same_as_path",
        ),
        (
            json!({ "idea": IDEA, "path": PATH, "context_paths": ["docs/prd.md", "docs/none.md"] }),
            "context_paths[1]",
            "not_found",
        ),
        (
            json!({ "idea": IDEA, "path": PATH, "context_paths": ["other/a.md"] }),
            "context_paths[0]",
            "outside_project",
        ),
        (
            json!({ "idea": IDEA, "path": PATH, "context_paths": ["docs/big-1.md", "docs/big-2.md"] }),
            "context_paths",
            "too_large",
        ),
        (
            json!({ "idea": IDEA, "path": PATH, "locale": "fr" }),
            "locale",
            "invalid",
        ),
        (
            json!({ "idea": IDEA, "path": PATH, "title": "x" }),
            "title",
            "unknown",
        ),
    ];
    for (body, field, code) in cases {
        let r = api.create(body.clone());
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {}",
            r.body
        );
        assert_eq!(r.code(), "validation_failed", "{body}");
        assert_eq!(r.error(), (field.to_string(), code.to_string()), "{body}");
    }
    let listed = api.get(&format!("{}?state=all", api.drafts_url()));
    assert_eq!(listed.body["items"], json!([]));
}

#[test]
fn an_unreachable_remote_at_create_is_a_bad_gateway() {
    let mut api = Api::new();
    api.sim.world.fail_doc_opens(true);
    let r = api.create(json!({ "idea": IDEA, "path": PATH }));
    assert_eq!(r.status, StatusCode::BAD_GATEWAY, "{}", r.body);
    assert_eq!(r.code(), "scm_unavailable");
    api.sim.world.fail_doc_opens(false);
    assert_eq!(
        api.create(json!({ "idea": IDEA, "path": PATH })).status,
        StatusCode::CREATED
    );
}

#[test]
fn the_list_pages_with_a_cursor_and_filters_by_state() {
    let mut api = Api::new();
    let ids: Vec<SpecDraftId> = ["a", "b", "c"]
        .iter()
        .map(|n| api.draft_at(&format!("docs/specs/{n}.md")))
        .collect();
    let url = api.drafts_url();
    let first = api.get(&format!("{url}?limit=2"));
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    let items = first.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], ids[2].0.to_string());
    assert_eq!(items[1]["id"], ids[1].0.to_string());
    assert!(items.iter().all(|i| i["idea"].is_null()));
    let cursor = first.body["next_cursor"].as_str().unwrap().to_string();
    let second = api.get(&format!("{url}?limit=2&cursor={cursor}"));
    let rest = second.body["items"].as_array().unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0]["id"], ids[0].0.to_string());
    assert_eq!(second.body["next_cursor"], Value::Null);
    let other_filter = api.get(&format!("{url}?limit=2&state=all&cursor={cursor}"));
    assert_eq!(other_filter.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(other_filter.code(), "cursor_invalid");

    assert_eq!(api.cancel(ids[1]).status, StatusCode::ACCEPTED);
    let count = |api: &Api, state: &str| {
        api.get(&format!("{url}?state={state}")).body["items"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(count(&api, "active"), 2);
    assert_eq!(count(&api, "finished"), 1);
    assert_eq!(count(&api, "all"), 3);
    assert_eq!(api.get(&url).body["items"].as_array().unwrap().len(), 2);
    let bad = api.get(&format!("{url}?state=done"));
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bad.error(), ("state".to_string(), "invalid".to_string()));
    assert_eq!(
        api.get(&format!("{url}?limit=0")).status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let unknown = api.get(&format!(
        "/api/v1/projects/{}/spec-drafts",
        uuid::Uuid::now_v7()
    ));
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    let waiting = api.get(&format!("/api/v1/spec-drafts/{}", ids[2].0));
    assert_eq!(
        waiting.body["waiting_on"],
        json!([{ "kind": "architect_slot" }])
    );
}

#[test]
fn rounds_content_and_packet_follow_a_draft_that_succeeds() {
    let mut api = Api::new();
    api.sim.world.put_doc("docs/prd.md", b"# PRD\n");
    api.sim
        .world
        .script_draft(vec![write(bad_crlf()), write(good())]);
    let created =
        api.create(json!({ "idea": IDEA, "path": PATH, "context_paths": ["docs/prd.md"] }));
    let id = id_of(&created.body);
    let base = format!("/api/v1/spec-drafts/{}", id.0);
    let none = api.get(&format!("{base}/content"));
    assert_eq!(none.status, StatusCode::NOT_FOUND);
    assert_eq!(none.code(), "not_found");
    let early = api.get(&format!("{base}/packet"));
    assert_eq!(early.status, StatusCode::CONFLICT);
    assert_eq!(early.code(), "invalid_state");
    assert_eq!(early.body["params"]["state"], "queued");
    assert_eq!(
        api.get(&format!("{base}/lint-rounds")).body["items"],
        json!([])
    );

    api.run_to_state(id, SpecDraftState::Succeeded);
    let detail = api.get(&base).body;
    assert_eq!(detail["state"], "succeeded");
    let draft = api.sim.draft(id);
    assert_eq!(
        detail["spec_id"],
        draft.draft.spec_id.unwrap().0.to_string()
    );
    assert_eq!(
        detail["commit"],
        draft.draft.commit_sha.clone().unwrap().as_str()
    );
    assert_eq!(detail["attempt"]["number"], 1);
    assert_eq!(detail["attempt"]["trigger"], "auto");
    assert_eq!(detail["attempt"]["state"], "succeeded");
    assert_eq!(detail["attempt"]["rounds"], 2);
    assert_eq!(detail["attempt"]["max_rounds"], 4);
    assert_eq!(detail["last_round"]["round"], 2);
    assert_eq!(detail["last_round"]["outcome"], "passed");
    assert_eq!(detail["available_actions"], json!([]));
    assert_eq!(detail["budget"], Value::Null);
    assert!(detail["finished_at"].is_string());

    let rounds = api.get(&format!("{base}/lint-rounds")).body;
    let items = rounds["items"].as_array().unwrap();
    let shape: Vec<(u64, u64, &str)> = items
        .iter()
        .map(|r| {
            (
                r["attempt"].as_u64().unwrap(),
                r["round"].as_u64().unwrap(),
                r["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(shape, [(1, 1, "failed"), (1, 2, "passed")]);
    assert!(!items[0]["issues"].as_array().unwrap().is_empty());
    assert_eq!(items[0]["parser_version"], 6);
    assert!(items[0]["session_id"].is_string());

    let content = api.get(&format!("{base}/content")).body;
    assert_eq!(content["round"], 2);
    assert_eq!(content["path"], PATH);
    assert_eq!(
        content["content"].as_str().unwrap().as_bytes(),
        good().as_slice()
    );

    let packet = api.get(&format!("{base}/packet")).body;
    assert_eq!(packet["attempt"], 1);
    let text = packet["text"].as_str().unwrap();
    let hex: String = Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(packet["sha256"], hex.as_str());
    assert_eq!(
        packet["base_sha"],
        draft.attempt.unwrap().base_sha.unwrap().as_str()
    );
    let docs = packet["documents"].as_array().unwrap();
    assert_eq!(docs[0]["origin"], "template");
    assert_eq!(docs[0]["source"], "saka:spec-template/en (parser 6)");
    assert_eq!(docs[0]["path"], Value::Null);
    assert_eq!(docs[1]["origin"], "rules");
    let prd = docs.iter().find(|d| d["path"] == "docs/prd.md").unwrap();
    assert_eq!(prd["origin"], "selected");
    assert_eq!(
        prd["source"],
        format!("docs/prd.md @ {}", packet["base_sha"].as_str().unwrap()).as_str()
    );
}

#[test]
fn the_content_of_a_failed_draft_is_the_last_written_bytes_exactly() {
    let mut api = Api::new();
    let id = api.scripted(vec![
        DraftTurn::Nothing,
        write(bad_crlf()),
        DraftTurn::Nothing,
        DraftTurn::Nothing,
    ]);
    api.run_to_state(id, SpecDraftState::Failed);
    let base = format!("/api/v1/spec-drafts/{}", id.0);
    let detail = api.get(&base).body;
    assert_eq!(detail["failure"]["code"], "lint_rounds_exhausted");
    assert_eq!(detail["primary_action"], "open_written");
    assert_eq!(detail["available_actions"], json!(["retry", "cancel"]));
    assert_eq!(detail["last_round"]["outcome"], "failed");
    assert_eq!(detail["last_round"]["round"], 4);
    let rounds = api.get(&format!("{base}/lint-rounds")).body;
    assert_eq!(rounds["items"][0]["outcome"], "missing");
    assert_eq!(rounds["items"][0]["bytes"], Value::Null);
    assert_eq!(rounds["items"][0]["content_hash"], Value::Null);
    let content = api.get(&format!("{base}/content")).body;
    assert_eq!(content["round"], 4);
    assert_eq!(
        content["content"].as_str().unwrap().as_bytes(),
        bad_crlf().as_slice()
    );
    assert_eq!(content["bytes"], bad_crlf().len());
    assert_eq!(
        content["content_hash"],
        saka_spec::content_hash(&bad_crlf()).as_str()
    );
}

#[test]
fn retry_returns_the_draft_to_the_queue_until_the_limit() {
    let mut api = Api::new();
    let id = api.scripted(vec![DraftTurn::Nothing; 16]);
    let queued = api.retry(id, json!({ "reason": "again" }));
    assert_eq!(queued.status, StatusCode::CONFLICT);
    assert_eq!(queued.code(), "invalid_state");
    for n in 1..=3 {
        api.run_to_state(id, SpecDraftState::Failed);
        let r = api.retry(
            id,
            json!({ "reason": format!("try {n}"), "note": "Write the file first." }),
        );
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
        assert_eq!(r.body["state"], "queued");
        assert_eq!(r.body["retries"], n);
        assert_eq!(
            r.headers["etag"],
            format!("W/\"{}\"", r.body["version"]).as_str()
        );
        api.run_to(id, |d| d.draft.attempt == n + 1);
    }
    api.run_to_state(id, SpecDraftState::Failed);
    let detail = api.get(&format!("/api/v1/spec-drafts/{}", id.0)).body;
    assert_eq!(detail["attempt"]["number"], 4);
    assert_eq!(detail["attempt"]["trigger"], "manual");
    assert_eq!(detail["attempt"]["note"], "Write the file first.");
    assert_eq!(detail["available_actions"], json!(["cancel"]));
    let limit = api.retry(id, json!({ "reason": "once more" }));
    assert_eq!(limit.status, StatusCode::CONFLICT, "{}", limit.body);
    assert_eq!(limit.code(), "retry_limit_reached");
    assert_eq!(limit.body["params"]["limit"], 3);

    for (body, field) in [
        (json!({ "reason": "" }), "reason"),
        (json!({ "reason": "r".repeat(501) }), "reason"),
        (json!({ "reason": "ok", "note": "n".repeat(4001) }), "note"),
        (json!({ "reason": "ok", "note": "" }), "note"),
        (json!({ "note": "n" }), "reason"),
    ] {
        let r = api.retry(id, body.clone());
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}: {}",
            r.body
        );
        assert_eq!(r.error().0, field, "{body}");
    }
    let unknown = api.retry(
        SpecDraftId(uuid::Uuid::now_v7()),
        json!({ "reason": "again" }),
    );
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
}

#[test]
fn a_failure_without_retry_refuses_retry_and_committing_refuses_cancel() {
    let mut api = Api::new();
    let id = api.scripted(vec![write(good())]);
    api.run_to_state(id, SpecDraftState::Committing);
    let refused = api.cancel(id);
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);
    assert_eq!(refused.code(), "invalid_state");
    api.sim.world.put_doc(PATH, b"# Someone else\n");
    api.run_to_state(id, SpecDraftState::Failed);
    let detail = api.get(&format!("/api/v1/spec-drafts/{}", id.0)).body;
    assert_eq!(detail["failure"]["code"], "target_exists");
    assert_eq!(detail["primary_action"], "open_written");
    assert_eq!(detail["available_actions"], json!(["cancel"]));
    let retry = api.retry(id, json!({ "reason": "again" }));
    assert_eq!(retry.status, StatusCode::CONFLICT);
    assert_eq!(retry.code(), "invalid_state");
    let cancelled = api.cancel(id);
    assert_eq!(cancelled.status, StatusCode::ACCEPTED, "{}", cancelled.body);
    assert_eq!(cancelled.body["state"], "cancelled");
    let again = api.cancel(id);
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.code(), "invalid_state");
}

#[test]
fn cancel_works_in_every_live_state_and_not_after_success() {
    let mut api = Api::with(SimConfig {
        agent: AgentScript {
            outcome: AgentOutcome::Hang,
            ..AgentScript::default()
        },
        ..SimConfig::default()
    });
    let queued = api.draft_at("docs/specs/q.md");
    let r = api.cancel(queued);
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
    assert_eq!(r.body["state"], "cancelled");
    assert_eq!(r.body["available_actions"], json!([]));

    for state in [SpecDraftState::Preparing, SpecDraftState::Drafting] {
        let id = api.draft_at(&format!("docs/specs/{}.md", state.as_str()));
        api.run_to_state(id, state);
        let r = api.cancel(id);
        assert_eq!(r.status, StatusCode::ACCEPTED, "{state:?}: {}", r.body);
        assert_eq!(r.body["state"], "cancelled");
        api.run_to(id, |d| {
            d.live_session().is_none() && d.live_environment().is_none()
        });
    }

    let mut api = Api::new();
    let id = api.scripted(vec![write(good())]);
    api.run_to_state(id, SpecDraftState::Succeeded);
    let r = api.cancel(id);
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "invalid_state");
    for body in [
        json!({ "reason": "" }),
        json!({ "reason": "r".repeat(501) }),
        json!({}),
    ] {
        let r = api.post(
            &format!("/api/v1/spec-drafts/{}/actions/cancel", id.0),
            &body,
        );
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(r.error().0, "reason");
    }
}

#[test]
fn a_running_draft_shows_its_session_and_budget_and_refuses_remember() {
    let mut api = Api::with(SimConfig {
        agent: AgentScript {
            outcome: AgentOutcome::Hang,
            ask: Some(("Bash".into(), json!({ "command": "ls" }))),
            ..AgentScript::default()
        },
        ..SimConfig::default()
    });
    let id = api.scripted(vec![write(good())]);
    api.run_to(id, |d| {
        d.session
            .as_ref()
            .is_some_and(|s| s.state == SessionState::Running)
            && !d.permissions.is_empty()
    });
    let detail = api.get(&format!("/api/v1/spec-drafts/{}", id.0)).body;
    assert_eq!(detail["state"], "drafting");
    assert_eq!(detail["session"]["state"], "running");
    assert_eq!(detail["budget"]["attempt_id"], detail["attempt"]["id"]);
    assert_eq!(detail["budget"]["warn_percent"], 80);
    assert_eq!(detail["budget"]["turns"]["limit"], 6);
    assert_eq!(detail["budget"]["tokens"]["limit"], 0);
    let sessions = api
        .get(&format!("/api/v1/sessions?spec_draft_id={}", id.0))
        .body;
    assert_eq!(sessions["items"][0]["id"], detail["session"]["id"]);

    let request = api.sim.draft(id).permissions[0].id;
    let r = api.post(
        &format!("/api/v1/permission-requests/{}/answer", request.0),
        &json!({ "decision": "allow", "remember": true }),
    );
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
    assert_eq!(
        r.error(),
        ("remember".to_string(), "not_applicable".to_string())
    );
}

#[test]
fn active_sessions_list_a_draft_beside_a_task_newest_first_and_page_by_cursor() {
    let mut api = Api::with(SimConfig {
        agent: AgentScript {
            outcome: AgentOutcome::Hang,
            ..AgentScript::default()
        },
        ..SimConfig::default()
    });
    api.sim.approve();
    assert!(api.sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Running)
    }));
    let id = api.scripted(vec![write(good())]);
    api.run_to(id, |d| {
        d.session
            .as_ref()
            .is_some_and(|s| s.state == SessionState::Running)
    });

    let all = api.get("/api/v1/sessions?active=true");
    assert_eq!(all.status, StatusCode::OK, "{}", all.body);
    let items = all.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{}", all.body);
    assert_eq!(items[0]["role"], "architect");
    assert_eq!(items[0]["owner"]["kind"], "draft_attempt");
    assert_eq!(items[0]["owner"]["spec_draft_id"], id.0.to_string());
    assert_eq!(items[0]["title"], "export");
    assert_eq!(items[1]["role"], "dev");
    assert_eq!(items[1]["owner"]["kind"], "stage_run");
    assert!(items[0]["started_at"].as_str() >= items[1]["started_at"].as_str());
    for item in items {
        assert_eq!(item["pending"], json!({ "permissions": 0, "decisions": 0 }));
    }

    let first = api.get("/api/v1/sessions?active=true&limit=1");
    assert_eq!(first.body["items"][0]["id"], items[0]["id"]);
    let cursor = first.body["next_cursor"].as_str().unwrap();
    let second = api.get(&format!(
        "/api/v1/sessions?active=true&limit=1&cursor={cursor}"
    ));
    assert_eq!(second.body["items"][0]["id"], items[1]["id"]);
    assert_eq!(second.body["next_cursor"], Value::Null);
}

#[test]
fn a_session_draft_is_a_conversation_the_operator_commits() {
    let mut api = Api::new();
    api.sim.world.script_draft(vec![write(good())]);
    let created = api.create(json!({ "idea": IDEA, "path": PATH, "mode": "session" }));
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["mode"], "session");
    let id = id_of(&created.body);
    let message = format!("/api/v1/spec-drafts/{}/message", id.0);
    let commit = format!("/api/v1/spec-drafts/{}/commit", id.0);
    let discard = format!("/api/v1/spec-drafts/{}/discard", id.0);

    let early = api.post(&message, &json!({ "text": "hello" }));
    assert_eq!(early.status, StatusCode::CONFLICT);
    assert_eq!(early.code(), "draft_not_waiting");

    api.run_to_state(id, SpecDraftState::AwaitingOperator);
    let waiting = api.get(&format!("/api/v1/spec-drafts/{}", id.0));
    assert_eq!(waiting.body["state"], "awaiting_operator");
    assert_eq!(
        waiting.body["available_actions"],
        json!(["message", "commit", "discard", "cancel"])
    );
    assert_eq!(waiting.body["last_round"]["outcome"], "passed");

    for body in [json!({ "text": "" }), json!({ "text": "x".repeat(4001) })] {
        let r = api.post(&message, &body);
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(r.error().0, "text");
    }
    let long = api.post(&commit, &json!({ "message": "m".repeat(201) }));
    assert_eq!(long.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(long.error().0, "message");
    let blank = api.post(&discard, &json!({ "reason": "" }));
    assert_eq!(blank.status, StatusCode::UNPROCESSABLE_ENTITY);

    let sent = api.post_keyed(&message, "msg-1", &json!({ "text": "Add examples." }));
    assert_eq!(sent.status, StatusCode::ACCEPTED, "{}", sent.body);
    assert_eq!(sent.body["state"], "drafting");
    let replay = api.post_keyed(&message, "msg-1", &json!({ "text": "Add examples." }));
    assert_eq!(replay.body, sent.body);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    api.run_to(id, |d| {
        d.draft.state == SpecDraftState::AwaitingOperator
            && d.attempt.as_ref().is_some_and(|a| a.rounds == 2)
    });
    assert_eq!(api.sim.world.prompts().len(), 2);

    let done = api.post(&commit, &json!({ "message": "docs: add the export spec" }));
    assert_eq!(done.status, StatusCode::ACCEPTED, "{}", done.body);
    assert_eq!(done.body["state"], "committing");
    let again = api.post(&commit, &json!({}));
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.code(), "draft_not_waiting");
    api.run_to_state(id, SpecDraftState::Succeeded);
    assert_eq!(api.sim.world.doc_commits().len(), 1);
}

#[test]
fn a_session_draft_that_does_not_lint_cannot_be_committed_and_can_be_discarded() {
    let mut api = Api::new();
    api.sim.world.script_draft(vec![write(bad_crlf())]);
    let created = api.create(json!({ "idea": IDEA, "path": PATH, "mode": "session" }));
    let id = id_of(&created.body);
    api.run_to_state(id, SpecDraftState::AwaitingOperator);
    let waiting = api.get(&format!("/api/v1/spec-drafts/{}", id.0));
    assert_eq!(
        waiting.body["available_actions"],
        json!(["message", "discard", "cancel"])
    );
    let refused = api.post(&format!("/api/v1/spec-drafts/{}/commit", id.0), &json!({}));
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.code(), "draft_lint_failed");
    let discarded = api.post(
        &format!("/api/v1/spec-drafts/{}/discard", id.0),
        &json!({ "reason": "wrong direction" }),
    );
    assert_eq!(discarded.status, StatusCode::ACCEPTED, "{}", discarded.body);
    assert_eq!(discarded.body["state"], "cancelled");
}

#[test]
fn the_mode_of_a_new_draft_is_checked() {
    let mut api = Api::new();
    let bad = api.create(json!({ "idea": IDEA, "path": PATH, "mode": "chat" }));
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bad.error(), ("mode".into(), "invalid".into()));
    let plain = api.create(json!({ "idea": IDEA, "path": PATH }));
    assert_eq!(plain.body["mode"], "draft");
}

#[test]
fn changes_list_the_files_with_their_diffs_and_the_lint_of_the_last_turn() {
    let mut api = Api::new();
    api.sim
        .world
        .put_doc("docs/prd.md", b"# PRD\n\nOld line.\n");
    let big = format!("# Notes\n{}", "x".repeat(300 * 1024)).into_bytes();
    api.sim.world.script_draft(vec![DraftTurn::WriteAll(vec![
        (PATH.into(), good()),
        ("docs/prd.md".into(), b"# PRD\n\nNew line.\n".to_vec()),
        ("docs/notes/big.md".into(), big),
    ])]);
    let created = api.create(json!({ "idea": IDEA, "path": PATH, "mode": "session" }));
    let id = id_of(&created.body);
    let url = format!("/api/v1/spec-drafts/{}/changes", id.0);
    let early = api.get(&url);
    assert_eq!(early.status, StatusCode::OK);
    assert_eq!(early.body["files"], json!([]));
    assert_eq!(early.body["lint"]["passed"], false);

    api.run_to_state(id, SpecDraftState::AwaitingOperator);
    let changes = api.get(&url);
    assert_eq!(changes.status, StatusCode::OK, "{}", changes.body);
    let files = changes.body["files"].as_array().unwrap();
    let paths: Vec<_> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["docs/notes/big.md", "docs/prd.md", PATH]);
    assert_eq!(files[0]["status"], "added");
    assert_eq!(files[0]["truncated"], true);
    assert_eq!(files[0]["diff"].as_str().unwrap().len(), 256 * 1024);
    assert_eq!(files[1]["status"], "modified");
    assert_eq!(files[1]["truncated"], false);
    let diff = files[1]["diff"].as_str().unwrap();
    assert!(
        diff.contains("-Old line.") && diff.contains("+New line."),
        "{diff}"
    );
    assert_eq!(changes.body["lint"]["passed"], true);
    assert_eq!(changes.body["lint"]["findings"], json!([]));

    let missing = api.get(&format!(
        "/api/v1/spec-drafts/{}/changes",
        uuid::Uuid::now_v7()
    ));
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[test]
fn a_failing_lint_shows_in_the_changes_and_a_plain_draft_has_none() {
    let mut api = Api::new();
    api.sim.world.script_draft(vec![write(bad_crlf())]);
    let created = api.create(json!({ "idea": IDEA, "path": PATH, "mode": "session" }));
    let id = id_of(&created.body);
    api.run_to_state(id, SpecDraftState::AwaitingOperator);
    let changes = api.get(&format!("/api/v1/spec-drafts/{}/changes", id.0));
    assert_eq!(changes.body["lint"]["passed"], false);
    assert!(
        !changes.body["lint"]["findings"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let mut api = Api::new();
    let plain = api.scripted(vec![write(good())]);
    let changes = api.get(&format!("/api/v1/spec-drafts/{}/changes", plain.0));
    assert_eq!(changes.body["files"], json!([]));
}

#[test]
fn commit_refuses_an_empty_draft_and_a_moved_file_with_its_paths() {
    let mut api = Api::new();
    api.sim.world.put_doc(PATH, &good());
    api.sim.world.put_doc("docs/prd.md", b"# PRD v1\n");
    api.sim.world.script_draft(vec![DraftTurn::Nothing]);
    let created = api.create(json!({
        "idea": IDEA, "path": PATH, "mode": "session", "target_kind": "revise"
    }));
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["target_kind"], "revise");
    let id = id_of(&created.body);
    api.run_to_state(id, SpecDraftState::AwaitingOperator);
    let commit = format!("/api/v1/spec-drafts/{}/commit", id.0);
    let empty = api.post(&commit, &json!({}));
    assert_eq!(empty.status, StatusCode::CONFLICT);
    assert_eq!(empty.code(), "draft_no_changes");

    let mut api = Api::new();
    api.sim.world.put_doc("docs/prd.md", b"# PRD v1\n");
    api.sim.world.script_draft(vec![DraftTurn::WriteAll(vec![
        (PATH.into(), good()),
        ("docs/prd.md".into(), b"# PRD v1\n\nMine.\n".to_vec()),
    ])]);
    let created = api.create(json!({ "idea": IDEA, "path": PATH, "mode": "session" }));
    let id = id_of(&created.body);
    api.run_to_state(id, SpecDraftState::AwaitingOperator);
    api.sim.world.put_doc("docs/prd.md", b"# PRD v2\n");
    let moved = api.post(&format!("/api/v1/spec-drafts/{}/commit", id.0), &json!({}));
    assert_eq!(moved.status, StatusCode::CONFLICT);
    assert_eq!(moved.code(), "draft_base_moved");
    assert_eq!(moved.body["params"]["paths"], json!(["docs/prd.md"]));
}

#[test]
fn the_target_kind_is_checked_and_a_revision_needs_an_existing_file() {
    let mut api = Api::new();
    let bad = api.create(json!({ "idea": IDEA, "path": PATH, "target_kind": "rewrite" }));
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bad.error(), ("target_kind".into(), "invalid".into()));
    let missing = api.create(json!({ "idea": IDEA, "path": PATH, "target_kind": "revise" }));
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.code(), "file_not_found");
    assert_eq!(missing.body["params"]["path"], PATH);
    let plain = api.create(json!({ "idea": IDEA, "path": PATH }));
    assert_eq!(plain.body["target_kind"], "new");

    api.sim.world.put_doc("docs/specs/there.md", &good());
    let revise = api.create(json!({
        "idea": IDEA, "path": "docs/specs/there.md", "target_kind": "revise"
    }));
    assert_eq!(revise.status, StatusCode::CREATED, "{}", revise.body);
    let twice = api.create(json!({
        "idea": IDEA, "path": "docs/specs/there.md", "target_kind": "revise"
    }));
    assert_eq!(twice.status, StatusCode::CONFLICT);
    assert_eq!(twice.body["params"]["reason"], "draft_active");
}
