use std::collections::BTreeSet;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use saka_agents::ClaudeAgents;
use saka_api::dto::{self, DiagnosticCheck, DiagnosticReport, DiagnosticSummary};
use saka_core::diagnostics::{self as eval, CheckResult, Finding, Subject};
use saka_core::vocab::{CheckId, DiagnosticStatus, ReportMode};
use saka_engine::diagnostics::{self as run, Host, Offline};
use saka_engine::{DiagnosticFilter, DiagnosticLimits};
use saka_env::{SystemdSupervisor, ToolchainInstaller};
use saka_ports::Clock;
use serde_json::json;

use crate::cli::{self, Client};
use crate::config::{self, Config};
use crate::serve::{self, EXIT_REFUSED, SystemClock, VERSION};

const REACH_TIMEOUT: Duration = Duration::from_secs(5);
const REPORT_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(clap::Args)]
pub struct DoctorArgs {
    #[arg(
        help = "Only this project's project checks (id or name); instance, agent, and integration checks always run",
        long
    )]
    project: Option<String>,
    #[arg(
        help = "Run only this check, for example agent.login (repeatable)",
        long = "check"
    )]
    check: Vec<String>,
    #[arg(help = "Exit 1 on a warning too", long)]
    strict: bool,
}

struct ClientChecks {
    checks: Vec<DiagnosticCheck>,
}

impl ClientChecks {
    fn push(&mut self, id: CheckId, finding: Finding, started: Instant) {
        self.checks.push(DiagnosticCheck::from(&CheckResult {
            id,
            subject: Subject::instance(),
            finding,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }));
    }
}

fn filter_of(args: &DoctorArgs) -> Result<Option<BTreeSet<CheckId>>, ExitCode> {
    if args.check.is_empty() {
        return Ok(None);
    }
    let mut ids = BTreeSet::new();
    for raw in &args.check {
        match raw.parse::<CheckId>() {
            Ok(id) => {
                ids.insert(id);
            }
            Err(_) => {
                cli::refuse("check_unknown", &json!({ "id": raw }));
                return Err(ExitCode::from(EXIT_REFUSED));
            }
        }
    }
    Ok(Some(ids))
}

fn summary_of(checks: &[DiagnosticCheck]) -> DiagnosticSummary {
    let s = eval::Summary::of(
        checks
            .iter()
            .filter_map(|c| c.status.parse::<DiagnosticStatus>().ok()),
    );
    DiagnosticSummary {
        pass: s.pass,
        warn: s.warn,
        fail: s.fail,
        skip: s.skip,
    }
}

fn line(c: &DiagnosticCheck) -> String {
    let mut text = format!(
        "{}  {}  {}  {} {}",
        c.status, c.id, c.subject.label, c.code, c.params
    );
    if let Some(command) = c
        .fix
        .as_ref()
        .and_then(|f| f.params.get("command"))
        .and_then(|v| v.as_str())
    {
        text.push_str(&format!("  → {command}"));
    }
    text
}

fn finish(report: &DiagnosticReport, json: bool, strict: bool, refused: bool) -> ExitCode {
    if json {
        cli::out(&serde_json::to_string(report).unwrap_or_default());
    } else {
        for c in &report.checks {
            cli::out(&line(c));
        }
        let s = report.summary;
        cli::out(&format!(
            "summary  {} pass, {} warn, {} fail, {} skip ({})",
            s.pass, s.warn, s.fail, s.skip, report.mode
        ));
    }
    if refused {
        ExitCode::from(EXIT_REFUSED)
    } else if report.summary.fail > 0 || (strict && report.summary.warn > 0) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn report(
    mode: ReportMode,
    server_version: &str,
    checks: Vec<DiagnosticCheck>,
) -> DiagnosticReport {
    DiagnosticReport {
        generated_at: dto::time(SystemClock.now()),
        server_version: server_version.into(),
        mode: mode.as_str().into(),
        summary: summary_of(&checks),
        checks,
    }
}

pub fn account_of(cfg: &Config) -> String {
    format!("{}:{}", cfg.agent.cli, cfg.account_name())
}

fn host(cfg: &Config) -> Result<Host, String> {
    let (sink, _events) = tokio::sync::mpsc::unbounded_channel();
    let toolchains = ToolchainInstaller::new(serve::toolchain_config(cfg)).map_err(|e| e.code)?;
    Ok(Host {
        agents: Arc::new(ClaudeAgents::new(
            serve::agents_config(cfg),
            Arc::new(SystemdSupervisor),
            sink,
        )),
        supervisor: Arc::new(SystemdSupervisor),
        toolchains: Arc::new(toolchains),
    })
}

fn unreachable_report(
    filter: &DiagnosticFilter,
    account: &str,
    server: &str,
) -> Vec<DiagnosticCheck> {
    CheckId::server()
        .filter(|id| filter.checks.as_ref().is_none_or(|c| c.contains(id)))
        .map(|id| {
            let subject = match id.subject_kind() {
                saka_core::vocab::SubjectKind::AgentAccount => Subject::agent_account(account),
                _ => Subject::instance(),
            };
            DiagnosticCheck::from(&CheckResult {
                id,
                subject,
                finding: eval::skipped_unreachable(server),
                duration_ms: 0,
            })
        })
        .collect()
}

async fn offline(cfg: &Config, filter: &DiagnosticFilter, server: &str) -> Vec<DiagnosticCheck> {
    let account = account_of(cfg);
    let host = match host(cfg) {
        Ok(h) => h,
        Err(code) => {
            cli::refuse("config_refused", &json!({ "code": code }));
            return unreachable_report(filter, &account, server);
        }
    };
    let db = cfg.data_dir().join("saka.db");
    let inputs = Offline {
        db: &db,
        account: &account,
        server,
        client_version: VERSION,
        now: SystemClock.now(),
    };
    let r = run::offline(&host, &inputs, filter, DiagnosticLimits::default()).await;
    r.checks.iter().map(DiagnosticCheck::from).collect()
}

fn query(filter: &DiagnosticFilter, project: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = project {
        parts.push(format!("project_id={p}"));
    }
    for id in filter.checks.iter().flatten() {
        parts.push(format!("check={id}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

pub async fn run(
    args: &DoctorArgs,
    server_flag: Option<String>,
    json: bool,
    token_file: Option<&std::path::Path>,
) -> ExitCode {
    let wanted = match filter_of(args) {
        Ok(f) => f,
        Err(code) => return code,
    };
    let server_checks: Option<BTreeSet<CheckId>> = wanted
        .as_ref()
        .map(|w| w.iter().copied().filter(|c| !c.is_client()).collect());
    let filter = DiagnosticFilter {
        checks: server_checks.clone(),
        project_id: None,
    };
    let mut client = ClientChecks { checks: Vec::new() };

    let started = Instant::now();
    let path = config::path();
    let file_exists = path.as_ref().is_some_and(|p| p.exists());
    let shown = path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let cfg = match Config::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            client.push(
                CheckId::CliConfig,
                eval::config_invalid(&shown, e.code, &e.detail),
                started,
            );
            let r = report(ReportMode::Offline, VERSION, client.checks);
            return finish(&r, json, args.strict, true);
        }
    };
    let config_finding = if file_exists {
        eval::config_ok(&shown)
    } else {
        eval::config_absent()
    };
    client.push(CheckId::CliConfig, config_finding, started);
    let server = server_flag
        .clone()
        .unwrap_or_else(|| format!("http://{}", cfg.server.listen));
    let token = match crate::cli_token::resolve(&server, token_file) {
        Ok(token) => token,
        Err(code) => return code,
    };
    let quick = Client::with_timeout(&server, json, REACH_TIMEOUT).authorized(token.as_ref());

    let started = Instant::now();
    let health = match quick.get_quiet("/healthz").await {
        Ok(a) if a.status == 200 => Ok(()),
        Ok(a) => Err(format!("HTTP {}", a.status)),
        Err(reason) => Err(reason),
    };
    if let Err(reason) = health {
        client.push(
            CheckId::ServerReachable,
            eval::server_unreachable(&server, &reason),
            started,
        );
        client.push(
            CheckId::ServerVersion,
            eval::skipped_unreachable(&server),
            Instant::now(),
        );
        let local = file_exists || server_flag.is_none();
        let server_part = if local {
            offline(&cfg, &filter, &server).await
        } else {
            unreachable_report(&filter, &account_of(&cfg), &server)
        };
        client.checks.extend(server_part);
        let r = report(ReportMode::Offline, VERSION, client.checks);
        return finish(&r, json, args.strict, false);
    }
    client.push(
        CheckId::ServerReachable,
        eval::server_reachable(&server),
        started,
    );

    let started = Instant::now();
    let meta = quick.get_quiet("/api/v1/meta").await;
    let version_finding = match &meta {
        Ok(a) => eval::api_version(
            a.body["server_version"].as_str().unwrap_or_default(),
            a.body["api_version"].as_str().unwrap_or_default(),
            a.body["min_client_api_version"]
                .as_str()
                .unwrap_or_default(),
            dto::API_VERSION,
        ),
        Err(reason) => eval::server_unreachable(&server, reason),
    };
    client.push(CheckId::ServerVersion, version_finding, started);

    let slow = Client::with_timeout(&server, json, REPORT_TIMEOUT).authorized(token.as_ref());
    let project = match &args.project {
        Some(p) => match slow.project_id(p).await {
            Ok(id) => Some(id),
            Err(_) => return ExitCode::from(EXIT_REFUSED),
        },
        None => None,
    };
    let server_version = meta
        .as_ref()
        .ok()
        .and_then(|a| a.body["server_version"].as_str().map(String::from))
        .unwrap_or_default();
    if server_checks.as_ref().is_some_and(BTreeSet::is_empty) {
        let r = report(ReportMode::Online, &server_version, client.checks);
        return finish(&r, json, args.strict, false);
    }
    let answer = match slow
        .get_quiet(&format!(
            "/api/v1/diagnostics{}",
            query(&filter, project.as_deref())
        ))
        .await
    {
        Ok(a) => a,
        Err(reason) => {
            cli::refuse(
                "server_unreachable",
                &json!({ "server": server, "reason": reason }),
            );
            return ExitCode::FAILURE;
        }
    };
    let parsed = (answer.status == 200)
        .then(|| serde_json::from_value::<DiagnosticReport>(answer.body.clone()).ok())
        .flatten();
    let Some(server_report) = parsed else {
        let code = answer.body["code"].as_str().unwrap_or("error");
        cli::refuse(code, &answer.body["params"]);
        return if answer.status == 422 {
            ExitCode::from(EXIT_REFUSED)
        } else {
            ExitCode::FAILURE
        };
    };
    client.checks.extend(server_report.checks);
    let r = report(
        ReportMode::Online,
        &server_report.server_version,
        client.checks,
    );
    finish(&r, json, args.strict, false)
}
