use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use reqwest::Url;
use saka_core::diagnostics::{self as eval, TokenStatus};
use saka_core::vocab::DiagnosticStatus;
use saka_ports::Clock;
use saka_scm::gitlab::{TOKEN_REQUIREMENTS, token_status};
use saka_scm::{GitLabClient, GitLabConfig};
use serde::Serialize;
use serde_json::{Value, json};

use crate::cli;
use crate::config;
use crate::scm_config::{
    self, ConfigBlock, Credentials, DEFAULT_HOOKS_LISTEN, Failure, OperatorConfig, WriteError,
    id_ok, path_text, print_failure, provider_url,
};
use crate::serve::{SystemClock, VERSION};

const DEFAULT_ID: &str = "gitlab";
const DEFAULT_WEB_URL: &str = "https://gitlab.com";
const SECRET_BYTES: usize = 32;
const TOKEN_FLAG: &str = "--token-file";
const SECRET_FLAG: &str = "--webhook-secret-file";

#[derive(clap::Args)]
pub struct ConnectGitlabArgs {
    #[arg(
        help = "A file holding a GitLab access token with the api scope (mode 0600, owned by you)",
        long
    )]
    token_file: PathBuf,
    #[arg(
        help = "The `[[scm]].id` to add",
        long,
        default_value = DEFAULT_ID
    )]
    id: String,
    #[arg(
        help = "The GitLab web URL, for a self-managed host (https only)",
        long,
        default_value = DEFAULT_WEB_URL
    )]
    web_url: String,
    #[arg(help = "The GitLab API URL (default: <web-url>/api/v4)", long)]
    api_url: Option<String>,
    #[arg(
        help = "A file for the webhook secret: used when it exists, created with 32 random bytes (hex, mode 0600) when it does not",
        long
    )]
    webhook_secret_file: Option<PathBuf>,
    #[arg(
        help = "The operator config to edit (default: SAKA_CONFIG, then the user config folder)",
        long
    )]
    config: Option<PathBuf>,
}

struct Plan {
    id: String,
    web_url: String,
    api_url: String,
    token_file: PathBuf,
    secret_file: Option<PathBuf>,
    config: Option<PathBuf>,
}

impl Plan {
    fn api_url_in_config(&self) -> Option<&str> {
        let derived = format!("{}/api/v4", self.web_url);
        (self.api_url != derived).then_some(self.api_url.as_str())
    }
}

fn absolute(flag: &str, path: &Path) -> Result<PathBuf, Failure> {
    std::path::absolute(path).map_err(|_| Failure::flag(flag, "not_a_path"))
}

fn plan(args: &ConnectGitlabArgs) -> Result<Plan, Failure> {
    if !id_ok(&args.id) {
        return Err(Failure::flag("--id", "pattern"));
    }
    let web = Url::parse(&args.web_url)
        .ok()
        .filter(|u| u.scheme() == "https" && u.host_str().is_some())
        .ok_or_else(|| Failure::flag("--web-url", "not_https"))?;
    if !web.username().is_empty() || web.password().is_some() || web.query().is_some() {
        return Err(Failure::flag("--web-url", "not_a_plain_url"));
    }
    let web_url = web.as_str().trim_end_matches('/').to_string();
    let api_url = match &args.api_url {
        Some(raw) => provider_url("--api-url", raw)?,
        None => format!("{web_url}/api/v4"),
    };
    Ok(Plan {
        id: args.id.clone(),
        web_url,
        api_url,
        token_file: absolute(TOKEN_FLAG, &args.token_file)?,
        secret_file: args
            .webhook_secret_file
            .as_deref()
            .map(|p| absolute(SECRET_FLAG, p))
            .transpose()?,
        config: args.config.clone(),
    })
}

fn private_file(path: &Path) -> Result<(), Failure> {
    config::check_private_file(path).map_err(|e| Failure::config(&e, path))
}

struct Prepared {
    config: OperatorConfig,
    secret_exists: bool,
    add_hooks: bool,
}

fn block<'a>(
    plan: &'a Plan,
    prepared: &Prepared,
    token: &'a str,
    secret: Option<&'a str>,
) -> ConfigBlock<'a> {
    ConfigBlock {
        id: &plan.id,
        web_url: Some(&plan.web_url),
        api_url: plan.api_url_in_config(),
        credentials: Credentials::Token { token_file: token },
        webhook_secret_file: secret,
        hooks_listen: (prepared.add_hooks && secret.is_some()).then_some(DEFAULT_HOOKS_LISTEN),
    }
}

fn preflight(plan: &Plan) -> Result<Prepared, Failure> {
    let config = OperatorConfig::load(plan.config.as_deref())?;
    config.check_free(&plan.id, &plan.web_url)?;
    private_file(&plan.token_file)?;
    let secret_exists = match &plan.secret_file {
        Some(path) => match std::fs::symlink_metadata(path) {
            Ok(_) => {
                private_file(path)?;
                true
            }
            Err(_) => false,
        },
        None => false,
    };
    let prepared = Prepared {
        add_hooks: plan.secret_file.is_some() && !config.has_hooks(),
        secret_exists,
        config,
    };
    let secret = plan.secret_file.as_deref().map(path_text).transpose()?;
    let trial = block(plan, &prepared, path_text(&plan.token_file)?, secret);
    prepared
        .config
        .check_block(&scm_config::block_text(&trial))?;
    Ok(prepared)
}

#[derive(Serialize)]
struct Note {
    code: &'static str,
    params: Value,
}

#[derive(Serialize)]
struct TokenView {
    name: String,
    scopes: Vec<String>,
    expires_at: Option<String>,
}

#[derive(Serialize)]
struct FilesView {
    token: String,
    webhook_secret: Option<String>,
}

#[derive(Serialize)]
struct ConfigView {
    path: String,
    created: bool,
    hooks_added: bool,
}

#[derive(Serialize)]
struct Report {
    integration: String,
    token: TokenView,
    files: FilesView,
    config: ConfigView,
    warnings: Vec<Note>,
    next: Vec<Note>,
    exit_code: u8,
}

fn finding_failure(finding: &eval::Finding) -> Failure {
    let mut params = finding.params.clone();
    if let (Some(fix), Some(map)) = (&finding.fix, params.as_object_mut()) {
        map.insert(
            "fix".into(),
            json!({ "code": fix.code, "params": fix.params }),
        );
    }
    Failure::failed(&finding.code, params)
}

async fn validated_token(plan: &Plan) -> Result<(TokenStatus, Vec<Note>), Failure> {
    let client = GitLabClient::new(
        GitLabConfig {
            integration_id: plan.id.clone(),
            api_url: plan.api_url.clone(),
            web_url: plan.web_url.clone(),
            token_file: plan.token_file.clone(),
            user_agent: format!("saka/{VERSION}"),
        },
        Arc::new(SystemClock),
    )
    .map_err(|e| Failure::refused(&e.code, e.params))?;
    let token = match token_status(&client).await {
        Ok(token) => token,
        Err(e) => {
            let finding =
                eval::token_failure(&TOKEN_REQUIREMENTS, &plan.web_url, &e.code, &e.params);
            return Err(finding_failure(&finding));
        }
    };
    let finding = eval::token_credential(
        &TOKEN_REQUIREMENTS,
        &plan.web_url,
        &token,
        SystemClock.now(),
    );
    let mut warnings = Vec::new();
    match finding.status {
        DiagnosticStatus::Fail => return Err(finding_failure(&finding)),
        DiagnosticStatus::Warn => warnings.push(Note {
            code: "token_expiring",
            params: finding.params.clone(),
        }),
        DiagnosticStatus::Pass | DiagnosticStatus::Skip => {}
    }
    Ok((token, warnings))
}

fn webhook_secret() -> Result<String, Failure> {
    let mut bytes = [0u8; SECRET_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| Failure::refused("random_unavailable", json!({})))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn create_secret(path: &Path) -> Result<(), Failure> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return Err(Failure::flag(SECRET_FLAG, "not_a_file_path"));
    };
    let secret = webhook_secret()?;
    match scm_config::write_new_private_file(dir, name, secret.as_bytes()) {
        Ok(_) => Ok(()),
        Err(WriteError::Exists(p)) => Err(Failure::refused(
            "scm_secret_file_exists",
            json!({ "path": p.display().to_string() }),
        )),
        Err(WriteError::Io(p, e)) => Err(Failure::failed(
            "scm_secret_file_write_failed",
            json!({ "path": p.display().to_string(), "reason": e.kind().to_string() }),
        )),
    }
}

async fn connect(plan: &Plan, prepared: &Prepared) -> Result<Report, Failure> {
    let (token, warnings) = validated_token(plan).await?;
    if let (Some(path), false) = (&plan.secret_file, prepared.secret_exists) {
        create_secret(path)?;
    }
    let secret = plan.secret_file.as_deref().map(path_text).transpose()?;
    let added = block(plan, prepared, path_text(&plan.token_file)?, secret);
    prepared
        .config
        .write(&scm_config::block_text(&added), None)?;
    let mut next = vec![Note {
        code: "add_token_to_projects",
        params: json!({ "role": "Maintainer", "token": token.name }),
    }];
    if let Some(path) = &plan.secret_file {
        next.push(Note {
            code: "add_project_webhook",
            params: json!({
                "path": format!("/hooks/{}", plan.id),
                "secret_file": path.display().to_string(),
                "events": TOKEN_REQUIREMENTS.hook_events,
            }),
        });
    }
    next.push(Note {
        code: "run_doctor",
        params: json!({ "command": "saka doctor --check scm.credential" }),
    });
    Ok(Report {
        integration: plan.id.clone(),
        token: TokenView {
            name: token.name,
            scopes: token.scopes,
            expires_at: token.expires_at.map(saka_core::time::Timestamp::to_rfc3339),
        },
        files: FilesView {
            token: plan.token_file.display().to_string(),
            webhook_secret: plan.secret_file.as_ref().map(|p| p.display().to_string()),
        },
        config: ConfigView {
            path: prepared.config.path.display().to_string(),
            created: prepared.config.created(),
            hooks_added: prepared.add_hooks,
        },
        warnings,
        next,
        exit_code: 0,
    })
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

fn print_report(report: &Report, json_out: bool) {
    if json_out {
        cli::out(&serde_json::to_string(report).unwrap_or_default());
        return;
    }
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
            "add_token_to_projects" => cli::out(&format!(
                "next  add the user of token {} to every project as {}",
                text(&n.params["token"]),
                text(&n.params["role"])
            )),
            "add_project_webhook" => cli::out(&format!(
                "next  add a project webhook at https://<public host>{} with the secret in {} and the events {}",
                text(&n.params["path"]),
                text(&n.params["secret_file"]),
                n.params["events"]
                    .as_array()
                    .map(|events| events.iter().map(text).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default()
            )),
            "run_doctor" => cli::out(&format!(
                "next  restart saka serve, then run {}",
                text(&n.params["command"])
            )),
            _ => {}
        }
    }
}

pub fn run(args: &ConnectGitlabArgs, json_out: bool) -> ExitCode {
    let outcome = plan(args).and_then(|plan| {
        let prepared = preflight(&plan)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Failure::refused("runtime_unavailable", json!({})))?;
        runtime.block_on(connect(&plan, &prepared))
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
