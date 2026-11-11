mod browser;
mod callback;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Url;
use saka_core::diagnostics::install_url;
use saka_ports::BestEffort;
use saka_scm::github::manifest::{
    self, CONVERSION_TIMEOUT, CreatedApp, ManifestError, ManifestRequest,
};
use saka_scm::github::requirements::{
    manifest_events, manifest_permissions, missing_events, missing_permissions,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use zeroize::Zeroizing;

use crate::cli;
use crate::scm_config::{
    self, ConfigBlock, Credentials, DEFAULT_HOOKS_LISTEN, Failure, OperatorConfig,
    PRIVATE_KEY_FILE, WEBHOOK_SECRET_FILE, WriteError, id_ok, path_text, print_failure,
    provider_url,
};
use crate::serve::VERSION;

const EXIT_INTERRUPTED: u8 = 130;
const DEFAULT_ID: &str = "github";
const DEFAULT_WEB_URL: &str = "https://github.com";
const DEFAULT_API_URL: &str = "https://api.github.com";
const MAX_NAME_CHARS: usize = 34;
const MAX_ORG_LEN: usize = 39;
const TIMEOUTS_SECS: std::ops::RangeInclusive<u64> = 5..=3300;
const PORTS: std::ops::RangeInclusive<u32> = 1024..=65535;
const TOKEN_BYTES: usize = 32;
const PLACEHOLDER_APP_ID: i64 = 1;
const PLACEHOLDER_CLIENT_ID: &str = "Iv1.0000000000000000";
const SERVER_DRAIN: Duration = Duration::from_secs(5);

#[derive(clap::Args)]
pub struct CreateAppArgs {
    #[arg(
        help = "The `[[scm]].id` to add",
        long,
        default_value = DEFAULT_ID
    )]
    id: String,
    #[arg(
        help = "Create the App in this organization (default: your personal account)",
        long
    )]
    org: Option<String>,
    #[arg(help = "The App name (default: saka-<6 random hex>)", long)]
    name: Option<String>,
    #[arg(
        help = "Public https base URL of the webhook listener; the App sends events to <base>/hooks/<id>",
        long
    )]
    webhook_base_url: Option<String>,
    #[arg(
        help = "`[hooks] listen` to add when the config has no [hooks] (default: 127.0.0.1:7421)",
        long
    )]
    hooks_listen: Option<String>,
    #[arg(
        help = "The provider web URL, for GitHub Enterprise Server",
        long,
        default_value = DEFAULT_WEB_URL
    )]
    web_url: String,
    #[arg(
        help = "The provider API URL (default: https://api.github.com, or <web-url>/api/v3)",
        long
    )]
    api_url: Option<String>,
    #[arg(
        help = "Port of the callback listener on 127.0.0.1 (0: random); fixed ports suit an SSH tunnel",
        long,
        default_value_t = 0
    )]
    port: u32,
    #[arg(
        help = "How long to wait for GitHub, in seconds",
        long,
        default_value_t = 900
    )]
    timeout_secs: u64,
    #[arg(help = "Print the URL to open instead of starting a browser", long)]
    no_browser: bool,
}

struct Plan {
    id: String,
    org: Option<String>,
    name: String,
    webhook_url: Option<String>,
    hooks_listen: String,
    web_url: String,
    api_url: String,
    port: u16,
    timeout_secs: u64,
    open_browser: bool,
}

impl Plan {
    fn web_url_in_config(&self) -> Option<&str> {
        (self.web_url != DEFAULT_WEB_URL).then_some(self.web_url.as_str())
    }

    fn api_url_in_config(&self) -> Option<&str> {
        (self.api_url != DEFAULT_API_URL).then_some(self.api_url.as_str())
    }

    fn apps_url(&self) -> String {
        manifest::apps_url(&self.web_url, self.org.as_deref())
    }

    fn settings_url(&self, slug: &str) -> String {
        manifest::settings_url(&self.web_url, self.org.as_deref(), slug)
    }

    fn known_app(&self, id: u64, slug: &str) -> Value {
        json!({ "id": id, "slug": slug, "settings_url": self.settings_url(slug) })
    }
}

fn random_bytes<const N: usize>() -> Result<[u8; N], Failure> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|_| Failure::refused("random_unavailable", json!({})))?;
    Ok(bytes)
}

fn token() -> Result<Zeroizing<String>, Failure> {
    Ok(Zeroizing::new(
        URL_SAFE_NO_PAD.encode(random_bytes::<TOKEN_BYTES>()?),
    ))
}

fn org_ok(org: &str) -> bool {
    let bytes = org.as_bytes();
    (1..=MAX_ORG_LEN).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

fn name_ok(name: &str) -> bool {
    (1..=MAX_NAME_CHARS).contains(&name.chars().count())
        && !name.chars().any(char::is_control)
        && !name.trim().is_empty()
}

fn webhook_base(raw: &str) -> Result<String, &'static str> {
    let url = Url::parse(raw).map_err(|_| "not_a_url")?;
    if url.scheme() != "https" {
        return Err("not_https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("has_userinfo");
    }
    if url.query().is_some() {
        return Err("has_query");
    }
    if url.fragment().is_some() {
        return Err("has_fragment");
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

fn plan(args: &CreateAppArgs) -> Result<Plan, Failure> {
    if !id_ok(&args.id) {
        return Err(Failure::flag("--id", "pattern"));
    }
    if args.org.as_deref().is_some_and(|o| !org_ok(o)) {
        return Err(Failure::flag("--org", "pattern"));
    }
    let name = match &args.name {
        Some(name) if !name_ok(name) => {
            return Err(Failure::flag("--name", "length_or_characters"));
        }
        Some(name) => name.clone(),
        None => {
            let suffix: String = random_bytes::<3>()?
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("saka-{suffix}")
        }
    };
    let webhook_url = args
        .webhook_base_url
        .as_deref()
        .map(|raw| {
            webhook_base(raw)
                .map(|base| format!("{base}/hooks/{}", args.id))
                .map_err(|reason| Failure::flag("--webhook-base-url", reason))
        })
        .transpose()?;
    let hooks_listen = match (&args.hooks_listen, &webhook_url) {
        (Some(_), None) => {
            return Err(Failure::flag("--hooks-listen", "needs_webhook_base_url"));
        }
        (Some(raw), Some(_)) => {
            raw.parse::<SocketAddr>()
                .map_err(|_| Failure::flag("--hooks-listen", "not_a_socket_address"))?;
            raw.clone()
        }
        (None, _) => DEFAULT_HOOKS_LISTEN.to_string(),
    };
    let web_url = provider_url("--web-url", &args.web_url)?;
    let api_url = match &args.api_url {
        Some(raw) => provider_url("--api-url", raw)?,
        None if web_url == DEFAULT_WEB_URL => DEFAULT_API_URL.to_string(),
        None => format!("{web_url}/api/v3"),
    };
    let port = match args.port {
        0 => 0,
        p if PORTS.contains(&p) => {
            u16::try_from(p).map_err(|_| Failure::flag("--port", "range"))?
        }
        _ => return Err(Failure::flag("--port", "range")),
    };
    if !TIMEOUTS_SECS.contains(&args.timeout_secs) {
        return Err(Failure::flag("--timeout-secs", "range"));
    }
    Ok(Plan {
        id: args.id.clone(),
        org: args.org.clone(),
        name,
        webhook_url,
        hooks_listen,
        web_url,
        api_url,
        port,
        timeout_secs: args.timeout_secs,
        open_browser: !args.no_browser,
    })
}

struct Prepared {
    config: OperatorConfig,
    key_dir: PathBuf,
    add_hooks: bool,
}

impl Prepared {
    fn key_path(&self) -> PathBuf {
        self.key_dir.join(PRIVATE_KEY_FILE)
    }

    fn secret_path(&self) -> PathBuf {
        self.key_dir.join(WEBHOOK_SECRET_FILE)
    }
}

fn block<'a>(
    plan: &'a Plan,
    prepared: &'a Prepared,
    key: &'a str,
    secret: Option<&'a str>,
    app_id: i64,
    client_id: &'a str,
) -> ConfigBlock<'a> {
    ConfigBlock {
        id: &plan.id,
        web_url: plan.web_url_in_config(),
        api_url: plan.api_url_in_config(),
        credentials: Credentials::App {
            app_id,
            private_key_file: key,
            client_id,
        },
        webhook_secret_file: secret,
        hooks_listen: (prepared.add_hooks && secret.is_some())
            .then_some(plan.hooks_listen.as_str()),
    }
}

fn preflight(plan: &Plan) -> Result<Prepared, Failure> {
    let config = OperatorConfig::load(None)?;
    config.check_free(&plan.id, &plan.web_url)?;
    let prepared = Prepared {
        key_dir: config.dir()?.join("scm").join(&plan.id),
        add_hooks: plan.webhook_url.is_some() && !config.has_hooks(),
        config,
    };
    let existing: Vec<String> = [prepared.key_path(), prepared.secret_path()]
        .iter()
        .filter(|p| std::fs::symlink_metadata(p).is_ok())
        .map(|p| p.display().to_string())
        .collect();
    if !existing.is_empty() {
        return Err(Failure::refused(
            "scm_app_files_exist",
            json!({ "paths": existing }),
        ));
    }
    let key = prepared.key_path();
    let secret = prepared.secret_path();
    let secret = plan
        .webhook_url
        .as_ref()
        .map(|_| path_text(&secret))
        .transpose()?;
    let trial = block(
        plan,
        &prepared,
        path_text(&key)?,
        secret,
        PLACEHOLDER_APP_ID,
        PLACEHOLDER_CLIENT_ID,
    );
    prepared
        .config
        .check_block(&scm_config::block_text(&trial))?;
    Ok(prepared)
}

#[derive(Serialize)]
struct AppView {
    id: u64,
    slug: String,
    name: String,
    owner: String,
    html_url: String,
    settings_url: String,
}

#[derive(Serialize)]
struct FilesView {
    private_key: String,
    webhook_secret: Option<String>,
}

#[derive(Serialize)]
struct ConfigView {
    path: String,
    created: bool,
    hooks_added: bool,
}

#[derive(Serialize)]
struct Note {
    code: &'static str,
    params: Value,
}

#[derive(Serialize)]
struct ScmAppReport {
    integration: String,
    app: AppView,
    client_id: String,
    permissions: std::collections::BTreeMap<String, String>,
    events: Vec<String>,
    webhook_url: Option<String>,
    files: FilesView,
    config: ConfigView,
    warnings: Vec<Note>,
    next: Vec<Note>,
    exit_code: u8,
}

fn grant_warnings(app: &CreatedApp, plan: &Plan) -> Vec<Note> {
    let permissions = missing_permissions(&app.permissions, &manifest_permissions());
    let events = if plan.webhook_url.is_some() {
        missing_events(&app.events, &manifest_events())
    } else {
        Vec::new()
    };
    if permissions.is_empty() && events.is_empty() {
        return Vec::new();
    }
    vec![Note {
        code: "app_grants_differ",
        params: json!({ "missing_permissions": permissions, "missing_events": events }),
    }]
}

fn error_params(params: Map<String, Value>, extra: &[(&str, Value)]) -> Value {
    let mut params = params;
    for (k, v) in extra {
        params.insert((*k).to_string(), v.clone());
    }
    Value::Object(params)
}

fn conversion_failure(plan: &Plan, e: &ManifestError) -> Failure {
    let recovery = match e.created() {
        Some((id, slug)) => ("app", plan.known_app(id, slug)),
        None => ("apps_url", Value::String(plan.apps_url())),
    };
    Failure::failed(e.code(), error_params(e.params(), &[recovery]))
}

async fn finish(
    plan: &Plan,
    prepared: &Prepared,
    code: &str,
    warnings: &mut Vec<Note>,
) -> Result<ScmAppReport, Failure> {
    let app = manifest::convert(
        &plan.api_url,
        &format!("saka/{VERSION}"),
        code,
        CONVERSION_TIMEOUT,
    )
    .await
    .map_err(|e| conversion_failure(plan, &e))?;
    let known = plan.known_app(app.id, &app.slug);
    let app_id = i64::try_from(app.id).map_err(|_| {
        Failure::failed(
            "app_manifest_response_invalid",
            json!({ "field": "id", "app": known }),
        )
    })?;
    warnings.extend(grant_warnings(&app, plan));
    let secret = match (&plan.webhook_url, &app.webhook_secret) {
        (Some(_), Some(secret)) => Some(secret),
        (Some(_), None) => {
            warnings.push(Note {
                code: "app_webhook_secret_missing",
                params: json!({}),
            });
            None
        }
        (None, _) => None,
    };
    let write_failed = |e: WriteError| match e {
        WriteError::Exists(path) => Failure::failed(
            "scm_app_files_exist",
            json!({ "paths": [path.display().to_string()], "app": known }),
        ),
        WriteError::Io(path, e) => Failure::failed(
            "files_write_failed",
            json!({ "path": path.display().to_string(), "reason": e.kind().to_string(), "app": known }),
        ),
    };
    let key_path =
        scm_config::write_new_private_file(&prepared.key_dir, PRIVATE_KEY_FILE, app.pem.as_bytes())
            .map_err(write_failed)?;
    let secret_path = secret
        .map(|s| {
            scm_config::write_new_private_file(&prepared.key_dir, WEBHOOK_SECRET_FILE, s.as_bytes())
                .map_err(write_failed)
        })
        .transpose()?;
    let key_text = path_text(&key_path)?;
    let secret_text = secret_path.as_deref().map(path_text).transpose()?;
    let added = block(
        plan,
        prepared,
        key_text,
        secret_text,
        app_id,
        &app.client_id,
    );
    let block_text = scm_config::block_text(&added);
    prepared.config.write(&block_text, Some(("app", &known)))?;
    let hooks_added = added.hooks_listen.is_some();
    let settings_url = plan.settings_url(&app.slug);
    Ok(ScmAppReport {
        integration: plan.id.clone(),
        next: vec![
            Note {
                code: "install_app",
                params: json!({ "url": install_url(&plan.web_url, &app.slug) }),
            },
            Note {
                code: "enable_device_flow",
                params: json!({ "url": settings_url }),
            },
            Note {
                code: "restart_server",
                params: json!({ "command": "saka serve" }),
            },
            Note {
                code: "run_doctor",
                params: json!({ "command": "saka doctor" }),
            },
        ],
        app: AppView {
            id: app.id,
            slug: app.slug.clone(),
            name: app.name.clone(),
            owner: app.owner.clone(),
            html_url: app.html_url.clone(),
            settings_url,
        },
        client_id: app.client_id.clone(),
        permissions: app.permissions.clone(),
        events: app.events.clone(),
        webhook_url: plan.webhook_url.clone(),
        files: FilesView {
            private_key: key_path.display().to_string(),
            webhook_secret: secret_path.map(|p| p.display().to_string()),
        },
        config: ConfigView {
            path: prepared.config.path.display().to_string(),
            created: prepared.config.created(),
            hooks_added,
        },
        warnings: Vec::new(),
        exit_code: 0,
    })
}

struct Interrupts {
    terminate: tokio::signal::unix::Signal,
    interrupt: tokio::signal::unix::Signal,
}

impl Interrupts {
    fn listen() -> Result<Interrupts, Failure> {
        use tokio::signal::unix::{SignalKind, signal};
        let unavailable = |_| Failure::refused("signals_unavailable", json!({}));
        Ok(Interrupts {
            terminate: signal(SignalKind::terminate()).map_err(unavailable)?,
            interrupt: signal(SignalKind::interrupt()).map_err(unavailable)?,
        })
    }

    async fn received(&mut self) {
        tokio::select! {
            _ = self.terminate.recv() => {}
            _ = self.interrupt.recv() => {}
        }
    }
}

async fn flow(plan: Plan, warnings: &mut Vec<Note>) -> Result<ScmAppReport, Failure> {
    let prepared = preflight(&plan)?;
    let mut interrupts = Interrupts::listen()?;
    let listener = callback::bind(plan.port).map_err(|e| {
        Failure::refused(
            "callback_port_unavailable",
            json!({ "port": plan.port, "reason": e.kind().to_string() }),
        )
    })?;
    let port = listener
        .local_addr()
        .map_err(|e| {
            Failure::refused(
                "callback_port_unavailable",
                json!({ "port": plan.port, "reason": e.kind().to_string() }),
            )
        })?
        .port();
    let tokens = callback::Tokens {
        state: token()?,
        launch: token()?,
        nonce: token()?.to_string(),
    };
    let redirect_url = format!("http://127.0.0.1:{port}/callback");
    let document = manifest::manifest(&ManifestRequest {
        name: &plan.name,
        integration_id: &plan.id,
        redirect_url: &redirect_url,
        webhook_url: plan.webhook_url.as_deref(),
    });
    let form_action = manifest::form_action(&plan.web_url, plan.org.as_deref(), &tokens.state)
        .map_err(|_| Failure::flag("--web-url", "not_https_or_loopback"))?;
    let form_origin = Url::parse(&plan.web_url)
        .map(|u| u.origin().ascii_serialization())
        .map_err(|_| Failure::flag("--web-url", "not_https_or_loopback"))?;
    let start_url = format!("http://127.0.0.1:{port}/start/{}", tokens.launch.as_str());
    let page = callback::Page {
        form_action,
        manifest: document.to_string(),
        form_origin,
    };
    let mut listener = callback::start(listener, port, tokens, page).map_err(|e| {
        Failure::refused(
            "callback_port_unavailable",
            json!({ "port": plan.port, "reason": e.kind().to_string() }),
        )
    })?;
    cli::err(&format!("open  {start_url}"));
    cli::err(&format!(
        "waiting  for GitHub (up to {} s)",
        plan.timeout_secs
    ));
    let opening = plan.open_browser.then(|| browser::open(&start_url));
    let deadline = tokio::time::sleep(Duration::from_secs(plan.timeout_secs));
    let claim = tokio::select! {
        claim = &mut listener.claim => claim.ok(),
        () = deadline => None,
        () = interrupts.received() => {
            listener.close().await;
            return Err(Failure {
                exit: EXIT_INTERRUPTED,
                code: "interrupted".into(),
                params: json!({}),
                block: None,
            });
        }
    };
    let Some(claim) = claim else {
        listener.close().await;
        return Err(Failure::failed(
            "app_manifest_timed_out",
            json!({ "timeout_secs": plan.timeout_secs, "apps_url": plan.apps_url() }),
        ));
    };
    let result = finish(&plan, &prepared, &claim.code, warnings).await;
    let text = match &result {
        Ok(report) => format!(
            "Saka received the GitHub App {}. Return to the terminal.",
            report.app.slug
        ),
        Err(f) => format!(
            "Saka could not finish creating the App ({}). Return to the terminal.",
            f.code
        ),
    };
    claim
        .reply
        .send(text)
        .map_err(|_| "the browser left")
        .best_effort("answer the callback");
    tokio::time::timeout(SERVER_DRAIN, listener.close())
        .await
        .best_effort("drain the callback server");
    if let Some(opening) = opening
        && let Err(reason) = opening.outcome().await
    {
        warnings.insert(
            0,
            Note {
                code: "browser_unavailable",
                params: json!({ "reason": reason }),
            },
        );
    }
    result
}

fn print_report(report: &ScmAppReport, json_out: bool) {
    if json_out {
        cli::out(&serde_json::to_string(report).unwrap_or_default());
        return;
    }
    let app = &report.app;
    cli::out(&format!(
        "app  {} {} created (owner {})",
        app.slug, app.id, app.owner
    ));
    let mut files = report.files.private_key.clone();
    if let Some(secret) = &report.files.webhook_secret {
        files.push(' ');
        files.push_str(secret);
    }
    cli::out(&format!("files  {files}"));
    let hooks = if report.config.hooks_added {
        ", [hooks] added"
    } else {
        ""
    };
    cli::out(&format!(
        "config  {} [[scm]] {} added{hooks}",
        report.config.path, report.integration
    ));
    for w in &report.warnings {
        cli::out(&format!("warning  {} {}", w.code, w.params));
    }
    for n in &report.next {
        match n.code {
            "install_app" => cli::out(&format!("next  install {}", text(&n.params["url"]))),
            "enable_device_flow" => cli::out(&format!(
                "next  enable Device Flow at {}",
                text(&n.params["url"])
            )),
            "restart_server" => cli::out("next  restart saka serve, then run saka doctor"),
            _ => {}
        }
    }
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

pub fn run(args: &CreateAppArgs, json_out: bool) -> ExitCode {
    let outcome = plan(args).and_then(|plan| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Failure::refused("runtime_unavailable", json!({})))?;
        let mut warnings = Vec::new();
        let mut report = runtime.block_on(flow(plan, &mut warnings))?;
        report.warnings = warnings;
        Ok(report)
    });
    match outcome {
        Ok(report) => {
            print_report(&report, json_out);
            ExitCode::SUCCESS
        }
        Err(f) => {
            print_failure(&f, json_out);
            ExitCode::from(f.exit)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn organization_logins_follow_the_provider_rule() {
        assert!(org_ok("acme"));
        assert!(org_ok("a-1"));
        assert!(!org_ok("-a"));
        assert!(!org_ok("a_b"));
        assert!(!org_ok(&"a".repeat(40)));
    }

    #[test]
    fn app_names_are_short_and_printable() {
        assert!(name_ok("Saka for acme"));
        assert!(name_ok(&"é".repeat(34)));
        assert!(!name_ok(&"a".repeat(35)));
        assert!(!name_ok("   "));
        assert!(!name_ok("a\tb"));
    }

    #[test]
    fn webhook_base_urls_are_plain_https() {
        assert_eq!(
            webhook_base("https://saka.example.com/").unwrap(),
            "https://saka.example.com"
        );
        assert_eq!(webhook_base("http://x").unwrap_err(), "not_https");
        assert_eq!(webhook_base("https://x/?a=1").unwrap_err(), "has_query");
        assert_eq!(webhook_base("https://u:p@x").unwrap_err(), "has_userinfo");
        assert_eq!(webhook_base("https://x/#f").unwrap_err(), "has_fragment");
    }

    #[test]
    fn a_written_integration_is_accepted_by_the_server_checks() {
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("scm/github");
        let secret = "s".repeat(40);
        let key = scm_config::write_new_private_file(&keys, PRIVATE_KEY_FILE, b"pem").unwrap();
        let secret_file =
            scm_config::write_new_private_file(&keys, WEBHOOK_SECRET_FILE, secret.as_bytes())
                .unwrap();
        let text = scm_config::appended(
            "# operator config\n[server]\nlisten = \"127.0.0.1:7400\"",
            &scm_config::block_text(&ConfigBlock {
                id: "github",
                web_url: Some("http://127.0.0.1:9"),
                api_url: Some("http://127.0.0.1:9"),
                credentials: Credentials::App {
                    app_id: 123_456,
                    private_key_file: key.to_str().unwrap(),
                    client_id: "Iv23liAbCdEfGh123456",
                },
                webhook_secret_file: secret_file.to_str(),
                hooks_listen: Some(DEFAULT_HOOKS_LISTEN),
            }),
        );
        let cfg = Config::parse(&text).unwrap();
        cfg.check_key_files().unwrap();
        assert_eq!(
            cfg.webhook_secrets().unwrap(),
            [("github".to_string(), secret.into_bytes())]
        );
        assert_eq!(
            cfg.hooks_listen().unwrap().to_string(),
            DEFAULT_HOOKS_LISTEN
        );
    }
}
