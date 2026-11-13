#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use saka_core::diagnostics::{AppStatus, CredentialStatus, Installation, RepoGrant};
use saka_core::time::Timestamp;
use saka_ports::Scm;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

mod support;

use support::*;

const HOUR: i64 = 3_600_000;

fn app_of(status: CredentialStatus) -> AppStatus {
    match status {
        CredentialStatus::App(app) => app,
        other => panic!("not an app credential: {other:?}"),
    }
}

fn installation_of(grant: RepoGrant) -> Installation {
    match grant {
        RepoGrant::Installation(found) => found,
        other => panic!("not an installation: {other:?}"),
    }
}

async fn mount(w: &World, at: &str, status: u16, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(at))
        .and(w.jwt())
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&w.server)
        .await;
}

async fn assert_only_app_jwt_calls(w: &World) {
    let requests = w.server.received_requests().await.unwrap();
    assert!(!requests.is_empty());
    for r in &requests {
        let auth = r.headers.get("authorization").unwrap().to_str().unwrap();
        assert!(auth.starts_with("Bearer ey"), "{} {auth}", r.url);
        assert!(!auth.contains("ghs_"), "{}", r.url);
        assert!(
            !r.url.path().ends_with("/access_tokens"),
            "an installation token was minted: {}",
            r.url
        );
    }
}

#[tokio::test]
async fn the_app_installation_and_webhook_are_read_with_the_app_jwt() {
    let w = World::new().await;
    mount(
        &w,
        "/app",
        200,
        json!({ "id": APP_ID, "slug": "saka-dev", "events": ["pull_request", "check_run"] }),
    )
    .await;
    mount(
        &w,
        "/repos/acme/app/installation",
        200,
        json!({ "id": 42, "permissions": { "contents": "write", "pull_requests": "read", "metadata": "read" } }),
    )
    .await;
    mount(
        &w,
        "/app/hook/config",
        200,
        json!({ "url": "https://saka.example/hooks/github", "content_type": "json", "insecure_ssl": "0" }),
    )
    .await;
    let at = |ms: i64| rfc3339(NOW_MS - ms);
    Mock::given(method("GET"))
        .and(path("/app/hook/deliveries"))
        .and(query_param("per_page", "100"))
        .and(w.jwt())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 3, "delivered_at": at(HOUR), "status_code": 500, "event": "status" },
            { "id": 2, "delivered_at": at(23 * HOUR), "status_code": 200, "event": "pull_request" },
            { "id": 1, "delivered_at": at(25 * HOUR), "status_code": 502, "event": "check_run" },
        ])))
        .mount(&w.server)
        .await;

    let app = app_of(w.gh.credential_status("github").await.unwrap());
    assert_eq!(app.app_id, APP_ID);
    assert_eq!(app.slug, "saka-dev");
    assert_eq!(app.events, ["pull_request", "check_run"]);

    let installed = installation_of(w.gh.repo_grant(&w.repo).await.unwrap());
    assert!(installed.installed);
    assert_eq!(installed.installation_id, Some(42));
    assert_eq!(
        installed
            .permissions
            .get("pull_requests")
            .map(String::as_str),
        Some("read")
    );
    assert_eq!(installed.permissions.len(), 3);

    let hook = w.gh.webhook_status("github").await.unwrap();
    assert!(hook.url_set);
    assert!(!hook.secret_set, "a config without `secret` has none");
    assert_eq!(hook.content_type.as_deref(), Some("json"));
    let seen: Vec<(Timestamp, u16)> = hook
        .deliveries
        .iter()
        .map(|d| (d.delivered_at, d.status_code))
        .collect();
    assert_eq!(
        seen,
        [
            (Timestamp(NOW_MS - HOUR), 500),
            (Timestamp(NOW_MS - 23 * HOUR), 200)
        ],
        "only the last 24 hours by the server clock"
    );
    assert_only_app_jwt_calls(&w).await;
}

#[tokio::test]
async fn a_missing_installation_and_a_masked_secret_are_read_as_such() {
    let w = World::new().await;
    mount(
        &w,
        "/repos/acme/app/installation",
        404,
        json!({ "message": "Not Found" }),
    )
    .await;
    mount(
        &w,
        "/app/hook/config",
        200,
        json!({ "url": "https://saka.example/hooks/github", "content_type": "form", "secret": "********" }),
    )
    .await;
    mount(&w, "/app/hook/deliveries", 200, json!([])).await;
    let installed = installation_of(w.gh.repo_grant(&w.repo).await.unwrap());
    assert!(!installed.installed);
    assert_eq!(installed.installation_id, None);
    let hook = w.gh.webhook_status("github").await.unwrap();
    assert!(hook.secret_set);
    assert_eq!(hook.content_type.as_deref(), Some("form"));
    assert!(hook.deliveries.is_empty());
    assert_only_app_jwt_calls(&w).await;
}

#[tokio::test]
async fn a_rejected_jwt_is_app_auth_failed_with_the_key_path_and_never_the_key() {
    let w = World::new().await;
    mount(
        &w,
        "/app",
        401,
        json!({ "message": "A JSON web token could not be decoded" }),
    )
    .await;
    let e = w.gh.credential_status("github").await.unwrap_err();
    assert_eq!(e.code, "app_auth_failed");
    assert_eq!(e.params["status"], 401);
    assert_eq!(e.params["app_id"], APP_ID);
    let key_file = e.params["key_file"].as_str().unwrap();
    assert!(key_file.ends_with("app.pem"), "{key_file}");
    assert!(!e.params.to_string().contains("BEGIN"), "{}", e.params);
}

#[tokio::test]
async fn other_provider_failures_are_scm_unavailable_with_the_status() {
    let w = World::new().await;
    mount(&w, "/app", 500, json!({})).await;
    mount(
        &w,
        "/repos/acme/app/installation",
        403,
        json!({ "message": "Forbidden" }),
    )
    .await;
    let e = w.gh.credential_status("github").await.unwrap_err();
    assert_eq!(
        (e.code.as_str(), &e.params),
        ("scm_unavailable", &json!({ "status": 500 }))
    );
    let e = w.gh.repo_grant(&w.repo).await.unwrap_err();
    assert_eq!(
        (e.code.as_str(), &e.params),
        ("scm_unavailable", &json!({ "status": 403 }))
    );
}

fn app_requirements() -> saka_core::diagnostics::AppRequirements {
    match saka_scm::github::requirements::APP {
        saka_core::diagnostics::ScmDiagnostics::App(requirements) => requirements,
        saka_core::diagnostics::ScmDiagnostics::Token(_) => panic!("GitHub declares an app"),
    }
}

#[test]
fn an_installation_without_the_issues_permission_fails_the_repo_access_check() {
    let granted = |pairs: &[(&str, &str)]| Installation {
        installed: true,
        installation_id: Some(42),
        permissions: pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    };
    let without = granted(&[
        ("contents", "write"),
        ("pull_requests", "write"),
        ("checks", "read"),
        ("statuses", "write"),
        ("metadata", "read"),
    ]);
    let finding = saka_core::diagnostics::installation(
        &app_requirements(),
        "acme/app",
        "https://github.example",
        "saka-dev",
        &without,
    );
    assert_eq!(finding.code, "app_permissions_missing");
    assert_eq!(
        finding.params,
        json!({ "missing": [{ "permission": "issues", "required": "write", "granted": null }] })
    );
    let with = granted(&[
        ("actions", "write"),
        ("contents", "write"),
        ("issues", "write"),
        ("pull_requests", "write"),
        ("checks", "read"),
        ("statuses", "write"),
        ("metadata", "read"),
    ]);
    let finding = saka_core::diagnostics::installation(
        &app_requirements(),
        "acme/app",
        "https://github.example",
        "saka-dev",
        &with,
    );
    assert_eq!(finding.code, "app_installed");
}

#[test]
fn an_installation_without_the_actions_permission_only_warns() {
    let installation = Installation {
        installed: true,
        installation_id: Some(42),
        permissions: [
            ("contents", "write"),
            ("issues", "write"),
            ("pull_requests", "write"),
            ("checks", "read"),
            ("statuses", "write"),
            ("metadata", "read"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect(),
    };
    let finding = saka_core::diagnostics::installation(
        &app_requirements(),
        "acme/app",
        "https://github.example",
        "saka-dev",
        &installation,
    );
    assert_eq!(finding.status.as_str(), "warn");
    assert_eq!(finding.code, "app_permission_recommended");
    assert_eq!(
        finding.params,
        json!({ "missing": [{ "permission": "actions", "required": "write", "granted": null }] })
    );
}

#[test]
fn an_app_without_the_issues_event_is_a_webhook_warning() {
    let hook = saka_core::diagnostics::WebhookStatus {
        url_set: true,
        content_type: Some("json".into()),
        secret_set: true,
        deliveries: Vec::new(),
    };
    let app = |events: &[&str]| saka_core::diagnostics::AppStatus {
        app_id: APP_ID,
        slug: "saka-dev".into(),
        events: events.iter().map(|e| (*e).to_string()).collect(),
    };
    let findings = saka_core::diagnostics::webhook(
        &app_requirements(),
        "github",
        &app(&[
            "pull_request",
            "issue_comment",
            "check_run",
            "check_suite",
            "status",
        ]),
        &hook,
        None,
    );
    assert_eq!(findings[0].code, "app_events_missing");
    assert_eq!(findings[0].params, json!({ "missing": ["issues"] }));
}
