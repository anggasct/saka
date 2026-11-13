#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use saka_scm::github::manifest::{ManifestRequest, manifest};
use serde_json::{Value, json};
use wiremock::matchers::{any, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod support;

use support::{nullable, schema_errors};

const SAKA: &str = env!("CARGO_BIN_EXE_saka");
const CODE: &str = "abc123";
const CLIENT_SECRET: &str = "client-secret-0123456789abcdef0123456789abcdef";
const WEBHOOK_SECRET: &str = "0123456789abcdef0123456789abcdef01234567";
const COMMENTED: &str = "# Saka operator config\n[server]\nlisten = \"127.0.0.1:7400\" # API\n";

struct Env {
    dir: tempfile::TempDir,
    config: PathBuf,
    server: MockServer,
}

impl Env {
    async fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("xdg/saka/config.toml");
        Env {
            dir,
            config,
            server: MockServer::start().await,
        }
    }

    fn write_config(&self, text: &str) {
        std::fs::create_dir_all(self.config.parent().unwrap()).unwrap();
        std::fs::write(&self.config, text).unwrap();
    }

    fn config_text(&self) -> Option<String> {
        std::fs::read_to_string(&self.config).ok()
    }

    fn scm_dir(&self) -> PathBuf {
        self.config.parent().unwrap().join("scm/github")
    }

    fn uri(&self) -> String {
        self.server.uri()
    }

    fn command(&self, args: &[&str], path_env: &str) -> Command {
        let mut c = Command::new(SAKA);
        c.arg("scm")
            .arg("create-app")
            .args(["--web-url", &self.uri(), "--api-url", &self.uri()])
            .args(args)
            .env_clear()
            .env("PATH", path_env)
            .env("HOME", self.dir.path().join("home"))
            .env("SAKA_CONFIG", &self.config)
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args, "/usr/bin:/bin").output().unwrap()
    }

    fn spawn(&self, args: &[&str]) -> Flow {
        self.spawn_with_path(args, "/usr/bin:/bin")
    }

    fn spawn_with_path(&self, args: &[&str], path_env: &str) -> Flow {
        let mut child = self
            .command(args, path_env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let all = Arc::new(Mutex::new(String::new()));
        let (tx, rx) = mpsc::channel();
        let sink = all.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let line = line.unwrap();
                sink.lock().unwrap().push_str(&format!("{line}\n"));
                let _ = tx.send(line);
            }
        });
        let start_url = loop {
            let line = rx
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_else(|_| panic!("no open line: {}", all.lock().unwrap()));
            if let Some(url) = line.strip_prefix("open  ") {
                break url.to_string();
            }
        };
        let port = start_url
            .strip_prefix("http://127.0.0.1:")
            .and_then(|rest| rest.split('/').next())
            .unwrap()
            .parse()
            .unwrap();
        Flow {
            child,
            stderr: all,
            start_url,
            port,
        }
    }
}

struct Flow {
    child: Child,
    stderr: Arc<Mutex<String>>,
    start_url: String,
    port: u16,
}

struct Done {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Flow {
    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    async fn get(&self, path_and_query: &str) -> reqwest::Response {
        client()
            .get(format!("{}{path_and_query}", self.origin()))
            .send()
            .await
            .unwrap()
    }

    async fn start_page(&self) -> StartPage {
        let response = client().get(&self.start_url).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let headers = response.headers().clone();
        let html = response.text().await.unwrap();
        StartPage::parse(&headers, &html)
    }

    async fn callback(&self, state: &str) -> reqwest::Response {
        self.get(&format!("/callback?code={CODE}&state={state}"))
            .await
    }

    fn wait(mut self) -> Done {
        let mut stdout = String::new();
        self.child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        let status = self.child.wait().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let stderr = self.stderr.lock().unwrap().clone();
        Done {
            code: status.code().unwrap_or(-1),
            stdout,
            stderr,
        }
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

struct StartPage {
    csp: String,
    headers: reqwest::header::HeaderMap,
    nonce: String,
    action: String,
    manifest: Value,
    state: String,
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let from = text.find(start).unwrap() + start.len();
    let len = text[from..].find(end).unwrap();
    &text[from..from + len]
}

impl StartPage {
    fn parse(headers: &reqwest::header::HeaderMap, html: &str) -> StartPage {
        let csp = headers["content-security-policy"]
            .to_str()
            .unwrap()
            .to_string();
        let action = unescape(between(html, "action=\"", "\""));
        let manifest =
            serde_json::from_str(&unescape(between(html, "name=\"manifest\" value=\"", "\"")))
                .unwrap();
        let state = reqwest::Url::parse(&action)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        StartPage {
            csp,
            headers: headers.clone(),
            nonce: between(html, "<script nonce=\"", "\"").to_string(),
            action,
            manifest,
            state,
        }
    }
}

fn openssl_pem() -> String {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("app.pem");
    let out = Command::new("openssl")
        .args([
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
        ])
        .arg(&key)
        .output()
        .expect("the openssl CLI is needed to make a test key");
    assert!(out.status.success());
    std::fs::read_to_string(key).unwrap()
}

fn created(pem: &str) -> Value {
    json!({
        "id": 123_456,
        "slug": "saka-3f9a1c",
        "name": "saka-3f9a1c",
        "owner": { "login": "acme" },
        "html_url": "https://github.com/apps/saka-3f9a1c",
        "client_id": "Iv23liAbCdEfGh123456",
        "client_secret": CLIENT_SECRET,
        "webhook_secret": WEBHOOK_SECRET,
        "pem": pem,
        "permissions": { "checks": "read", "contents": "write", "issues": "write", "metadata": "read", "pull_requests": "write", "statuses": "write" },
        "events": ["check_run", "check_suite", "issue_comment", "issues", "pull_request", "status"],
    })
}

async fn conversion(env: &Env, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path(format!("/app-manifests/{CODE}/conversions")))
        .respond_with(response)
        .expect(1)
        .mount(&env.server)
        .await;
}

fn assert_no_secrets(done: &Done, pem: &str, state: &str) {
    for text in [&done.stdout, &done.stderr] {
        assert!(!text.contains("-----BEGIN"), "{text}");
        assert!(!text.contains(pem.trim()), "{text}");
        assert!(!text.contains(CLIENT_SECRET), "{text}");
        assert!(!text.contains(WEBHOOK_SECRET), "{text}");
        assert!(!text.contains(CODE), "{text}");
        assert!(!text.contains(state), "{text}");
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn report_schema() -> Value {
    let note = json!({ "type": "object", "additionalProperties": false, "required": ["code", "params"],
        "properties": { "code": { "type": "string" }, "params": { "type": "object" } } });
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["integration", "app", "client_id", "permissions", "events", "webhook_url", "files", "config", "warnings", "next", "exit_code"],
        "properties": {
            "integration": { "type": "string" },
            "app": { "type": "object", "additionalProperties": false, "required": ["id", "slug", "name", "owner", "html_url", "settings_url"],
                "properties": { "id": { "type": "integer" }, "slug": { "type": "string" }, "name": { "type": "string" },
                    "owner": { "type": "string" }, "html_url": { "type": "string" }, "settings_url": { "type": "string" } } },
            "client_id": { "type": "string" },
            "permissions": { "type": "object" },
            "events": { "type": "array", "items": { "type": "string" } },
            "webhook_url": nullable("string"),
            "files": { "type": "object", "additionalProperties": false, "required": ["private_key", "webhook_secret"],
                "properties": { "private_key": { "type": "string" }, "webhook_secret": nullable("string") } },
            "config": { "type": "object", "additionalProperties": false, "required": ["path", "created", "hooks_added"],
                "properties": { "path": { "type": "string" }, "created": { "type": "boolean" }, "hooks_added": { "type": "boolean" } } },
            "warnings": { "type": "array", "items": note },
            "next": { "type": "array", "items": note },
            "exit_code": { "type": "integer" }
        }
    })
}

fn report(done: &Done) -> Value {
    let value: Value = serde_json::from_str(done.stdout.trim()).unwrap();
    let mut errors = Vec::new();
    schema_errors(&report_schema(), &value, "$", &mut errors);
    assert!(errors.is_empty(), "{errors:#?}\n{value:#}");
    value
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_before_the_listener_touch_nothing_and_leave_the_port_free() {
    let env = Env::new().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&env.server)
        .await;
    let other_scm = format!(
        "[[scm]]\nid = \"other\"\nprovider = \"github\"\napp_id = 1\nprivate_key_file = \"/k\"\nweb_url = \"{0}/\"\napi_url = \"{0}\"\n",
        env.uri()
    );
    let same_id = "[[scm]]\nid = \"github\"\nprovider = \"github\"\napp_id = 1\nweb_url = \"https://ghe.example.com\"\napi_url = \"https://ghe.example.com/api/v3\"\nprivate_key_file = \"/k\"\n";
    let long_name = "a".repeat(35);
    let cases: Vec<(&str, String, Vec<&str>, &str)> = vec![
        ("same id", same_id.into(), vec![], "scm_integration_exists"),
        ("same host", other_scm, vec![], "scm_host_configured"),
        (
            "files exist",
            COMMENTED.into(),
            vec![],
            "scm_app_files_exist",
        ),
        (
            "bad config",
            "[scheduler]\nmax_active_tasks = 99\n".into(),
            vec![],
            "config_invalid",
        ),
        (
            "hooks on the API port",
            COMMENTED.into(),
            vec![
                "--webhook-base-url",
                "https://saka.example.com",
                "--hooks-listen",
                "127.0.0.1:7400",
            ],
            "config_invalid",
        ),
        (
            "http webhook",
            COMMENTED.into(),
            vec!["--webhook-base-url", "http://x"],
            "flag_invalid",
        ),
        (
            "webhook query",
            COMMENTED.into(),
            vec!["--webhook-base-url", "https://x/?a=1"],
            "flag_invalid",
        ),
        (
            "hooks without webhook",
            COMMENTED.into(),
            vec!["--hooks-listen", "127.0.0.1:7421"],
            "flag_invalid",
        ),
        (
            "long name",
            COMMENTED.into(),
            vec!["--name", &long_name],
            "flag_invalid",
        ),
        (
            "bad org",
            COMMENTED.into(),
            vec!["--org=-a"],
            "flag_invalid",
        ),
        (
            "low port",
            COMMENTED.into(),
            vec!["--port", "80"],
            "flag_invalid",
        ),
        (
            "short timeout",
            COMMENTED.into(),
            vec!["--timeout-secs", "4"],
            "flag_invalid",
        ),
    ];
    for (name, config, args, code) in cases {
        env.write_config(&config);
        let _ = std::fs::remove_dir_all(env.config.parent().unwrap().join("scm"));
        if name == "files exist" {
            std::fs::create_dir_all(env.scm_dir()).unwrap();
            std::fs::write(env.scm_dir().join("private-key.pem"), "x").unwrap();
        }
        let port = free_port().to_string();
        let mut all = vec!["--json", "--no-browser"];
        if !args.contains(&"--port") {
            all.extend(["--port", port.as_str()]);
        }
        all.extend(args.iter().copied());
        let out = env.run(&all);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{name}: {stderr}");
        assert!(
            stderr.contains(&format!("error: {code} ")),
            "{name}: {stderr}"
        );
        let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(doc["code"], code, "{name}");
        assert_eq!(doc["exit_code"], 2, "{name}");
        assert_eq!(env.config_text().unwrap(), config, "{name}");
        if name != "files exist" {
            assert!(!env.config.parent().unwrap().join("scm").exists(), "{name}");
        }
        assert!(!stderr.contains("open  "), "{name}: {stderr}");
        std::net::TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
    }

    env.write_config(COMMENTED);
    let _ = std::fs::remove_dir_all(env.config.parent().unwrap().join("scm"));
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let out = env.run(&["--json", "--no-browser", "--port", &port]);
    assert_eq!(out.status.code(), Some(2));
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["code"], "callback_port_unavailable");
    assert_eq!(env.config_text().unwrap(), COMMENTED);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_listener_refuses_everything_but_one_valid_callback_and_the_app_is_saved() {
    let env = Env::new().await;
    env.write_config(COMMENTED);
    let pem = openssl_pem();
    conversion(
        &env,
        ResponseTemplate::new(201).set_body_json(created(&pem)),
    )
    .await;
    let flow = env.spawn(&[
        "--json",
        "--no-browser",
        "--org",
        "acme",
        "--name",
        "saka-3f9a1c",
        "--webhook-base-url",
        "https://saka.example.com/",
    ]);
    assert!(flow.start_url.starts_with("http://127.0.0.1:"));

    let page = flow.start_page().await;
    assert_eq!(page.headers["cache-control"], "no-store");
    assert_eq!(page.headers["referrer-policy"], "no-referrer");
    assert_eq!(page.headers["x-content-type-options"], "nosniff");
    assert_eq!(page.headers["x-frame-options"], "DENY");
    assert!(
        page.csp
            .contains(&format!("script-src 'nonce-{}'", page.nonce)),
        "{}",
        page.csp
    );
    assert!(
        page.csp.contains(&format!("form-action {};", env.uri())),
        "{}",
        page.csp
    );
    assert!(page.csp.contains("default-src 'none'"));
    assert_eq!(page.state.len(), 43);
    assert_eq!(
        page.action,
        format!(
            "{}/organizations/acme/settings/apps/new?state={}",
            env.uri(),
            page.state
        )
    );
    let redirect = format!("{}/callback", flow.origin());
    assert_eq!(
        page.manifest,
        manifest(&ManifestRequest {
            name: "saka-3f9a1c",
            integration_id: "github",
            redirect_url: &redirect,
            webhook_url: Some("https://saka.example.com/hooks/github"),
        })
    );

    assert_eq!(
        client().get(&flow.start_url).send().await.unwrap().status(),
        410
    );
    assert_eq!(flow.get("/start/another-launch").await.status(), 404);
    let evil = client()
        .get(format!("{}/callback", flow.origin()))
        .header("host", format!("evil.example:{}", flow.port))
        .send()
        .await
        .unwrap();
    assert_eq!(evil.status(), 421);
    assert_eq!(
        flow.get(&format!("/callback?code={CODE}")).await.status(),
        403
    );
    assert_eq!(flow.callback("wrong").await.status(), 403);
    assert_eq!(
        flow.get(&format!("/callback?code=a%2Fb&state={}", page.state))
            .await
            .status(),
        400
    );
    assert_eq!(flow.get("/elsewhere").await.status(), 404);

    let done_page = flow.callback(&page.state).await;
    assert_eq!(done_page.status(), 200);
    assert!(
        done_page
            .text()
            .await
            .unwrap()
            .contains("Saka received the GitHub App saka-3f9a1c")
    );
    let port = flow.port;
    let state = page.state.clone();
    let done = flow.wait();
    assert_eq!(done.code, 0, "{}", done.stderr);
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    assert_no_secrets(&done, &pem, &state);

    let dir = env.scm_dir();
    let key = dir.join("private-key.pem");
    let secret = dir.join("webhook-secret");
    assert_eq!(std::fs::read_to_string(&key).unwrap(), pem);
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), WEBHOOK_SECRET);
    assert_eq!(
        (mode(&key), mode(&secret), mode(&dir)),
        (0o600, 0o600, 0o700)
    );
    let expected = format!(
        "{COMMENTED}\n[hooks]\nlisten = \"127.0.0.1:7421\"\n\n[[scm]]\nid = \"github\"\nprovider = \"github\"\nweb_url = \"{uri}\"\napi_url = \"{uri}\"\napp_id = 123456\nprivate_key_file = \"{key}\"\nclient_id = \"Iv23liAbCdEfGh123456\"\nwebhook_secret_file = \"{secret}\"\n",
        uri = env.uri(),
        key = key.display(),
        secret = secret.display(),
    );
    assert_eq!(env.config_text().unwrap(), expected);

    let r = report(&done);
    let settings = format!("{}/organizations/acme/settings/apps/saka-3f9a1c", env.uri());
    assert_eq!(r["integration"], "github");
    assert_eq!(r["app"]["settings_url"], settings);
    assert_eq!(r["app"]["owner"], "acme");
    assert_eq!(r["webhook_url"], "https://saka.example.com/hooks/github");
    assert_eq!(r["files"]["webhook_secret"], secret.display().to_string());
    assert_eq!(
        r["config"],
        json!({ "path": env.config.display().to_string(), "created": false, "hooks_added": true })
    );
    assert_eq!(r["warnings"], json!([]));
    assert_eq!(
        r["next"],
        json!([
            { "code": "install_app", "params": { "url": format!("{}/apps/saka-3f9a1c/installations/new", env.uri()) } },
            { "code": "enable_device_flow", "params": { "url": settings } },
            { "code": "restart_server", "params": { "command": "saka serve" } },
            { "code": "run_doctor", "params": { "command": "saka doctor" } },
        ])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_existing_hooks_table_is_kept_and_the_human_output_lists_the_next_steps() {
    let env = Env::new().await;
    let initial = "[hooks]\nlisten = \"127.0.0.1:7500\"\n\n[[scm]]\nid = \"ghe\"\nprovider = \"github\"\nweb_url = \"https://ghe.example.com\"\napi_url = \"https://ghe.example.com/api/v3\"\napp_id = 2\nprivate_key_file = \"/k\"\nwebhook_secret_file = \"/s\"\n";
    env.write_config(initial);
    let pem = openssl_pem();
    conversion(
        &env,
        ResponseTemplate::new(201).set_body_json(created(&pem)),
    )
    .await;
    let flow = env.spawn(&[
        "--no-browser",
        "--webhook-base-url",
        "https://saka.example.com",
    ]);
    let page = flow.start_page().await;
    assert!(
        page.action
            .starts_with(&format!("{}/settings/apps/new?state=", env.uri()))
    );
    assert_eq!(flow.callback(&page.state).await.status(), 200);
    let done = flow.wait();
    assert_eq!(done.code, 0, "{}", done.stderr);
    assert_no_secrets(&done, &pem, &page.state);
    let config = env.config_text().unwrap();
    assert!(config.starts_with(initial));
    assert_eq!(config.matches("[hooks]").count(), 1);
    let dir = env.scm_dir();
    let lines: Vec<&str> = done.stdout.lines().collect();
    assert_eq!(
        lines,
        [
            "app  saka-3f9a1c 123456 created (owner acme)".to_string(),
            format!(
                "files  {} {}",
                dir.join("private-key.pem").display(),
                dir.join("webhook-secret").display()
            ),
            format!("config  {} [[scm]] github added", env.config.display()),
            format!(
                "next  install {}/apps/saka-3f9a1c/installations/new",
                env.uri()
            ),
            format!(
                "next  enable Device Flow at {}/settings/apps/saka-3f9a1c",
                env.uri()
            ),
            "next  restart saka serve, then run saka doctor".to_string(),
        ]
    );
    assert!(done.stderr.contains("waiting  for GitHub (up to 900 s)"));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_webhook_or_a_config_only_the_integration_is_written() {
    let env = Env::new().await;
    let pem = openssl_pem();
    conversion(
        &env,
        ResponseTemplate::new(201).set_body_json(created(&pem)),
    )
    .await;
    let flow = env.spawn(&["--json", "--no-browser"]);
    let page = flow.start_page().await;
    assert!(page.manifest.get("hook_attributes").is_none());
    assert!(page.manifest.get("default_events").is_none());
    assert_eq!(flow.callback(&page.state).await.status(), 200);
    let done = flow.wait();
    assert_eq!(done.code, 0, "{}", done.stderr);
    let dir = env.scm_dir();
    let key = dir.join("private-key.pem");
    assert!(!dir.join("webhook-secret").exists());
    assert_eq!(mode(&env.config), 0o600);
    assert_eq!(mode(env.config.parent().unwrap()), 0o700);
    assert_eq!(
        env.config_text().unwrap(),
        format!(
            "[[scm]]\nid = \"github\"\nprovider = \"github\"\nweb_url = \"{uri}\"\napi_url = \"{uri}\"\napp_id = 123456\nprivate_key_file = \"{key}\"\nclient_id = \"Iv23liAbCdEfGh123456\"\n",
            uri = env.uri(),
            key = key.display()
        )
    );
    let r = report(&done);
    assert_eq!(r["config"]["created"], true);
    assert_eq!(r["config"]["hooks_added"], false);
    assert_eq!(r["webhook_url"], Value::Null);
    assert_eq!(r["files"]["webhook_secret"], Value::Null);
    assert_no_secrets(&done, &pem, &page.state);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_code_writes_nothing_and_points_to_the_apps_page() {
    let env = Env::new().await;
    env.write_config(COMMENTED);
    conversion(
        &env,
        ResponseTemplate::new(422).set_body_json(json!({ "message": "expired" })),
    )
    .await;
    let flow = env.spawn(&["--json", "--no-browser", "--org", "acme"]);
    let page = flow.start_page().await;
    let answer = flow.callback(&page.state).await;
    assert!(
        answer
            .text()
            .await
            .unwrap()
            .contains("could not finish creating the App (app_manifest_code_invalid)")
    );
    let done = flow.wait();
    assert_eq!(done.code, 1, "{}", done.stderr);
    let doc: Value = serde_json::from_str(done.stdout.trim()).unwrap();
    assert_eq!(doc["code"], "app_manifest_code_invalid");
    assert_eq!(doc["params"]["status"], 422);
    assert_eq!(
        doc["params"]["apps_url"],
        format!("{}/organizations/acme/settings/apps", env.uri())
    );
    assert!(!env.config.parent().unwrap().join("scm").exists());
    assert_eq!(env.config_text().unwrap(), COMMENTED);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_config_changed_while_waiting_is_left_alone_and_the_block_is_printed() {
    let env = Env::new().await;
    env.write_config(COMMENTED);
    let pem = openssl_pem();
    conversion(
        &env,
        ResponseTemplate::new(201).set_body_json(created(&pem)),
    )
    .await;
    let flow = env.spawn(&["--json", "--no-browser"]);
    let page = flow.start_page().await;
    let changed = format!("{COMMENTED}# edited meanwhile\n");
    env.write_config(&changed);
    flow.callback(&page.state).await;
    let done = flow.wait();
    assert_eq!(done.code, 1, "{}", done.stderr);
    let doc: Value = serde_json::from_str(done.stdout.trim()).unwrap();
    assert_eq!(doc["code"], "config_changed_concurrently");
    assert_eq!(doc["params"]["app"]["id"], 123_456);
    assert!(env.scm_dir().join("private-key.pem").exists());
    assert_eq!(env.config_text().unwrap(), changed);
    assert!(done.stderr.contains("[[scm]]"), "{}", done.stderr);
    assert!(done.stderr.contains("app_id = 123456"), "{}", done.stderr);
    assert_no_secrets(&done, &pem, &page.state);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unwritable_key_folder_fails_with_the_app_settings_and_leaves_the_config() {
    let env = Env::new().await;
    env.write_config(COMMENTED);
    std::fs::create_dir_all(env.scm_dir()).unwrap();
    std::fs::set_permissions(env.scm_dir(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let pem = openssl_pem();
    conversion(
        &env,
        ResponseTemplate::new(201).set_body_json(created(&pem)),
    )
    .await;
    let flow = env.spawn(&["--json", "--no-browser"]);
    let page = flow.start_page().await;
    flow.callback(&page.state).await;
    let done = flow.wait();
    std::fs::set_permissions(env.scm_dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(done.code, 1, "{}", done.stderr);
    let doc: Value = serde_json::from_str(done.stdout.trim()).unwrap();
    assert_eq!(doc["code"], "files_write_failed");
    assert_eq!(
        doc["params"]["app"]["settings_url"],
        format!("{}/settings/apps/saka-3f9a1c", env.uri())
    );
    assert_eq!(env.config_text().unwrap(), COMMENTED);
    assert_no_secrets(&done, &pem, &page.state);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_callback_in_time_fails_and_an_interrupt_exits_130_without_files() {
    let env = Env::new().await;
    env.write_config(COMMENTED);
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&env.server)
        .await;
    let flow = env.spawn(&["--json", "--no-browser", "--timeout-secs", "5"]);
    let done = flow.wait();
    assert_eq!(done.code, 1, "{}", done.stderr);
    let doc: Value = serde_json::from_str(done.stdout.trim()).unwrap();
    assert_eq!(doc["code"], "app_manifest_timed_out");
    assert_eq!(doc["params"]["timeout_secs"], 5);
    assert_eq!(
        doc["params"]["apps_url"],
        format!("{}/settings/apps", env.uri())
    );

    let flow = env.spawn(&["--no-browser"]);
    let pid = rustix::process::Pid::from_raw(i32::try_from(flow.child.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();
    let port = flow.port;
    let done = flow.wait();
    assert_eq!(done.code, 130, "{}", done.stderr);
    assert!(done.stderr.contains("error: interrupted"));
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    assert!(!env.config.parent().unwrap().join("scm").exists());
    assert_eq!(env.config_text().unwrap(), COMMENTED);
}

#[tokio::test(flavor = "multi_thread")]
async fn narrower_grants_and_a_missing_webhook_secret_are_warnings() {
    let env = Env::new().await;
    env.write_config(COMMENTED);
    let pem = openssl_pem();
    let mut body = created(&pem);
    body["permissions"]["statuses"] = json!("read");
    body["events"] = json!(["check_run", "check_suite", "issue_comment", "pull_request"]);
    body["webhook_secret"] = Value::Null;
    conversion(&env, ResponseTemplate::new(201).set_body_json(body)).await;
    let flow = env.spawn(&[
        "--json",
        "--no-browser",
        "--webhook-base-url",
        "https://saka.example.com",
    ]);
    let page = flow.start_page().await;
    flow.callback(&page.state).await;
    let done = flow.wait();
    assert_eq!(done.code, 0, "{}", done.stderr);
    let r = report(&done);
    assert_eq!(
        r["warnings"],
        json!([
            { "code": "app_grants_differ", "params": {
                "missing_permissions": [{ "permission": "statuses", "requested": "write", "granted": "read" }],
                "missing_events": ["issues", "status"] } },
            { "code": "app_webhook_secret_missing", "params": {} },
        ])
    );
    assert!(!env.scm_dir().join("webhook-secret").exists());
    let config = env.config_text().unwrap();
    assert!(!config.contains("webhook_secret_file"), "{config}");
    assert!(!config.contains("[hooks]"), "{config}");
    assert_eq!(r["config"]["hooks_added"], false);
}

fn fake_opener(dir: &Path, exit: i32) -> String {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let opener = bin.join("xdg-open");
    std::fs::write(
        &opener,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nexit {exit}\n",
            dir.join("opened").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!("{}:/usr/bin:/bin", bin.display())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_browser_opener_gets_the_start_url_once_and_its_failure_is_a_warning() {
    for exit in [0, 1] {
        let env = Env::new().await;
        env.write_config(COMMENTED);
        let pem = openssl_pem();
        conversion(
            &env,
            ResponseTemplate::new(201).set_body_json(created(&pem)),
        )
        .await;
        let path_env = fake_opener(env.dir.path(), exit);
        let flow = env.spawn_with_path(&["--json"], &path_env);
        let page = flow.start_page().await;
        flow.callback(&page.state).await;
        let start_url = flow.start_url.clone();
        let done = flow.wait();
        assert_eq!(done.code, 0, "{}", done.stderr);
        let opened = std::fs::read_to_string(env.dir.path().join("opened")).unwrap();
        assert_eq!(opened, format!("{start_url}\n"));
        let warnings = report(&done)["warnings"].clone();
        if exit == 0 {
            assert_eq!(warnings, json!([]));
        } else {
            assert_eq!(warnings[0]["code"], "browser_unavailable");
            assert_eq!(warnings[0]["params"]["reason"], "exit 1");
        }
    }
}
