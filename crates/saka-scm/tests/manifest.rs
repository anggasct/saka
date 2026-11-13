#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::collections::BTreeMap;
use std::time::Duration;

use saka_scm::github::manifest::{CONVERSION_TIMEOUT, ManifestError, convert};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

mod support;

const CODE: &str = "abc123";
const CONVERSION_PATH: &str = "/app-manifests/abc123/conversions";
const USER_AGENT: &str = "saka/0.1.0";
const CLIENT_SECRET: &str = "client-secret-0123456789abcdef0123456789abcdef";
const WEBHOOK_SECRET: &str = "0123456789abcdef0123456789abcdef01234567";

struct NoAuthorization;

impl Match for NoAuthorization {
    fn matches(&self, request: &Request) -> bool {
        !request.headers.contains_key("authorization")
    }
}

fn test_pem() -> String {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("app.pem");
    support::openssl(&[
        "genpkey",
        "-algorithm",
        "RSA",
        "-pkeyopt",
        "rsa_keygen_bits:2048",
        "-out",
        key.to_str().unwrap(),
    ]);
    std::fs::read_to_string(key).unwrap()
}

fn created(pem: &str) -> Value {
    json!({
        "id": 123_456,
        "slug": "saka-3f9a1c",
        "name": "saka-3f9a1c",
        "owner": { "login": "acme", "type": "Organization" },
        "html_url": "https://github.com/apps/saka-3f9a1c",
        "client_id": "Iv23liAbCdEfGh123456",
        "client_secret": CLIENT_SECRET,
        "webhook_secret": WEBHOOK_SECRET,
        "pem": pem,
        "permissions": { "checks": "read", "contents": "write", "issues": "write", "metadata": "read", "pull_requests": "write", "statuses": "write" },
        "events": ["check_run", "check_suite", "issue_comment", "issues", "pull_request", "status"],
    })
}

async fn github(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CONVERSION_PATH))
        .and(header("accept", "application/vnd.github+json"))
        .and(header("x-github-api-version", "2026-03-10"))
        .and(header("user-agent", USER_AGENT))
        .and(NoAuthorization)
        .respond_with(response)
        .expect(1)
        .mount(&server)
        .await;
    server
}

async fn run(server: &MockServer) -> Result<saka_scm::github::manifest::CreatedApp, ManifestError> {
    convert(&server.uri(), USER_AGENT, CODE, CONVERSION_TIMEOUT).await
}

#[tokio::test]
async fn a_created_app_is_read_with_every_field() {
    let pem = test_pem();
    let server = github(ResponseTemplate::new(201).set_body_json(created(&pem))).await;
    let app = run(&server).await.unwrap();
    assert_eq!(app.id, 123_456);
    assert_eq!(app.slug, "saka-3f9a1c");
    assert_eq!(app.name, "saka-3f9a1c");
    assert_eq!(app.owner, "acme");
    assert_eq!(app.html_url, "https://github.com/apps/saka-3f9a1c");
    assert_eq!(app.client_id, "Iv23liAbCdEfGh123456");
    assert_eq!(app.pem.as_str(), pem);
    assert_eq!(
        app.webhook_secret.as_ref().map(|s| s.as_str()),
        Some(WEBHOOK_SECRET)
    );
    let permissions: BTreeMap<String, String> = [
        ("checks", "read"),
        ("contents", "write"),
        ("issues", "write"),
        ("metadata", "read"),
        ("pull_requests", "write"),
        ("statuses", "write"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(app.permissions, permissions);
    assert_eq!(
        app.events,
        [
            "check_run",
            "check_suite",
            "issue_comment",
            "issues",
            "pull_request",
            "status"
        ]
    );
}

#[tokio::test]
async fn the_debug_form_of_a_created_app_hides_its_secrets() {
    let pem = test_pem();
    let server = github(ResponseTemplate::new(201).set_body_json(created(&pem))).await;
    let shown = format!("{:?}", run(&server).await.unwrap());
    assert!(!shown.contains("-----BEGIN"));
    assert!(!shown.contains(WEBHOOK_SECRET));
    assert!(!shown.contains(CLIENT_SECRET));
    assert!(shown.contains("saka-3f9a1c"));
}

#[tokio::test]
async fn a_used_or_expired_code_is_invalid() {
    for status in [404, 422] {
        let server =
            github(ResponseTemplate::new(status).set_body_json(json!({"message": "x"}))).await;
        let err = run(&server).await.unwrap_err();
        assert_eq!(err, ManifestError::CodeInvalid { status });
        assert_eq!(err.code(), "app_manifest_code_invalid");
        assert_eq!(Value::Object(err.params()), json!({ "status": status }));
    }
}

#[tokio::test]
async fn a_server_error_is_unavailable_and_not_retried() {
    let server = github(ResponseTemplate::new(500)).await;
    let err = run(&server).await.unwrap_err();
    assert_eq!(err.code(), "scm_unavailable");
    assert_eq!(Value::Object(err.params()), json!({ "status": 500 }));
}

#[tokio::test]
async fn a_slow_response_times_out_as_unavailable_and_is_not_retried() {
    let pem = test_pem();
    let server = github(
        ResponseTemplate::new(201)
            .set_body_json(created(&pem))
            .set_delay(Duration::from_secs(3)),
    )
    .await;
    let err = convert(&server.uri(), USER_AGENT, CODE, Duration::from_millis(300))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "scm_unavailable");
    assert_eq!(Value::Object(err.params()), json!({ "reason": "timeout" }));
}

#[tokio::test]
async fn a_pem_that_is_not_an_rsa_key_is_an_invalid_response() {
    let body = created("not a key");
    let server = github(ResponseTemplate::new(201).set_body_json(body)).await;
    let err = run(&server).await.unwrap_err();
    assert_eq!(err.code(), "app_manifest_response_invalid");
    assert_eq!(Value::Object(err.params()), json!({ "field": "pem" }));
    assert_eq!(err.created(), Some((123_456, "saka-3f9a1c")));
}

#[tokio::test]
async fn a_client_id_outside_the_allowed_characters_is_an_invalid_response() {
    let mut body = created(&test_pem());
    body["client_id"] = json!("a b");
    let server = github(ResponseTemplate::new(201).set_body_json(body)).await;
    let err = run(&server).await.unwrap_err();
    assert_eq!(Value::Object(err.params()), json!({ "field": "client_id" }));
}

#[tokio::test]
async fn a_missing_app_id_is_an_invalid_response_without_a_known_app() {
    let mut body = created(&test_pem());
    body["id"] = json!(0);
    let server = github(ResponseTemplate::new(201).set_body_json(body)).await;
    let err = run(&server).await.unwrap_err();
    assert_eq!(Value::Object(err.params()), json!({ "field": "id" }));
    assert_eq!(err.created(), None);
}

#[tokio::test]
async fn a_null_webhook_secret_is_accepted() {
    let mut body = created(&test_pem());
    body["webhook_secret"] = Value::Null;
    let server = github(ResponseTemplate::new(201).set_body_json(body)).await;
    assert!(run(&server).await.unwrap().webhook_secret.is_none());
}

#[tokio::test]
async fn a_webhook_secret_too_short_to_trust_is_dropped() {
    let mut body = created(&test_pem());
    body["webhook_secret"] = json!("short");
    let server = github(ResponseTemplate::new(201).set_body_json(body)).await;
    assert!(run(&server).await.unwrap().webhook_secret.is_none());
}
