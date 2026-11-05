#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SAKA: &str = env!("CARGO_BIN_EXE_saka");
const TOKEN: &str = "424242:SECRET-token-value";

fn config(dir: &Path, api_url: &str) -> std::path::PathBuf {
    let token = dir.join("ops.token");
    std::fs::write(&token, format!("{TOKEN}\n")).unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let file = dir.join("config.toml");
    std::fs::write(
        &file,
        format!(
            "[notify]\npublic_url = \"https://saka.example.com\"\n\n[[notify.channel]]\nname = \"ops\"\nkind = \"telegram\"\ntoken_file = {:?}\nchat = \"-1001\"\napi_url = {api_url:?}\n",
            token.display().to_string()
        ),
    )
    .unwrap();
    file
}

async fn saka(config: &Path, args: &[&str]) -> Output {
    let config = config.to_path_buf();
    let args: Vec<String> = args.iter().map(ToString::to_string).collect();
    tokio::task::spawn_blocking(move || {
        Command::new(SAKA)
            .args(args)
            .env("SAKA_CONFIG", config)
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

fn all_output(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_test_message_reaches_the_channel_and_the_token_stays_out_of_the_output() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("/bot{TOKEN}/sendMessage")))
        .and(body_partial_json(
            json!({ "chat_id": -1001, "text": "Saka: test message" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
        .expect(2)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &server.uri());
    let sent = saka(&cfg, &["notify", "test", "ops"]).await;
    assert_eq!(sent.status.code(), Some(0), "{}", all_output(&sent));
    assert!(String::from_utf8_lossy(&sent.stdout).contains("Sent a test message to ops."));
    assert!(!all_output(&sent).contains("SECRET"));
    let as_json = saka(&cfg, &["--json", "notify", "test", "ops"]).await;
    assert_eq!(as_json.status.code(), Some(0));
    let body: serde_json::Value = serde_json::from_slice(&as_json.stdout).unwrap();
    assert_eq!(body, json!({ "channel": "ops", "sent": true }));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_channel_and_a_refusing_platform_have_their_own_exit_codes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string(format!("bad token {TOKEN}")))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &server.uri());

    let unknown = saka(&cfg, &["notify", "test", "elsewhere"]).await;
    assert_eq!(unknown.status.code(), Some(2));
    assert!(all_output(&unknown).contains("notify_channel_unconfigured"));

    let refused = saka(&cfg, &["notify", "test", "ops"]).await;
    assert_eq!(refused.status.code(), Some(1));
    let text = all_output(&refused);
    assert!(text.contains("notify_channel_rejected"), "{text}");
    assert!(text.contains("401"));
    assert!(!text.contains("SECRET"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_file_anyone_can_read_is_refused() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), &server.uri());
    std::fs::set_permissions(
        dir.path().join("ops.token"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let out = saka(&cfg, &["notify", "test", "ops"]).await;
    assert_eq!(out.status.code(), Some(2));
    assert!(all_output(&out).contains("config_key_file_mode"));
    assert!(server.received_requests().await.unwrap().is_empty());
}
