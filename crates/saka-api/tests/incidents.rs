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
use saka_core::deploy::{self, Deployment, Input, QueueRequest};
use saka_core::ids::IdSource;
use saka_core::incident::{self, Arrival, IncidentSignal, Observed, RawSource, Source};
use saka_core::time::Timestamp;
use saka_core::vocab::{DeployActionKind, DeployCause, IncidentSeverity, SignalStatus};
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

fn source(name: &str, extra: impl FnOnce(&mut RawSource)) -> Source {
    let mut raw = RawSource {
        name: name.into(),
        project: "tasks".into(),
        kind: "alertmanager".into(),
        secret_file: "/etc/saka/incident.secret".into(),
        ..RawSource::default()
    };
    extra(&mut raw);
    Source::validate(&raw).unwrap()
}

impl Api {
    fn new() -> Api {
        let sim = Sim::new(SimConfig {
            incident_sources: vec![
                source("prod-alerts", |_| {}),
                source("errors", |s| {
                    s.kind = "sentry".into();
                    s.action = Some("draft".into());
                    s.min_severity = Some("critical".into());
                    s.repo = Some("tasks".into());
                }),
            ],
            public_url: Some("https://saka.example.com".into()),
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

    fn deployment(&self) -> Deployment {
        let at = Timestamp(1_000);
        let mut ids = Ids(7_000);
        let mut ctx = Ctx {
            now: at,
            ids: &mut ids,
        };
        let queued = deploy::queue(
            None,
            QueueRequest {
                project_id: self.sim.project_id,
                repo_id: self.sim.project().project.repos[0].repo_id,
                environment: "staging".into(),
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
        let queued = queued
            .iter()
            .fold(None, |d, e| deploy::evolve(d, e, at))
            .unwrap();
        let events = deploy::decide(
            &queued,
            Input::Start {
                sha: "b".repeat(40),
                previous_sha: Some("a".repeat(40)),
            },
        )
        .unwrap();
        deploy::evolve_all(Some(queued), &events, at).unwrap()
    }

    fn open(
        &self,
        source: &str,
        fingerprint: &str,
        severity: IncidentSeverity,
        environment: Option<&str>,
        deployment: Option<&Deployment>,
        at: i64,
    ) -> Uuid {
        let signal = IncidentSignal {
            fingerprint: fingerprint.into(),
            status: SignalStatus::Firing,
            severity,
            title: format!("<b>{fingerprint}</b> failing"),
            summary: "5xx".into(),
            environment: environment.map(String::from),
            url: Some("https://alerts.example.com/1".into()),
            started_at: Some(Timestamp(at - 500)),
            labels: [("team".to_string(), "web".to_string())].into(),
        };
        let arrival = Arrival {
            project_id: self.sim.project_id,
            source,
            max_open: 50,
            live_in_source: 0,
            deployment_id: deployment.map(|d| d.id),
        };
        let mut ids = Ids(u128::from(at.unsigned_abs()) * 1_000_000);
        let mut ctx = Ctx {
            now: Timestamp(at),
            ids: &mut ids,
        };
        let Observed::Opened(events) = incident::observe(None, &signal, &arrival, &mut ctx) else {
            panic!("expected an incident");
        };
        let incident = incident::evolve_all(None, &events, Timestamp(at)).unwrap();
        let mut batch = Batch::new(Timestamp(at));
        batch.deployments.extend(deployment.cloned());
        batch.incidents.push(incident.clone());
        self.sim.engine().store().commit(&batch).unwrap();
        incident.id.0
    }
}

#[test]
fn sources_list_their_endpoint_and_settings_and_never_the_secret() {
    let api = Api::new();
    api.open(
        "prod-alerts",
        "a",
        IncidentSeverity::High,
        None,
        None,
        1_000,
    );
    let reply = api.get(&format!(
        "/api/v1/projects/{}/incident-sources",
        api.project()
    ));
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.body["items"],
        json!([
            {
                "name": "errors",
                "kind": "sentry",
                "endpoint": "https://saka.example.com/hooks/incident/errors",
                "endpoint_path": "/hooks/incident/errors",
                "action": "draft",
                "min_severity": "critical",
                "max_open": 50,
                "repo": "tasks",
                "open": 0
            },
            {
                "name": "prod-alerts",
                "kind": "alertmanager",
                "endpoint": "https://saka.example.com/hooks/incident/prod-alerts",
                "endpoint_path": "/hooks/incident/prod-alerts",
                "action": "inbox",
                "min_severity": "high",
                "max_open": 50,
                "repo": null,
                "open": 1
            }
        ])
    );
    let text = reply.body.to_string();
    assert!(!text.contains("secret"), "{text}");
}

#[test]
fn incidents_page_most_recently_seen_first_and_filter_by_state_severity_environment_and_source() {
    let api = Api::new();
    let a = api.open(
        "prod-alerts",
        "a",
        IncidentSeverity::Low,
        Some("staging"),
        None,
        1_000,
    );
    let b = api.open(
        "errors",
        "b",
        IncidentSeverity::Critical,
        Some("production"),
        None,
        2_000,
    );
    let c = api.open(
        "prod-alerts",
        "c",
        IncidentSeverity::High,
        Some("staging"),
        None,
        3_000,
    );
    let base = format!("/api/v1/projects/{}/incidents", api.project());

    let all = api.get(&base);
    assert_eq!(all.status, StatusCode::OK);
    let ids: Vec<&str> = all.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [c.to_string(), b.to_string(), a.to_string()]);
    assert!(all.body["next_cursor"].is_null());

    let ids_of = |query: &str| -> Vec<String> {
        api.get(&format!("{base}?{query}")).body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(ids_of("severity=critical"), [b.to_string()]);
    assert_eq!(
        ids_of("environment=staging"),
        [c.to_string(), a.to_string()]
    );
    assert_eq!(ids_of("source=errors"), [b.to_string()]);
    assert_eq!(ids_of("state=open&source=prod-alerts").len(), 2);
    assert!(ids_of("state=dismissed").is_empty());

    let first = api.get(&format!("{base}?limit=2"));
    assert_eq!(first.body["items"].as_array().unwrap().len(), 2);
    let cursor = first.body["next_cursor"].as_str().unwrap();
    let rest = api.get(&format!("{base}?limit=2&cursor={cursor}"));
    assert_eq!(rest.body["items"][0]["id"], a.to_string());
    assert!(rest.body["next_cursor"].is_null());
}

#[test]
fn a_bad_filter_or_limit_is_a_validation_error_and_an_unknown_project_is_not_found() {
    let api = Api::new();
    let base = format!("/api/v1/projects/{}/incidents", api.project());
    for query in ["state=closed", "severity=urgent", "limit=0", "limit=201"] {
        let reply = api.get(&format!("{base}?{query}"));
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        assert_eq!(reply.body["code"], "validation_failed", "{query}");
    }
    let unknown = Uuid::from_u128(5);
    for path in ["incidents", "incident-sources"] {
        let reply = api.get(&format!("/api/v1/projects/{unknown}/{path}"));
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
    }
}

#[test]
fn an_incident_carries_its_fields_and_its_linked_deployment_with_the_range() {
    let api = Api::new();
    let deployment = api.deployment();
    let id = api.open(
        "prod-alerts",
        "fp-1",
        IncidentSeverity::High,
        Some("staging"),
        Some(&deployment),
        9_000,
    );
    let reply = api.get(&format!("/api/v1/incidents/{id}"));
    assert_eq!(reply.status, StatusCode::OK);
    let body = &reply.body;
    assert_eq!(body["id"], id.to_string());
    assert_eq!(body["project_id"], api.project());
    assert_eq!(body["source"], "prod-alerts");
    assert_eq!(body["fingerprint"], "fp-1");
    assert_eq!(body["state"], "open");
    assert_eq!(body["resolution"], Value::Null);
    assert_eq!(body["signal_status"], "firing");
    assert_eq!(body["severity"], "high");
    assert_eq!(body["title"], "<b>fp-1</b> failing");
    assert_eq!(body["environment"], "staging");
    assert_eq!(body["labels"], json!({ "team": "web" }));
    assert_eq!(body["occurrences"], 1);
    assert_eq!(body["started_at"], "1970-01-01T00:00:08.500Z");
    assert_eq!(body["first_seen"], "1970-01-01T00:00:09.000Z");
    assert_eq!(body["deployment"]["id"], deployment.id.to_string());
    assert_eq!(body["deployment"]["state"], "deploying");
    assert_eq!(
        body["range"],
        json!({ "previous_sha": "a".repeat(40), "sha": "b".repeat(40) })
    );
    assert_eq!(body["suspects"], json!([]));
    for field in ["dismiss_reason", "actor", "resolved_at", "url"] {
        assert!(body.get(field).is_some(), "{field}");
    }
}

#[test]
fn an_incident_without_a_deployment_has_no_range_and_an_unknown_one_is_not_found() {
    let api = Api::new();
    let id = api.open(
        "prod-alerts",
        "fp",
        IncidentSeverity::Low,
        None,
        None,
        1_000,
    );
    let reply = api.get(&format!("/api/v1/incidents/{id}"));
    assert_eq!(reply.body["deployment"], Value::Null);
    assert_eq!(reply.body["range"], Value::Null);
    let missing = api.get(&format!("/api/v1/incidents/{}", Uuid::from_u128(9)));
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.body["code"], "not_found");
}
