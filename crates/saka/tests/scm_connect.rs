#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SAKA: &str = env!("CARGO_BIN_EXE_saka");
const TOKEN: &str = "glpat-connect-token-0123456789";
const WEB_URL: &str = "https://gitlab.example";
const COMMENTED: &str = "# Saka operator config\n[server]\nlisten = \"127.0.0.1:7400\" # API\n";

struct Env {
    dir: tempfile::TempDir,
    config: PathBuf,
    token: PathBuf,
    server: MockServer,
}

impl Env {
    async fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("gitlab-token");
        std::fs::write(&token, format!("{TOKEN}\n")).unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        let config = dir.path().join("xdg/saka/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, COMMENTED).unwrap();
        Env {
            dir,
            config,
            token,
            server: MockServer::start().await,
        }
    }

    async fn token_answers(&self, status: u16, body: Value) {
        Mock::given(method("GET"))
            .and(path("/personal_access_tokens/self"))
            .and(header("private-token", TOKEN))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&self.server)
            .await;
    }

    async fn valid_token(&self) {
        self.token_answers(
            200,
            json!({
                "name": "saka", "active": true, "revoked": false,
                "scopes": ["api"], "expires_at": null,
            }),
        )
        .await;
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(SAKA)
            .args(["scm", "connect-gitlab"])
            .args(["--token-file", self.token.to_str().unwrap()])
            .args(["--web-url", WEB_URL, "--api-url", &self.server.uri()])
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.dir.path().join("home"))
            .env("SAKA_CONFIG", &self.config)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn config_text(&self) -> String {
        std::fs::read_to_string(&self.config).unwrap()
    }

    async fn calls(&self) -> usize {
        self.server.received_requests().await.unwrap().len()
    }

    fn block(&self, extra: &str) -> String {
        format!(
            "[[scm]]\nid = \"gitlab\"\nprovider = \"gitlab\"\nweb_url = \"{WEB_URL}\"\napi_url = \"{}\"\ntoken_file = \"{}\"\n{extra}",
            self.server.uri(),
            self.token.display()
        )
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test(flavor = "multi_thread")]
async fn a_valid_token_adds_the_block_and_leaves_the_rest_of_the_config_byte_identical() {
    let env = Env::new().await;
    env.valid_token().await;
    let o = env.run(&[]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(env.config_text(), format!("{COMMENTED}\n{}", env.block("")));
    let out = stdout(&o);
    assert!(
        out.contains(&format!(
            "config  {} [[scm]] gitlab added\n",
            env.config.display()
        )),
        "{out}"
    );
    assert!(out.contains("next  add the user of token saka"), "{out}");
    assert!(out.contains("saka doctor --check scm.credential"), "{out}");
    assert!(!out.contains("project webhook"), "{out}");
    assert!(!format!("{out}{}", stderr(&o)).contains(TOKEN));
    assert_eq!(env.calls().await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_webhook_secret_file_is_created_private_and_named_in_the_block() {
    let env = Env::new().await;
    env.valid_token().await;
    let secret = env.dir.path().join("secrets/gitlab-webhook");
    let o = env.run(&["--webhook-secret-file", secret.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let written = std::fs::read_to_string(&secret).unwrap();
    assert_eq!(written.len(), 64);
    assert!(written.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(mode(&secret), 0o600);
    let config = env.config_text();
    let extra = format!("webhook_secret_file = \"{}\"\n", secret.display());
    assert_eq!(
        config,
        format!(
            "{COMMENTED}\n[hooks]\nlisten = \"127.0.0.1:7421\"\n\n{}",
            env.block(&extra)
        )
    );
    let out = stdout(&o);
    assert!(out.contains("[[scm]] gitlab added, [hooks] added"), "{out}");
    assert!(
        out.contains("next  add a project webhook at https://<public host>/hooks/gitlab"),
        "{out}"
    );
    assert!(out.contains("merge_requests_events, note_events"), "{out}");
    assert!(!format!("{out}{}", stderr(&o)).contains(&written));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_webhook_secret_file_is_kept_and_a_loose_one_is_refused() {
    let env = Env::new().await;
    env.valid_token().await;
    let secret = env.dir.path().join("gitlab-webhook");
    std::fs::write(&secret, "already-there\n").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
    let loose = env.run(&["--webhook-secret-file", secret.to_str().unwrap()]);
    assert_eq!(code(&loose), 2, "{}", stderr(&loose));
    assert_eq!(env.config_text(), COMMENTED);
    assert_eq!(env.calls().await, 0);

    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    let kept = env.run(&["--webhook-secret-file", secret.to_str().unwrap()]);
    assert_eq!(code(&kept), 0, "{}", stderr(&kept));
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "already-there\n");
    assert!(env.config_text().contains("webhook_secret_file"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_token_exits_one_and_changes_nothing() {
    let env = Env::new().await;
    env.token_answers(
        200,
        json!({
            "name": "saka", "active": false, "revoked": true,
            "scopes": ["api"], "expires_at": null,
        }),
    )
    .await;
    let secret = env.dir.path().join("gitlab-webhook");
    let o = env.run(&["--webhook-secret-file", secret.to_str().unwrap()]);
    assert_eq!(code(&o), 1, "{}", stderr(&o));
    assert!(
        stderr(&o).contains("error: token_inactive {"),
        "{}",
        stderr(&o)
    );
    assert!(stderr(&o).contains("\"reason\":\"revoked\""));
    assert!(stderr(&o).contains("create_gitlab_token"));
    assert_eq!(env.config_text(), COMMENTED);
    assert!(!secret.exists(), "no secret file before the token is good");
    assert!(!format!("{}{}", stdout(&o), stderr(&o)).contains(TOKEN));
}

#[tokio::test(flavor = "multi_thread")]
async fn every_failing_token_state_exits_one_with_its_code() {
    let cases = [
        (
            401,
            json!({ "message": "401 Unauthorized" }),
            "token_rejected",
        ),
        (
            200,
            json!({
                "name": "saka", "active": true, "revoked": false,
                "scopes": ["read_api"], "expires_at": null,
            }),
            "token_scopes_missing",
        ),
        (500, json!({}), "scm_unavailable"),
    ];
    for (status, body, expected) in cases {
        let env = Env::new().await;
        env.token_answers(status, body).await;
        let o = env.run(&[]);
        assert_eq!(code(&o), 1, "{expected}: {}", stderr(&o));
        assert!(
            stderr(&o).contains(&format!("error: {expected} ")),
            "{expected}: {}",
            stderr(&o)
        );
        assert_eq!(env.config_text(), COMMENTED);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expiring_token_warns_and_is_still_written() {
    let env = Env::new().await;
    let soon = jiff::Timestamp::now()
        .checked_add(jiff::SignedDuration::from_hours(24 * 5))
        .unwrap();
    let day = soon.to_zoned(jiff::tz::TimeZone::UTC).date().to_string();
    env.token_answers(
        200,
        json!({
            "name": "saka", "active": true, "revoked": false,
            "scopes": ["api"], "expires_at": day,
        }),
    )
    .await;
    let o = env.run(&[]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(
        stdout(&o).contains("warning  token_expiring"),
        "{}",
        stdout(&o)
    );
    assert!(env.config_text().contains("[[scm]]"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_file_open_to_others_is_refused_before_the_provider_is_called() {
    let env = Env::new().await;
    env.valid_token().await;
    std::fs::set_permissions(&env.token, std::fs::Permissions::from_mode(0o644)).unwrap();
    let o = env.run(&[]);
    assert_eq!(code(&o), 2, "{}", stderr(&o));
    assert!(
        stderr(&o).contains("config_key_file_mode"),
        "{}",
        stderr(&o)
    );
    assert_eq!(env.calls().await, 0);
    assert_eq!(env.config_text(), COMMENTED);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_id_or_host_and_a_plain_http_web_url_are_refused_before_the_provider() {
    let env = Env::new().await;
    env.valid_token().await;
    assert_eq!(code(&env.run(&[])), 0);
    let added = env.config_text();
    let again = env.run(&[]);
    assert_eq!(code(&again), 2, "{}", stderr(&again));
    assert!(stderr(&again).contains("scm_integration_exists"));
    let other_id = env.run(&["--id", "gitlab-two"]);
    assert_eq!(code(&other_id), 2, "{}", stderr(&other_id));
    assert!(stderr(&other_id).contains("scm_host_configured"));
    assert_eq!(env.calls().await, 1);
    assert_eq!(env.config_text(), added);

    let fresh = Env::new().await;
    fresh.valid_token().await;
    let http = Command::new(SAKA)
        .args(["scm", "connect-gitlab", "--token-file"])
        .arg(&fresh.token)
        .args(["--web-url", "http://gitlab.example"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SAKA_CONFIG", &fresh.config)
        .output()
        .unwrap();
    assert_eq!(code(&http), 2, "{}", stderr(&http));
    assert!(stderr(&http).contains("\"flag\":\"--web-url\""));
    assert_eq!(fresh.calls().await, 0);
    let bad_id = fresh.run(&["--id", "Not Valid"]);
    assert_eq!(code(&bad_id), 2);
    assert_eq!(fresh.config_text(), COMMENTED);
}

#[tokio::test(flavor = "multi_thread")]
async fn json_prints_one_document_without_the_token() {
    let env = Env::new().await;
    env.valid_token().await;
    let o = env.run(&["--json"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let text = stdout(&o);
    let doc: Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(doc["integration"], "gitlab");
    assert_eq!(doc["token"]["name"], "saka");
    assert_eq!(doc["token"]["scopes"], json!(["api"]));
    assert_eq!(doc["config"]["created"], false);
    assert_eq!(doc["files"]["token"], env.token.to_str().unwrap());
    assert_eq!(doc["exit_code"], 0);
    assert!(!format!("{text}{}", stderr(&o)).contains(TOKEN));

    let failing = Env::new().await;
    failing
        .token_answers(401, json!({ "message": "401 Unauthorized" }))
        .await;
    let o = failing.run(&["--json"]);
    assert_eq!(code(&o), 1);
    let doc: Value = serde_json::from_str(stdout(&o).trim()).unwrap();
    assert_eq!(doc["code"], "token_rejected");
    assert_eq!(doc["exit_code"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_config_flag_picks_the_file_to_edit() {
    let env = Env::new().await;
    env.valid_token().await;
    let other = env.dir.path().join("other.toml");
    let o = env.run(&["--config", other.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(std::fs::read_to_string(&other).unwrap(), env.block(""));
    assert_eq!(env.config_text(), COMMENTED);
    assert_eq!(mode(&other), 0o600);
}
