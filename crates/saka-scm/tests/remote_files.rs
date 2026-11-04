#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use saka_ports::Scm;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

mod support;

use support::*;

const SHA: &str = "9f1c0000000000000000000000000000000000aa";

#[tokio::test]
async fn branch_head_reads_the_tip_with_the_installation_token() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/branches/release%2F1.2"))
        .and(header("authorization", "Bearer ghs_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "release/1.2", "commit": { "sha": SHA }
        })))
        .expect(1)
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/branches/nope"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({ "message": "Branch not found" })),
        )
        .mount(&w.server)
        .await;
    assert_eq!(
        w.gh.branch_head("github", "acme/app", "release/1.2")
            .await
            .unwrap()
            .as_deref(),
        Some(SHA)
    );
    assert_eq!(
        w.gh.branch_head("github", "acme/app", "nope")
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn file_at_reads_raw_bytes_at_a_commit() {
    let w = World::new().await;
    w.mount_tokens().await;
    let toml = "[project]\nname = \"shop\"\n";
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/contents/docs/saka.toml"))
        .and(query_param("ref", SHA))
        .and(header("accept", "application/vnd.github.raw"))
        .and(header("authorization", "Bearer ghs_1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(toml))
        .expect(1)
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/contents/missing/saka.toml"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "Not Found" })))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/contents/big/saka.toml"))
        .respond_with(ResponseTemplate::new(200).set_body_string("#".repeat(65_537)))
        .mount(&w.server)
        .await;
    assert_eq!(
        w.gh.file_at("github", "acme/app", SHA, "docs/saka.toml", 65_536)
            .await
            .unwrap(),
        Some(toml.as_bytes().to_vec())
    );
    assert_eq!(
        w.gh.file_at("github", "acme/app", SHA, "missing/saka.toml", 65_536)
            .await
            .unwrap(),
        None
    );
    let error =
        w.gh.file_at("github", "acme/app", SHA, "big/saka.toml", 65_536)
            .await
            .unwrap_err();
    assert_eq!(error.code, "file_too_large");
    assert!(!error.retryable);
}

#[tokio::test]
async fn provider_failures_while_reading_the_remote_are_unavailable() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/branches/main"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/contents/saka.toml"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&w.server)
        .await;
    let error =
        w.gh.branch_head("github", "acme/app", "main")
            .await
            .unwrap_err();
    assert_eq!(error.code, "scm_unavailable");
    assert!(error.retryable);
    let error =
        w.gh.file_at("github", "acme/app", SHA, "saka.toml", 65_536)
            .await
            .unwrap_err();
    assert_eq!(error.code, "scm_unavailable");
}
