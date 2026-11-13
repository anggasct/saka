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
use saka_core::Ctx;
use saka_core::deploy::{self, CheckRecord, Deployment, Input, QueueRequest};
use saka_core::environment::{self, RawEnvironment};
use saka_core::ids::IdSource;
use saka_core::model::Coded;
use saka_core::time::Timestamp;
use saka_core::vocab::{
    DeployActionKind, DeployCause, DeployCheckState, DeployPhase, DeploymentState,
};
use saka_store::Batch;
use saka_testkit::{CommandScript, Sim, SimConfig};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct Api {
    sim: Sim,
    app: Router,
    keys: std::cell::Cell<u32>,
}

struct Reply {
    status: StatusCode,
    body: Value,
}

struct Ids(u128);

impl IdSource for Ids {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

impl Api {
    fn new() -> Api {
        let staging = RawEnvironment {
            project: "tasks".into(),
            environment: "staging".into(),
            action: "run_host".into(),
            command: Some("./secret-deploy.sh".into()),
            target_url: Some("https://staging.example.com".into()),
            on_failure: Some("revert".into()),
            ..RawEnvironment::default()
        };
        let preview = RawEnvironment {
            environment: "preview".into(),
            action: "webhook".into(),
            command: None,
            url: Some("https://ci.example.com/hooks/deploy?token=hunter2".into()),
            secret: Some("DEPLOY_SIGNING_KEY".into()),
            gate: Some(false),
            on_failure: Some("fix".into()),
            ..staging.clone()
        };
        let sim = Sim::new(SimConfig {
            environments: environment::parse_all(&[staging, preview]).unwrap(),
            ..SimConfig::default()
        });
        let state = AppState::new(ApiConfig::new("0.1.0-test"));
        state.set_engine(sim.engine());
        let app = router(state);
        Api {
            sim,
            app,
            keys: std::cell::Cell::new(0),
        }
    }

    fn post_with(&self, path: &str, key: Option<&str>, body: Value) -> Reply {
        let mut req = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header("host", "127.0.0.1:7420")
            .header("content-type", "application/json");
        if let Some(key) = key {
            req = req.header("idempotency-key", key);
        }
        let req = req.body(Body::from(body.to_string())).unwrap();
        self.sim.block_on(async {
            let response = self.app.clone().oneshot(req).await.unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            Reply {
                status,
                body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            }
        })
    }

    fn post(&self, path: &str, body: Value) -> Reply {
        self.keys.set(self.keys.get() + 1);
        let key = format!("key-{}", self.keys.get());
        self.post_with(path, Some(&key), body)
    }

    fn get(&self, path: &str) -> Reply {
        let req = Request::get(path)
            .header("host", "127.0.0.1:7420")
            .body(Body::empty())
            .unwrap();
        self.sim.block_on(async {
            let response = self.app.clone().oneshot(req).await.unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            Reply {
                status,
                body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            }
        })
    }

    fn project(&self) -> String {
        self.sim.project_id.to_string()
    }

    fn deployment(&self, n: u128, environment: &str) -> Deployment {
        let mut ids = Ids(n * 1000);
        let at = Timestamp(i64::try_from(n).unwrap() * 1_000);
        let mut ctx = Ctx {
            now: at,
            ids: &mut ids,
        };
        let events = deploy::queue(
            None,
            QueueRequest {
                project_id: self.sim.project_id,
                repo_id: self.sim.project().project.repos[0].repo_id,
                environment: environment.into(),
                branch: "main".into(),
                cause: DeployCause::Redeploy,
                action: DeployActionKind::RunHost,
                target_url: None,
                on_failure: saka_core::vocab::OnFailure::Revert,
                actor: None,
                reason: None,
                change_request_id: None,
            },
            &mut ctx,
        )
        .unwrap();
        events
            .iter()
            .fold(None, |d, e| deploy::evolve(d, e, at))
            .unwrap()
    }

    fn commit(&self, deployments: Vec<Deployment>, checks: Vec<(Deployment, CheckRecord)>) {
        let mut batch = Batch::new(Timestamp(10_000));
        batch.deployments = deployments;
        batch.deployment_checks = checks.into_iter().map(|(d, c)| (d.id, c)).collect();
        self.sim.engine().store().commit(&batch).unwrap();
    }
}

fn failed(d: &Deployment) -> Deployment {
    let step = |d: &Deployment, input| {
        let events = deploy::decide(d, input).unwrap();
        deploy::evolve_all(Some(d.clone()), &events, Timestamp(5_000)).unwrap()
    };
    let started = step(
        d,
        Input::Start {
            sha: "b".repeat(40),
            previous_sha: Some("a".repeat(40)),
        },
    );
    step(
        &started,
        Input::Fail {
            phase: DeployPhase::Verify,
            error: Coded {
                code: "verify_failed".into(),
                params: json!({ "failed": 1, "total": 2 }),
            },
        },
    )
}

#[test]
fn environments_list_their_binding_and_verification_and_never_the_action() {
    let api = Api::new();
    let reply = api.get(&format!("/api/v1/projects/{}/environments", api.project()));
    assert_eq!(reply.status, StatusCode::OK);
    let items = reply.body["items"].as_array().unwrap();
    assert_eq!(
        items
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["preview", "staging"]
    );
    let staging = &items[1];
    assert_eq!(staging["repo"]["name"], "app");
    assert_eq!(staging["branch"], "main");
    assert_eq!(staging["action"], "run_host");
    assert_eq!(staging["gate"], true);
    assert_eq!(staging["on_failure"], "revert");
    assert_eq!(staging["target_url"], "https://staging.example.com");
    assert_eq!(
        staging["verification"]["warnings"],
        json!(["verify_missing"])
    );
    assert_eq!(items[0]["gate"], false);
    assert_eq!(
        items[0]["verification"]["warnings"],
        json!(["verify_missing", "ready_missing"])
    );
    let text = reply.body.to_string();
    for secret in [
        "secret-deploy",
        "hunter2",
        "ci.example.com",
        "DEPLOY_SIGNING_KEY",
    ] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    assert_eq!(
        api.get(&format!(
            "/api/v1/projects/{}/environments",
            Uuid::from_u128(9_999_999)
        ))
        .status,
        StatusCode::NOT_FOUND
    );
}

#[test]
fn deployments_are_listed_newest_first_filtered_and_read_with_their_evidence() {
    let api = Api::new();
    let project = api.project();
    let empty = api.get(&format!("/api/v1/projects/{project}/deployments"));
    assert_eq!(empty.status, StatusCode::OK);
    assert_eq!(empty.body["items"], json!([]));

    let older = failed(&api.deployment(1, "staging"));
    let newer = api.deployment(2, "preview");
    let check = CheckRecord {
        phase: DeployPhase::Verify,
        ordinal: 1,
        command: Some("npm run e2e".into()),
        state: DeployCheckState::Failed,
        exit_code: Some(1),
        http_status: None,
        attempts: 1,
        duration_ms: 90,
        output_tail: "boom".into(),
        log: None,
        started_at: Timestamp(5_100),
        finished_at: Timestamp(5_190),
    };
    api.commit(
        vec![older.clone(), newer.clone()],
        vec![(older.clone(), check)],
    );

    let list = api.get(&format!("/api/v1/projects/{project}/deployments?limit=1"));
    assert_eq!(list.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(list.body["items"][0]["id"], newer.id.to_string());
    assert_eq!(list.body["items"][0]["state"], "queued");
    assert!(list.body["items"][0].get("checks").is_none());
    let cursor = list.body["next_cursor"].as_str().unwrap();
    let second = api.get(&format!(
        "/api/v1/projects/{project}/deployments?limit=1&cursor={cursor}"
    ));
    assert_eq!(second.body["items"][0]["id"], older.id.to_string());
    assert!(second.body["next_cursor"].is_null());

    let filtered = api.get(&format!(
        "/api/v1/projects/{project}/deployments?environment=staging&state={}",
        DeploymentState::Failed
    ));
    assert_eq!(filtered.body["items"].as_array().unwrap().len(), 1);
    let bad = api.get(&format!(
        "/api/v1/projects/{project}/deployments?state=gone"
    ));
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bad.body["code"], "validation_failed");

    let detail = api.get(&format!("/api/v1/deployments/{}", older.id));
    assert_eq!(detail.status, StatusCode::OK);
    let body = &detail.body;
    assert_eq!(body["state"], "failed");
    assert_eq!(body["phase"], "verify");
    assert_eq!(body["sha"], "b".repeat(40));
    assert_eq!(body["previous_sha"], "a".repeat(40));
    assert_eq!(body["error"]["code"], "verify_failed");
    assert_eq!(body["error"]["params"]["total"], 2);
    assert_eq!(body["checks"][0]["output_tail"], "boom");
    assert_eq!(body["checks"][0]["command"], "npm run e2e");
    assert_eq!(body["merges"], json!([]));
    assert_eq!(body["tasks"], json!([]));
    assert!(
        body["repo"]["full_name"]
            .as_str()
            .is_some_and(|n| n.contains('/'))
    );

    let missing = api.get(&format!("/api/v1/deployments/{}", Uuid::from_u128(77)));
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.body["code"], "not_found");
}

#[test]
fn a_redeploy_queues_one_deployment_and_refuses_a_second_while_it_waits() {
    let api = Api::new();
    let project = api.project();
    let path = format!("/api/v1/projects/{project}/environments/staging/actions/deploy");
    let made = api.post(&path, json!({ "reason": "ship it" }));
    assert_eq!(made.status, StatusCode::ACCEPTED, "{:?}", made.body);
    assert_eq!(made.body["state"], "queued");
    assert_eq!(made.body["cause"], "redeploy");
    assert_eq!(made.body["reason"], "ship it");
    assert_eq!(made.body["environment"], "staging");
    let again = api.post(&path, json!({ "reason": "twice" }));
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.body["code"], "deployment_queued");
    assert_eq!(again.body["params"]["deployment_id"], made.body["id"]);
    let unknown = api.post(
        &format!("/api/v1/projects/{project}/environments/nowhere/actions/deploy"),
        json!({ "reason": "x" }),
    );
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.body["code"], "environment_not_found");
    let empty = api.post(&path, json!({ "reason": "" }));
    assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);
    let keyless = api.post_with(&path, None, json!({ "reason": "x" }));
    assert_eq!(keyless.status, StatusCode::BAD_REQUEST);
    assert_eq!(keyless.body["code"], "idempotency_key_required");
    let replay = api.post_with(&path, Some("same"), json!({ "reason": "once" }));
    let replayed = api.post_with(&path, Some("same"), json!({ "reason": "once" }));
    assert_eq!(
        replay.status, replayed.status,
        "a replay returns the stored answer"
    );
}

#[test]
fn a_waiting_deployment_can_be_cancelled_once_and_a_running_one_cannot() {
    let api = Api::new();
    let project = api.project();
    let made = api.post(
        &format!("/api/v1/projects/{project}/environments/staging/actions/deploy"),
        json!({ "reason": "ship it" }),
    );
    let id = made.body["id"].as_str().unwrap().to_string();
    let cancel = format!("/api/v1/deployments/{id}/actions/cancel");
    let done = api.post(&cancel, json!({ "reason": "changed my mind" }));
    assert_eq!(done.status, StatusCode::OK, "{:?}", done.body);
    assert_eq!(done.body["state"], "cancelled");
    let twice = api.post(&cancel, json!({ "reason": "again" }));
    assert_eq!(twice.status, StatusCode::CONFLICT);
    assert_eq!(twice.body["code"], "deployment_finished");

    let mut running = api.deployment(9, "preview");
    running = deploy::evolve_all(
        Some(running.clone()),
        &deploy::decide(
            &running,
            Input::Start {
                sha: "c".repeat(40),
                previous_sha: None,
            },
        )
        .unwrap(),
        Timestamp(9_500),
    )
    .unwrap();
    api.commit(vec![running.clone()], Vec::new());
    let busy = api.post(
        &format!("/api/v1/deployments/{}/actions/cancel", running.id),
        json!({ "reason": "stop" }),
    );
    assert_eq!(busy.status, StatusCode::CONFLICT);
    assert_eq!(busy.body["code"], "deployment_running");
    let missing = api.post(
        &format!("/api/v1/deployments/{}/actions/cancel", Uuid::from_u128(77)),
        json!({ "reason": "stop" }),
    );
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

fn held_api(on_failure: &str) -> Api {
    let environment = RawEnvironment {
        project: "tasks".into(),
        environment: "staging".into(),
        action: "run_host".into(),
        command: Some("./deploy.sh".into()),
        on_failure: Some(on_failure.into()),
        ..RawEnvironment::default()
    };
    let verify = std::collections::BTreeMap::from([(
        "staging".to_string(),
        saka_core::environment::VerifySpec {
            commands: vec!["npm run smoke".into()],
            ..Default::default()
        },
    )]);
    let mut sim = Sim::new(SimConfig {
        environments: environment::parse_all(&[environment]).unwrap(),
        verify,
        ..SimConfig::default()
    });
    sim.world
        .script_command("npm run smoke", CommandScript::Exit(1));
    sim.approve();
    assert!(sim.run_until(30_000, |s| {
        s.task(1)
            .open_blocker()
            .is_some_and(|b| b.code == saka_core::vocab::BlockerCode::DeployUnverified)
    }));
    let state = AppState::new(ApiConfig::new("0.1.0-test"));
    state.set_engine(sim.engine());
    Api {
        app: router(state),
        sim,
        keys: std::cell::Cell::new(0),
    }
}

#[test]
fn a_task_held_by_a_failed_deployment_shows_its_gate_and_can_be_accepted_without_verification() {
    let api = held_api("revert");
    let task = api.sim.task(1);
    let item = task.item.id.to_string();
    let shown = api.get(&format!("/api/v1/work-items/{item}"));
    assert_eq!(shown.status, StatusCode::OK);
    let gate = &shown.body["deploy_gates"][0];
    assert_eq!(gate["environment"], "staging");
    assert_eq!(gate["state"], "failed");
    assert_eq!(gate["deployment"]["state"], "failed");
    assert_eq!(gate["deployment"]["phase"], "verify");
    assert_eq!(shown.body["blocker"]["code"], "deploy_unverified");
    assert_eq!(shown.body["blocker"]["params"]["environment"], "staging");
    let offered: Vec<&str> = shown.body["available_actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(
        offered.contains(&"retry") && offered.contains(&"accept_unverified"),
        "{offered:?}"
    );
    assert!(!offered.contains(&"takeover"), "{offered:?}");

    let path = format!("/api/v1/work-items/{item}/actions/accept-unverified");
    let wrong = api.post(
        &path,
        json!({ "blocker_id": Uuid::from_u128(5).to_string(), "reason": "staging is flaky" }),
    );
    assert_eq!(wrong.status, StatusCode::CONFLICT);
    assert_eq!(wrong.body["code"], "blocker_not_open");
    let blocker = shown.body["blocker"]["id"].as_str().unwrap().to_string();
    let missing_reason = api.post(&path, json!({ "blocker_id": blocker }));
    assert_eq!(missing_reason.status, StatusCode::UNPROCESSABLE_ENTITY);
    let accepted = api.post(
        &path,
        json!({ "blocker_id": blocker, "reason": "staging is flaky" }),
    );
    assert_eq!(accepted.status, StatusCode::ACCEPTED, "{:?}", accepted.body);
    let after = api.get(&format!("/api/v1/work-items/{item}"));
    assert_eq!(after.body["deploy_gates"][0]["state"], "accepted");
    assert_eq!(after.body["deploy_gates"][0]["reason"], "staging is flaky");
}

#[test]
fn a_draft_fix_is_requested_for_a_failed_deployment_and_once_only() {
    let api = held_api("fix");
    let deployment = api
        .sim
        .engine()
        .deployments(
            api.sim.project_id,
            None,
            None,
            &saka_engine::Page {
                after: None,
                limit: 5,
            },
        )
        .unwrap()
        .items[0]
        .deployment
        .id;
    let path = format!("/api/v1/deployments/{deployment}/actions/draft-fix");
    let made = api.post(&path, json!({ "note": "after the cache change" }));
    assert_eq!(made.status, StatusCode::ACCEPTED, "{:?}", made.body);
    assert_eq!(made.body["kind"], "bugfix");
    assert_eq!(made.body["deployment_id"], deployment.to_string());
    assert!(
        made.body["path"]
            .as_str()
            .unwrap()
            .contains("bugfix-deploy-staging-")
    );
    let twice = api.post(&path, json!({}));
    assert_eq!(twice.status, StatusCode::CONFLICT);
    assert_eq!(twice.body["code"], "fix_draft_in_progress");
    assert_eq!(twice.body["params"]["spec_draft_id"], made.body["id"]);
    let detail = api.get(&format!("/api/v1/deployments/{deployment}"));
    assert_eq!(detail.body["fix_draft"]["id"], made.body["id"]);
    let long = api.post(&path, json!({ "note": "x".repeat(1001) }));
    assert_eq!(long.status, StatusCode::UNPROCESSABLE_ENTITY);
    let unknown = api.post(
        &format!(
            "/api/v1/deployments/{}/actions/draft-fix",
            Uuid::from_u128(77)
        ),
        json!({}),
    );
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
}

#[test]
fn a_draft_fix_of_a_deployment_that_did_not_fail_is_refused() {
    let api = Api::new();
    let made = api.post(
        &format!(
            "/api/v1/projects/{}/environments/staging/actions/deploy",
            api.project()
        ),
        json!({ "reason": "ship it" }),
    );
    let id = made.body["id"].as_str().unwrap();
    let refused = api.post(
        &format!("/api/v1/deployments/{id}/actions/draft-fix"),
        json!({}),
    );
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.body["code"], "deployment_not_failed");
}

#[test]
fn the_primary_action_of_a_held_task_reverts_it_or_drafts_the_fix_by_on_failure() {
    let api = held_api("revert");
    let item = api.sim.task(1).item.id.to_string();
    let shown = api.get(&format!("/api/v1/work-items/{item}"));
    assert_eq!(shown.body["blocker"]["primary_action"]["kind"], "revert");
    let blocker = shown.body["blocker"]["id"].as_str().unwrap().to_string();
    let path = format!("/api/v1/work-items/{item}/actions/primary");
    let stale = api.post(
        &path,
        json!({ "blocker_id": blocker, "action": "retry", "reason": "undo it" }),
    );
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(stale.body["code"], "blocker_action_changed");
    let done = api.post(
        &path,
        json!({ "blocker_id": blocker, "action": "revert", "reason": "staging is broken" }),
    );
    assert_eq!(done.status, StatusCode::ACCEPTED, "{:?}", done.body);
    let after = api.get(&format!("/api/v1/work-items/{item}"));
    assert!(
        after.body["reverted_by"]["key"]
            .as_str()
            .unwrap()
            .ends_with("-R1")
    );

    let api = held_api("fix");
    let item = api.sim.task(1).item.id.to_string();
    let shown = api.get(&format!("/api/v1/work-items/{item}"));
    assert_eq!(shown.body["blocker"]["primary_action"]["kind"], "draft_fix");
    let blocker = shown.body["blocker"]["id"].as_str().unwrap().to_string();
    let done = api.post(
        &format!("/api/v1/work-items/{item}/actions/primary"),
        json!({ "blocker_id": blocker, "action": "draft_fix", "reason": "draft it" }),
    );
    assert_eq!(done.status, StatusCode::ACCEPTED, "{:?}", done.body);
    assert_eq!(done.body["blocker"]["code"], "deploy_unverified");
    let deployment = shown.body["blocker"]["params"]["deployment_id"]
        .as_str()
        .unwrap();
    let detail = api.get(&format!("/api/v1/deployments/{deployment}"));
    assert!(detail.body["fix_draft"]["id"].is_string());
}
