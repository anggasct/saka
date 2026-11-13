#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
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
use saka_testkit::{Sim, SimConfig};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct Api {
    sim: Sim,
    app: Router,
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
        Api { sim, app }
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
