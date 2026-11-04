#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use saka_core::vocab::Tool;
use saka_env::{HttpReleaseIndex, ReleaseIndexConfig};
use saka_ports::{Release, ReleaseIndex};
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn index(server: &MockServer) -> HttpReleaseIndex {
    HttpReleaseIndex::new(ReleaseIndexConfig {
        go_dl_url: format!("{}/dl/", server.uri()),
        node_dist_url: format!("{}/dist", server.uri()),
        npm_registry_url: format!("{}/npm", server.uri()),
        user_agent: "saka-test".into(),
    })
    .unwrap()
}

fn release(version: &str, lts: Option<&str>) -> Release {
    Release {
        version: version.into(),
        lts: lts.map(String::from),
    }
}

#[tokio::test]
async fn go_keeps_only_stable_releases_and_drops_the_go_prefix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/dl/"))
        .and(query_param("mode", "json"))
        .and(query_param("include", "all"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "version": "go1.26rc1", "stable": false, "files": [] },
            { "version": "go1.25.4", "stable": true, "files": [] },
            { "version": "go1.20", "stable": true, "files": [] },
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let index = index(&server);
    let want = vec![release("1.25.4", None), release("1.20", None)];
    assert_eq!(index.releases(Tool::Go).await.unwrap(), want);
    assert_eq!(index.releases(Tool::Go).await.unwrap(), want);
}

#[tokio::test]
async fn node_reads_the_lts_codename_or_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/dist/index.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "version": "v25.1.0", "lts": false, "files": [] },
            { "version": "v24.11.0", "lts": "Krypton", "files": [] },
        ])))
        .mount(&server)
        .await;
    assert_eq!(
        index(&server).releases(Tool::Node).await.unwrap(),
        [release("25.1.0", None), release("24.11.0", Some("Krypton"))]
    );
}

#[tokio::test]
async fn pnpm_reads_the_abbreviated_document_without_prereleases() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/npm/pnpm"))
        .and(header("accept", "application/vnd.npm.install-v1+json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "pnpm",
            "dist-tags": { "latest": "9.15.9" },
            "versions": {
                "9.15.9": { "dist": {} },
                "10.0.0-rc.1": { "dist": {} },
                "9.0.0": { "dist": {} },
            }
        })))
        .mount(&server)
        .await;
    let mut versions: Vec<String> = index(&server)
        .releases(Tool::Pnpm)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.version)
        .collect();
    versions.sort();
    assert_eq!(versions, ["9.0.0", "9.15.9"]);
}

#[tokio::test]
async fn a_failing_index_is_a_retryable_error_and_is_not_cached() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/dist/index.json"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/dist/index.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    let index = index(&server);
    let error = index.releases(Tool::Node).await.unwrap_err();
    assert_eq!(error.code, "release_index_unavailable");
    assert!(error.retryable);
    assert_eq!(error.params["detail"], "HTTP 500");
    assert_eq!(index.releases(Tool::Node).await.unwrap(), []);
}

#[tokio::test]
async fn a_malformed_index_is_unavailable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/npm/pnpm"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
        .mount(&server)
        .await;
    let error = index(&server).releases(Tool::Pnpm).await.unwrap_err();
    assert_eq!(error.code, "release_index_unavailable");
}
