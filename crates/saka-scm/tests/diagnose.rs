#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use saka_core::time::Timestamp;
use saka_ports::Scm;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

mod support;

use support::*;

const HOUR: i64 = 3_600_000;

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

    let app = w.gh.app_status("github").await.unwrap();
    assert_eq!(app.app_id, APP_ID);
    assert_eq!(app.slug, "saka-dev");
    assert_eq!(app.events, ["pull_request", "check_run"]);

    let installed = w.gh.app_installation(&w.repo).await.unwrap();
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
    let installed = w.gh.app_installation(&w.repo).await.unwrap();
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
    let e = w.gh.app_status("github").await.unwrap_err();
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
    let e = w.gh.app_status("github").await.unwrap_err();
    assert_eq!(
        (e.code.as_str(), &e.params),
        ("scm_unavailable", &json!({ "status": 500 }))
    );
    let e = w.gh.app_installation(&w.repo).await.unwrap_err();
    assert_eq!(
        (e.code.as_str(), &e.params),
        ("scm_unavailable", &json!({ "status": 403 }))
    );
}
