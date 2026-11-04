use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::admission::paused_windows;
use crate::authorship::{PROACTIVE_REFRESH_WINDOW_MS, ProjectUserToken, UserLink};
use crate::model::{AccountQuota, Project};
use crate::paths::project_config_path;
use crate::time::Timestamp;
use crate::vocab::{
    AuthorizationFailure, CheckId, DiagnosticStatus, HostTool, ReportMode, SubjectKind, Tool,
};

pub const INSTANCE: &str = "instance";
pub const MIGRATE_COMMAND: &str = "saka migrate";
pub const SERVE_COMMAND: &str = "saka serve";
pub const AGENT_LOGIN_COMMAND: &str = "saka agent login";
pub const LINGER_COMMAND: &str = "loginctl enable-linger";

impl CheckId {
    pub fn subject_kind(self) -> SubjectKind {
        match self {
            CheckId::AgentBinary | CheckId::AgentLogin | CheckId::AgentQuota => {
                SubjectKind::AgentAccount
            }
            CheckId::CliConfig
            | CheckId::ServerReachable
            | CheckId::ServerVersion
            | CheckId::ServerReady
            | CheckId::DbMigrations
            | CheckId::RuntimeSupervisor
            | CheckId::ToolchainCache
            | CheckId::ToolchainSources => SubjectKind::Instance,
            CheckId::ScmApp | CheckId::ScmWebhook | CheckId::ScmUserLink => {
                SubjectKind::Integration
            }
            CheckId::ScmInstallation => SubjectKind::Repo,
            CheckId::ProjectUserToken | CheckId::ProjectConfig => SubjectKind::Project,
            CheckId::ToolchainRepo => SubjectKind::ProjectRepo,
        }
    }

    pub fn is_client(self) -> bool {
        matches!(
            self,
            CheckId::CliConfig | CheckId::ServerReachable | CheckId::ServerVersion
        )
    }

    pub fn runs_offline(self) -> bool {
        matches!(
            self,
            CheckId::DbMigrations
                | CheckId::RuntimeSupervisor
                | CheckId::AgentBinary
                | CheckId::AgentLogin
                | CheckId::ToolchainCache
        )
    }

    pub fn server() -> impl Iterator<Item = CheckId> {
        CheckId::ALL.iter().copied().filter(|c| !c.is_client())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub kind: SubjectKind,
    pub id: String,
    pub label: String,
}

impl Subject {
    pub fn instance() -> Subject {
        Subject {
            kind: SubjectKind::Instance,
            id: INSTANCE.into(),
            label: INSTANCE.into(),
        }
    }

    pub fn agent_account(account: &str) -> Subject {
        Subject {
            kind: SubjectKind::AgentAccount,
            id: account.into(),
            label: account.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub code: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub status: DiagnosticStatus,
    pub code: String,
    pub params: Value,
    pub fix: Option<Fix>,
}

impl Finding {
    pub fn pass(code: &str, params: Value) -> Finding {
        Finding::of(DiagnosticStatus::Pass, code, params, None)
    }

    pub fn skip(code: &str, params: Value) -> Finding {
        Finding::of(DiagnosticStatus::Skip, code, params, None)
    }

    pub fn warn(code: &str, params: Value, fix: Option<Fix>) -> Finding {
        Finding::of(DiagnosticStatus::Warn, code, params, fix)
    }

    pub fn fail(code: &str, params: Value, fix: Option<Fix>) -> Finding {
        Finding::of(DiagnosticStatus::Fail, code, params, fix)
    }

    fn of(status: DiagnosticStatus, code: &str, params: Value, fix: Option<Fix>) -> Finding {
        Finding {
            status,
            code: code.into(),
            params,
            fix,
        }
    }
}

fn fix(code: &str, params: Value) -> Option<Fix> {
    Some(Fix {
        code: code.into(),
        params,
    })
}

fn command(code: &str, command: &str) -> Option<Fix> {
    fix(code, json!({ "command": command }))
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    pub id: CheckId,
    pub subject: Subject,
    pub finding: Finding,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub pass: u32,
    pub warn: u32,
    pub fail: u32,
    pub skip: u32,
}

impl Summary {
    pub fn of(statuses: impl IntoIterator<Item = DiagnosticStatus>) -> Summary {
        let mut s = Summary::default();
        for status in statuses {
            let n = match status {
                DiagnosticStatus::Pass => &mut s.pass,
                DiagnosticStatus::Warn => &mut s.warn,
                DiagnosticStatus::Fail => &mut s.fail,
                DiagnosticStatus::Skip => &mut s.skip,
            };
            *n = n.saturating_add(1);
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub generated_at: Timestamp,
    pub server_version: String,
    pub mode: ReportMode,
    pub summary: Summary,
    pub checks: Vec<CheckResult>,
}

impl Report {
    #[must_use]
    pub fn new(
        generated_at: Timestamp,
        server_version: &str,
        mode: ReportMode,
        mut checks: Vec<CheckResult>,
    ) -> Report {
        order(&mut checks);
        Report {
            generated_at,
            server_version: server_version.into(),
            mode,
            summary: Summary::of(checks.iter().map(|c| c.finding.status)),
            checks,
        }
    }
}

pub fn order(checks: &mut [CheckResult]) {
    checks.sort_by(|a, b| {
        a.id.cmp(&b.id)
            .then_with(|| a.subject.label.cmp(&b.subject.label))
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationState {
    pub applied: i64,
    pub latest: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorProbe {
    Available,
    Unavailable {
        exit_code: Option<i32>,
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCli {
    pub command: String,
    pub path_dirs: Vec<String>,
    pub found: Option<FoundCli>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundCli {
    pub path: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLogin {
    CliNotFound { command: String },
    LoggedIn { auth_method: String },
    NotLoggedIn,
    Unavailable { exit_code: Option<i32> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledToolchain {
    pub tool: Tool,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainCache {
    pub path: String,
    pub writable: bool,
    pub installed: Vec<InstalledToolchain>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFailure {
    pub url: Option<String>,
    pub status: Option<u16>,
}

fn cli_of(account: &str) -> &str {
    account.split_once(':').map_or(account, |(cli, _)| cli)
}

pub fn config_ok(path: &str) -> Finding {
    Finding::pass("config_ok", json!({ "path": path }))
}

pub fn config_invalid(path: &str, code: &str, detail: &str) -> Finding {
    Finding::fail(
        "config_invalid",
        json!({ "code": code, "detail": detail }),
        fix("fix_config", json!({ "path": path })),
    )
}

pub fn config_absent() -> Finding {
    Finding::skip("config_absent", json!({}))
}

pub fn server_reachable(server: &str) -> Finding {
    Finding::pass("server_reachable", json!({ "server": server }))
}

pub fn server_unreachable(server: &str, reason: &str) -> Finding {
    Finding::fail(
        "server_unreachable",
        json!({ "server": server, "reason": reason }),
        command("start_server", SERVE_COMMAND),
    )
}

pub fn skipped_unreachable(server: &str) -> Finding {
    Finding::skip("server_unreachable", json!({ "server": server }))
}

pub fn api_version(
    server_version: &str,
    server_api: &str,
    min_client: &str,
    client: &str,
) -> Finding {
    let number = |v: &str| v.parse::<u32>().ok();
    let compatible = matches!(
        (number(server_api), number(min_client), number(client)),
        (Some(s), Some(min), Some(c)) if s == c && min <= c
    );
    if compatible {
        Finding::pass(
            "api_compatible",
            json!({ "server_version": server_version, "api_version": server_api }),
        )
    } else {
        Finding::fail(
            "api_version_mismatch",
            json!({ "server": server_api, "client": client }),
            fix("upgrade_saka", json!({})),
        )
    }
}

pub fn server_ready(failing: &[&str]) -> Finding {
    if failing.is_empty() {
        Finding::pass("server_ready", json!({}))
    } else {
        Finding::fail("server_not_ready", json!({ "failing": failing }), None)
    }
}

pub fn migrations(state: MigrationState) -> Finding {
    let MigrationState { applied, latest } = state;
    if applied == latest {
        Finding::pass("migrations_current", json!({ "applied": applied }))
    } else if applied < latest {
        Finding::fail(
            "migrations_pending",
            json!({ "applied": applied, "latest": latest }),
            command("run_migrate", MIGRATE_COMMAND),
        )
    } else {
        Finding::fail(
            "migrations_ahead",
            json!({ "applied": applied, "latest": latest }),
            fix("upgrade_saka", json!({})),
        )
    }
}

pub fn supervisor(probe: &SupervisorProbe) -> Finding {
    match probe {
        SupervisorProbe::Available => Finding::pass("systemd_user_ok", json!({})),
        SupervisorProbe::Unavailable { exit_code, detail } => Finding::fail(
            "systemd_user_unavailable",
            json!({ "exit_code": exit_code, "detail": detail }),
            command("enable_systemd_user", LINGER_COMMAND),
        ),
    }
}

pub fn agent_binary(account: &str, cli: &AgentCli) -> Finding {
    match &cli.found {
        Some(found) => Finding::pass(
            "agent_cli_found",
            json!({ "command": cli.command, "path": found.path, "version": found.version }),
        ),
        None => Finding::fail(
            "agent_cli_not_found",
            json!({ "command": cli.command, "path_dirs": cli.path_dirs }),
            fix("install_agent_cli", json!({ "cli": cli_of(account) })),
        ),
    }
}

pub fn agent_login(account: &str, login: &AgentLogin) -> Finding {
    match login {
        AgentLogin::CliNotFound { command } => {
            Finding::skip("agent_cli_not_found", json!({ "command": command }))
        }
        AgentLogin::LoggedIn { auth_method } => Finding::pass(
            "agent_logged_in",
            json!({ "account": account, "auth_method": auth_method }),
        ),
        AgentLogin::NotLoggedIn => Finding::fail(
            "agent_not_logged_in",
            json!({ "account": account }),
            command("run_agent_login", AGENT_LOGIN_COMMAND),
        ),
        AgentLogin::Unavailable { exit_code } => Finding::fail(
            "agent_status_unavailable",
            json!({ "account": account, "exit_code": exit_code }),
            fix("upgrade_agent_cli", json!({ "cli": cli_of(account) })),
        ),
    }
}

pub fn agent_quota(
    account: &str,
    quotas: &[AccountQuota],
    threshold: f64,
    now: Timestamp,
) -> Finding {
    let mut windows: Vec<&AccountQuota> = quotas.iter().filter(|q| q.account == account).collect();
    if windows.is_empty() {
        return Finding::skip("quota_unknown", json!({ "account": account }));
    }
    windows.sort_by(|a, b| a.window.cmp(&b.window));
    let binding = paused_windows(quotas, account, threshold, now).max_by(|a, b| {
        a.resets_at
            .cmp(&b.resets_at)
            .then_with(|| b.window.cmp(&a.window))
    });
    match binding {
        Some(q) => Finding::warn(
            "quota_paused",
            json!({
                "account": account,
                "window": q.window,
                "utilization": q.utilization,
                "resets_at": q.resets_at.map(Timestamp::to_rfc3339),
            }),
            None,
        ),
        None => Finding::pass(
            "quota_ok",
            json!({
                "windows": windows.iter().map(|q| json!({
                    "window": q.window,
                    "utilization": q.utilization,
                    "resets_at": q.resets_at.map(Timestamp::to_rfc3339),
                })).collect::<Vec<_>>(),
            }),
        ),
    }
}

pub fn toolchain_cache(cache: &ToolchainCache) -> Finding {
    if !cache.writable {
        return Finding::fail(
            "toolchain_cache_unwritable",
            json!({ "path": cache.path }),
            None,
        );
    }
    let mut installed = cache.installed.clone();
    installed.sort_by(|a, b| (a.tool, &a.version).cmp(&(b.tool, &b.version)));
    Finding::pass(
        "toolchain_cache_ok",
        json!({
            "path": cache.path,
            "installed": installed.iter().map(|t| json!({ "tool": t.tool, "version": t.version })).collect::<Vec<_>>(),
        }),
    )
}

pub fn mirror_key(tool: Tool) -> &'static str {
    match tool {
        Tool::Go => "toolchain.go_dl_url",
        Tool::Node => "toolchain.node_dist_url",
        Tool::Pnpm => "toolchain.npm_registry_url",
    }
}

pub fn toolchain_sources(results: &[(Tool, Option<SourceFailure>)]) -> Vec<Finding> {
    let failed: Vec<Finding> = results
        .iter()
        .filter_map(|(tool, failure)| {
            let f = failure.as_ref()?;
            Some(Finding::warn(
                "toolchain_source_unreachable",
                json!({ "tool": tool, "url": f.url, "status": f.status }),
                fix(
                    "check_toolchain_mirror",
                    json!({ "key": mirror_key(*tool) }),
                ),
            ))
        })
        .collect();
    if !failed.is_empty() {
        return failed;
    }
    let tools: Vec<Tool> = results.iter().map(|(t, _)| *t).collect();
    vec![Finding::pass(
        "toolchain_sources_ok",
        json!({ "tools": tools }),
    )]
}

pub fn timed_out(timeout_ms: u64) -> Finding {
    Finding::fail("check_timed_out", json!({ "timeout_ms": timeout_ms }), None)
}

pub fn check_failed(error: &str) -> Finding {
    Finding::fail("check_failed", json!({ "error": error }), None)
}

pub fn engine_not_ready() -> Finding {
    Finding::skip("engine_not_ready", json!({}))
}

pub const SAKA_INIT_COMMAND: &str = "saka init";
pub const EXPIRY_WARNING_MS: i64 = PROACTIVE_REFRESH_WINDOW_MS;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppRequirements {
    pub permissions: &'static [(&'static str, &'static str)],
    pub recommended: &'static [(&'static str, &'static str)],
    pub events: &'static [&'static str],
}
pub const WEBHOOK_CONTENT_TYPE: &str = "json";

impl Subject {
    pub fn integration(integration_id: &str) -> Subject {
        Subject {
            kind: SubjectKind::Integration,
            id: integration_id.into(),
            label: integration_id.into(),
        }
    }

    pub fn repo(repo_id: &str, full_name: &str) -> Subject {
        Subject {
            kind: SubjectKind::Repo,
            id: repo_id.into(),
            label: full_name.into(),
        }
    }

    pub fn project(project_id: &str, name: &str) -> Subject {
        Subject {
            kind: SubjectKind::Project,
            id: project_id.into(),
            label: name.into(),
        }
    }

    pub fn project_repo(project_id: &str, project_name: &str, binding: &str) -> Subject {
        Subject {
            kind: SubjectKind::ProjectRepo,
            id: format!("{project_id}/{binding}"),
            label: format!("{project_name}/{binding}"),
        }
    }
}

impl HostTool {
    pub fn program(self) -> &'static str {
        match self {
            HostTool::Rust => "rustc",
            HostTool::Python => "python3",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppStatus {
    pub app_id: u64,
    pub slug: String,
    pub events: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    pub installed: bool,
    pub installation_id: Option<u64>,
    pub permissions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookDelivery {
    pub delivered_at: Timestamp,
    pub status_code: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookStatus {
    pub url_set: bool,
    pub content_type: Option<String>,
    pub secret_set: bool,
    pub deliveries: Vec<HookDelivery>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeOverlap {
    pub repo: String,
    pub other_project: String,
    pub other_repo: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainDrift {
    pub source: String,
    pub file: String,
    pub line: usize,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostVersion {
    Found(String),
    Missing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepoToolchain {
    pub tool: String,
    pub source: Option<String>,
    pub value: Option<String>,
    pub pinned: Option<String>,
    pub drift: Vec<ToolchainDrift>,
    pub cached: Option<bool>,
    pub host: Option<HostVersion>,
    pub proposal: Option<Fix>,
}

pub fn unsupported_by_adapter() -> Finding {
    Finding::skip("unsupported_by_adapter", json!({}))
}

pub fn app_ok(app: &AppStatus) -> Finding {
    Finding::pass("app_ok", json!({ "app_id": app.app_id, "slug": app.slug }))
}

pub fn scm_failure(code: &str, params: &Value) -> Finding {
    let status = params.get("status").cloned().unwrap_or(Value::Null);
    match code {
        "app_auth_failed" => Finding::fail(
            "app_auth_failed",
            json!({ "status": status }),
            fix(
                "check_app_credentials",
                json!({
                    "app_id": params.get("app_id").cloned().unwrap_or(Value::Null),
                    "key_file": params.get("key_file").cloned().unwrap_or(Value::Null),
                }),
            ),
        ),
        "scm_unavailable" => Finding::fail("scm_unavailable", json!({ "status": status }), None),
        other => check_failed(other),
    }
}

pub fn install_url(web_url: &str, slug: &str) -> String {
    format!(
        "{}/apps/{slug}/installations/new",
        web_url.trim_end_matches('/')
    )
}

pub fn permissions_url(web_url: &str, slug: &str) -> String {
    format!(
        "{}/settings/apps/{slug}/permissions",
        web_url.trim_end_matches('/')
    )
}

fn level(access: &str) -> u8 {
    match access {
        "read" => 1,
        "write" => 2,
        "admin" => 3,
        _ => 0,
    }
}

pub fn access_covers(granted: &str, wanted: &str) -> bool {
    level(granted) >= level(wanted)
}

fn missing_permissions(granted: &BTreeMap<String, String>, wanted: &[(&str, &str)]) -> Vec<Value> {
    wanted
        .iter()
        .filter_map(|(permission, required)| {
            let has = granted.get(*permission);
            (!has.is_some_and(|g| access_covers(g, required)))
                .then(|| json!({ "permission": permission, "required": required, "granted": has }))
        })
        .collect()
}

pub fn installation(
    requirements: &AppRequirements,
    repo: &str,
    web_url: &str,
    slug: &str,
    found: &Installation,
) -> Finding {
    if !found.installed {
        return Finding::fail(
            "app_not_installed",
            json!({ "repo": repo }),
            fix("install_app", json!({ "url": install_url(web_url, slug) })),
        );
    }
    let grant = || {
        fix(
            "grant_app_permissions",
            json!({ "url": permissions_url(web_url, slug) }),
        )
    };
    let required = missing_permissions(&found.permissions, requirements.permissions);
    if !required.is_empty() {
        return Finding::fail(
            "app_permissions_missing",
            json!({ "missing": required }),
            grant(),
        );
    }
    let recommended = missing_permissions(&found.permissions, requirements.recommended);
    if !recommended.is_empty() {
        return Finding::warn(
            "app_permission_recommended",
            json!({ "missing": recommended }),
            grant(),
        );
    }
    Finding::pass(
        "app_installed",
        json!({ "installation_id": found.installation_id, "permissions": found.permissions }),
    )
}

pub fn hook_path(integration_id: &str) -> String {
    format!("/hooks/{integration_id}")
}

pub fn webhook_not_configured() -> Finding {
    Finding::skip("webhook_not_configured", json!({}))
}

fn is_success(status_code: u16) -> bool {
    (200..300).contains(&status_code)
}

pub fn webhook(
    requirements: &AppRequirements,
    integration_id: &str,
    app: &AppStatus,
    hook: &WebhookStatus,
    last_received_at: Option<Timestamp>,
) -> Vec<Finding> {
    let configure = || {
        fix(
            "configure_app_webhook",
            json!({ "url": hook_path(integration_id) }),
        )
    };
    let mut fails = Vec::new();
    if !hook.url_set {
        fails.push(Finding::fail(
            "app_webhook_url_missing",
            json!({}),
            configure(),
        ));
    }
    if !hook.secret_set {
        fails.push(Finding::fail(
            "app_webhook_secret_missing",
            json!({}),
            configure(),
        ));
    }
    if hook.url_set && hook.content_type.as_deref() != Some(WEBHOOK_CONTENT_TYPE) {
        fails.push(Finding::fail(
            "app_webhook_content_type",
            json!({ "content_type": hook.content_type }),
            configure(),
        ));
    }
    if !fails.is_empty() {
        return fails;
    }
    let mut warns = Vec::new();
    let missing: Vec<&str> = requirements
        .events
        .iter()
        .copied()
        .filter(|e| !app.events.iter().any(|have| have == e))
        .collect();
    if !missing.is_empty() {
        warns.push(Finding::warn(
            "app_events_missing",
            json!({ "missing": missing }),
            fix("subscribe_app_events", json!({ "missing": missing })),
        ));
    }
    let failed: Vec<&HookDelivery> = hook
        .deliveries
        .iter()
        .filter(|d| !is_success(d.status_code))
        .collect();
    if let Some(last) = failed.iter().max_by_key(|d| d.delivered_at) {
        warns.push(Finding::warn(
            "webhook_deliveries_failing",
            json!({
                "failed": failed.len(),
                "total": hook.deliveries.len(),
                "last_status_code": last.status_code,
                "last_delivered_at": last.delivered_at.to_rfc3339(),
            }),
            configure(),
        ));
    }
    if !warns.is_empty() {
        return warns;
    }
    vec![Finding::pass(
        "webhook_ok",
        json!({
            "deliveries_24h": hook.deliveries.len(),
            "last_received_at": last_received_at.map(Timestamp::to_rfc3339),
        }),
    )]
}

pub fn link_command(integration_id: &str) -> String {
    format!("saka scm link {integration_id}")
}

pub fn device_flow_unconfigured() -> Finding {
    Finding::skip("device_flow_unconfigured", json!({}))
}

pub fn user_link(
    integration_id: &str,
    link: Option<&UserLink>,
    last_failure: Option<AuthorizationFailure>,
    user_mode_projects: &[String],
    now: Timestamp,
) -> Vec<Finding> {
    let relink = || command("run_scm_link", &link_command(integration_id));
    match link {
        Some(l) if !l.is_active() => {
            let cause = l.problem();
            return vec![Finding::fail(
                "user_link_needs_relink",
                json!({ "cause": cause }),
                relink(),
            )];
        }
        None if !user_mode_projects.is_empty() => {
            return vec![Finding::fail(
                "user_link_missing",
                json!({ "projects": user_mode_projects }),
                relink(),
            )];
        }
        _ => {}
    }
    let mut warns = Vec::new();
    if let Some(l) = link
        && l.refresh_expires_at.0.saturating_sub(now.0) < EXPIRY_WARNING_MS
    {
        warns.push(Finding::warn(
            "user_link_expiring",
            json!({ "login": l.login, "refresh_expires_at": l.refresh_expires_at.to_rfc3339() }),
            relink(),
        ));
    }
    if last_failure == Some(AuthorizationFailure::DeviceFlowDisabled) {
        warns.push(Finding::warn(
            "device_flow_disabled",
            json!({}),
            fix("enable_device_flow", json!({})),
        ));
    }
    if !warns.is_empty() {
        return warns;
    }
    vec![match link {
        Some(l) => Finding::pass(
            "user_link_active",
            json!({ "login": l.login, "refresh_expires_at": l.refresh_expires_at.to_rfc3339() }),
        ),
        None => Finding::skip("user_link_absent", json!({})),
    }]
}

fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b'@'));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

pub fn user_token_command(project_name: &str) -> String {
    format!("saka project user-token set {}", shell_word(project_name))
}

pub fn user_token(project_name: &str, token: Option<&ProjectUserToken>, now: Timestamp) -> Finding {
    let Some(t) = token else {
        return Finding::skip("user_token_absent", json!({}));
    };
    let replace = || command("replace_user_token", &user_token_command(project_name));
    if let Some(cause) = t.problem(now) {
        return Finding::fail("user_token_invalid", json!({ "cause": cause }), replace());
    }
    let params = json!({
        "login": t.identity.login,
        "expires_at": t.expires_at.map(Timestamp::to_rfc3339),
    });
    match t.expires_at {
        Some(at) if at.0.saturating_sub(now.0) < EXPIRY_WARNING_MS => {
            Finding::warn("user_token_expiring", params, replace())
        }
        _ => Finding::pass("user_token_active", params),
    }
}

pub fn project_config(project: &Project, overlaps: &[ScopeOverlap]) -> Vec<Finding> {
    if let Some(e) = &project.config_error {
        return vec![Finding::fail(
            "project_config_rejected",
            json!({ "code": e.code, "commit": e.commit, "params": e.params }),
            fix(
                "fix_saka_toml",
                json!({ "path": project_config_path(&project.project_path) }),
            ),
        )];
    }
    if overlaps.is_empty() {
        return vec![Finding::pass(
            "project_config_ok",
            json!({ "config_commit": project.config_commit }),
        )];
    }
    overlaps
        .iter()
        .map(|o| {
            Finding::warn(
                "path_scope_overlap",
                json!({ "repo": o.repo, "other_project": o.other_project, "other_repo": o.other_repo }),
                fix("narrow_path_scope", json!({ "repo": o.repo })),
            )
        })
        .collect()
}

pub fn repo_unreachable(status: Option<u16>) -> Finding {
    Finding::skip("repo_unreachable", json!({ "status": status }))
}

pub fn host_version_of(output: &str) -> Option<String> {
    output
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)
        .filter(|v| v.starts_with(|c: char| c.is_ascii_digit()))
        .map(String::from)
}

pub fn host_matches(declared: &str, found: &str) -> bool {
    found == declared
        || found
            .strip_prefix(declared)
            .is_some_and(|rest| rest.starts_with('.'))
}

pub fn toolchain_repo(tools: &[RepoToolchain]) -> Vec<Finding> {
    let fails: Vec<Finding> = tools
        .iter()
        .filter(|t| t.pinned.is_none())
        .map(|t| {
            Finding::fail(
                "toolchain_unpinned",
                json!({ "tool": t.tool, "source": t.source, "value": t.value }),
                t.proposal
                    .clone()
                    .or_else(|| command("run_saka_init", SAKA_INIT_COMMAND)),
            )
        })
        .collect();
    if !fails.is_empty() {
        return fails;
    }
    let mut warns = Vec::new();
    let mut pinned = Vec::new();
    for t in tools {
        let Some(version) = &t.pinned else { continue };
        if !t.drift.is_empty() {
            warns.push(Finding::warn(
                "toolchain_drift",
                json!({
                    "tool": t.tool,
                    "pinned": version,
                    "drift": t.drift.iter().map(|d| json!({
                        "source": d.source, "file": d.file, "line": d.line, "value": d.value,
                    })).collect::<Vec<_>>(),
                }),
                None,
            ));
        }
        let host_ok = match &t.host {
            Some(HostVersion::Missing) => {
                warns.push(Finding::warn(
                    "host_toolchain_missing",
                    json!({ "tool": t.tool, "declared": version }),
                    None,
                ));
                false
            }
            Some(HostVersion::Found(found)) if !host_matches(version, found) => {
                warns.push(Finding::warn(
                    "host_toolchain_mismatch",
                    json!({ "tool": t.tool, "declared": version, "found": found }),
                    None,
                ));
                false
            }
            Some(HostVersion::Found(_)) => true,
            None => false,
        };
        pinned.push(json!({
            "tool": t.tool,
            "version": version,
            "installed": t.cached.unwrap_or(host_ok),
        }));
    }
    if !warns.is_empty() {
        return warns;
    }
    vec![Finding::pass(
        "toolchains_pinned",
        json!({ "toolchains": pinned }),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP: AppRequirements = AppRequirements {
        permissions: &[
            ("contents", "write"),
            ("pull_requests", "write"),
            ("checks", "read"),
            ("statuses", "read"),
            ("metadata", "read"),
        ],
        recommended: &[("statuses", "write")],
        events: &["pull_request", "check_run", "check_suite", "status"],
    };

    fn quota(window: &str, utilization: f64, resets_at: Option<i64>) -> AccountQuota {
        AccountQuota {
            account: "claude:default".into(),
            window: window.into(),
            utilization,
            status: "allowed".into(),
            resets_at: resets_at.map(Timestamp),
            observed_at: Timestamp(1),
        }
    }

    type Row<'a> = (DiagnosticStatus, &'a str, Value, Option<(&'a str, Value)>);

    fn row(f: &Finding) -> Row<'_> {
        (
            f.status,
            f.code.as_str(),
            f.params.clone(),
            f.fix.as_ref().map(|x| (x.code.as_str(), x.params.clone())),
        )
    }

    const ACCOUNT: &str = "claude:default";

    #[test]
    fn every_step_one_catalog_row_yields_its_status_code_params_and_fix() {
        let cli = |found: Option<FoundCli>| AgentCli {
            command: "claude".into(),
            path_dirs: vec!["/usr/bin".into(), "/bin".into()],
            found,
        };
        use DiagnosticStatus::{Fail, Pass, Skip, Warn};
        let cases: Vec<(Finding, Row<'_>)> = vec![
            (
                config_ok("/c.toml"),
                (Pass, "config_ok", json!({"path": "/c.toml"}), None),
            ),
            (
                config_invalid("/c.toml", "config_unknown_field", "unknown field `x`"),
                (
                    Fail,
                    "config_invalid",
                    json!({"code": "config_unknown_field", "detail": "unknown field `x`"}),
                    Some(("fix_config", json!({"path": "/c.toml"}))),
                ),
            ),
            (config_absent(), (Skip, "config_absent", json!({}), None)),
            (
                server_reachable("http://s"),
                (
                    Pass,
                    "server_reachable",
                    json!({"server": "http://s"}),
                    None,
                ),
            ),
            (
                server_unreachable("http://s", "connection refused"),
                (
                    Fail,
                    "server_unreachable",
                    json!({"server": "http://s", "reason": "connection refused"}),
                    Some(("start_server", json!({"command": "saka serve"}))),
                ),
            ),
            (
                api_version("0.1.0", "1", "1", "1"),
                (
                    Pass,
                    "api_compatible",
                    json!({"server_version": "0.1.0", "api_version": "1"}),
                    None,
                ),
            ),
            (
                api_version("0.9.0", "2", "2", "1"),
                (
                    Fail,
                    "api_version_mismatch",
                    json!({"server": "2", "client": "1"}),
                    Some(("upgrade_saka", json!({}))),
                ),
            ),
            (
                skipped_unreachable("http://s"),
                (
                    Skip,
                    "server_unreachable",
                    json!({"server": "http://s"}),
                    None,
                ),
            ),
            (server_ready(&[]), (Pass, "server_ready", json!({}), None)),
            (
                server_ready(&["worker", "reconciler"]),
                (
                    Fail,
                    "server_not_ready",
                    json!({"failing": ["worker", "reconciler"]}),
                    None,
                ),
            ),
            (
                migrations(MigrationState {
                    applied: 17,
                    latest: 17,
                }),
                (Pass, "migrations_current", json!({"applied": 17}), None),
            ),
            (
                migrations(MigrationState {
                    applied: 0,
                    latest: 17,
                }),
                (
                    Fail,
                    "migrations_pending",
                    json!({"applied": 0, "latest": 17}),
                    Some(("run_migrate", json!({"command": "saka migrate"}))),
                ),
            ),
            (
                migrations(MigrationState {
                    applied: 18,
                    latest: 17,
                }),
                (
                    Fail,
                    "migrations_ahead",
                    json!({"applied": 18, "latest": 17}),
                    Some(("upgrade_saka", json!({}))),
                ),
            ),
            (
                supervisor(&SupervisorProbe::Available),
                (Pass, "systemd_user_ok", json!({}), None),
            ),
            (
                supervisor(&SupervisorProbe::Unavailable {
                    exit_code: Some(1),
                    detail: "Failed to connect to bus".into(),
                }),
                (
                    Fail,
                    "systemd_user_unavailable",
                    json!({"exit_code": 1, "detail": "Failed to connect to bus"}),
                    Some((
                        "enable_systemd_user",
                        json!({"command": "loginctl enable-linger"}),
                    )),
                ),
            ),
            (
                agent_binary(
                    ACCOUNT,
                    &cli(Some(FoundCli {
                        path: "/usr/bin/claude".into(),
                        version: Some("2.1.0".into()),
                    })),
                ),
                (
                    Pass,
                    "agent_cli_found",
                    json!({"command": "claude", "path": "/usr/bin/claude", "version": "2.1.0"}),
                    None,
                ),
            ),
            (
                agent_binary(ACCOUNT, &cli(None)),
                (
                    Fail,
                    "agent_cli_not_found",
                    json!({"command": "claude", "path_dirs": ["/usr/bin", "/bin"]}),
                    Some(("install_agent_cli", json!({"cli": "claude"}))),
                ),
            ),
            (
                agent_login(
                    ACCOUNT,
                    &AgentLogin::LoggedIn {
                        auth_method: "claude.ai".into(),
                    },
                ),
                (
                    Pass,
                    "agent_logged_in",
                    json!({"account": ACCOUNT, "auth_method": "claude.ai"}),
                    None,
                ),
            ),
            (
                agent_login(ACCOUNT, &AgentLogin::NotLoggedIn),
                (
                    Fail,
                    "agent_not_logged_in",
                    json!({"account": ACCOUNT}),
                    Some(("run_agent_login", json!({"command": "saka agent login"}))),
                ),
            ),
            (
                agent_login(ACCOUNT, &AgentLogin::Unavailable { exit_code: Some(3) }),
                (
                    Fail,
                    "agent_status_unavailable",
                    json!({"account": ACCOUNT, "exit_code": 3}),
                    Some(("upgrade_agent_cli", json!({"cli": "claude"}))),
                ),
            ),
            (
                agent_login(
                    ACCOUNT,
                    &AgentLogin::CliNotFound {
                        command: "claude".into(),
                    },
                ),
                (
                    Skip,
                    "agent_cli_not_found",
                    json!({"command": "claude"}),
                    None,
                ),
            ),
            (
                agent_quota(ACCOUNT, &[], 0.9, Timestamp(1_000)),
                (Skip, "quota_unknown", json!({"account": ACCOUNT}), None),
            ),
            (
                agent_quota(
                    ACCOUNT,
                    &[
                        quota("seven_day", 0.5, Some(9_000)),
                        quota("five_hour", 0.95, Some(500)),
                    ],
                    0.9,
                    Timestamp(1_000),
                ),
                (
                    Pass,
                    "quota_ok",
                    json!({"windows": [
                        {"window": "five_hour", "utilization": 0.95, "resets_at": "1970-01-01T00:00:00.500Z"},
                        {"window": "seven_day", "utilization": 0.5, "resets_at": "1970-01-01T00:00:09.000Z"},
                    ]}),
                    None,
                ),
            ),
            (
                agent_quota(
                    ACCOUNT,
                    &[
                        quota("five_hour", 0.9, Some(5_000)),
                        quota("seven_day", 0.97, Some(9_000)),
                    ],
                    0.9,
                    Timestamp(1_000),
                ),
                (
                    Warn,
                    "quota_paused",
                    json!({"account": ACCOUNT, "window": "seven_day", "utilization": 0.97, "resets_at": "1970-01-01T00:00:09.000Z"}),
                    None,
                ),
            ),
            (
                toolchain_cache(&ToolchainCache {
                    path: "/d/toolchains".into(),
                    writable: true,
                    installed: vec![
                        InstalledToolchain {
                            tool: Tool::Node,
                            version: "22.1.0".into(),
                        },
                        InstalledToolchain {
                            tool: Tool::Go,
                            version: "1.26.1".into(),
                        },
                    ],
                }),
                (
                    Pass,
                    "toolchain_cache_ok",
                    json!({"path": "/d/toolchains", "installed": [
                        {"tool": "go", "version": "1.26.1"}, {"tool": "node", "version": "22.1.0"},
                    ]}),
                    None,
                ),
            ),
            (
                toolchain_cache(&ToolchainCache {
                    path: "/d/toolchains".into(),
                    writable: false,
                    installed: vec![],
                }),
                (
                    Fail,
                    "toolchain_cache_unwritable",
                    json!({"path": "/d/toolchains"}),
                    None,
                ),
            ),
            (
                timed_out(10_000),
                (Fail, "check_timed_out", json!({"timeout_ms": 10_000}), None),
            ),
            (
                check_failed("store_failed"),
                (Fail, "check_failed", json!({"error": "store_failed"}), None),
            ),
            (
                engine_not_ready(),
                (Skip, "engine_not_ready", json!({}), None),
            ),
        ];
        for (finding, expected) in &cases {
            assert_eq!(row(finding), expected.clone());
        }
    }

    #[test]
    fn a_quota_at_the_threshold_pauses_until_its_window_resets() {
        let at = |u: f64, resets: i64| {
            agent_quota(
                ACCOUNT,
                &[quota("five_hour", u, Some(resets))],
                0.9,
                Timestamp(1_000),
            )
            .status
        };
        assert_eq!(at(0.9, 2_000), DiagnosticStatus::Warn);
        assert_eq!(at(0.89, 2_000), DiagnosticStatus::Pass);
        assert_eq!(
            at(0.99, 1_000),
            DiagnosticStatus::Pass,
            "a window that has reset no longer pauses"
        );
    }

    #[test]
    fn toolchain_sources_warn_once_per_unreachable_tool() {
        let ok = toolchain_sources(&[(Tool::Go, None), (Tool::Node, None), (Tool::Pnpm, None)]);
        assert_eq!(ok.len(), 1);
        assert_eq!(
            row(&ok[0]),
            (
                DiagnosticStatus::Pass,
                "toolchain_sources_ok",
                json!({"tools": ["go", "node", "pnpm"]}),
                None
            )
        );
        let failure = SourceFailure {
            url: Some("https://nodejs.org/dist".into()),
            status: Some(500),
        };
        let node = toolchain_sources(&[
            (Tool::Go, None),
            (Tool::Node, Some(failure)),
            (Tool::Pnpm, None),
        ]);
        assert_eq!(node.len(), 1);
        assert_eq!(
            row(&node[0]),
            (
                DiagnosticStatus::Warn,
                "toolchain_source_unreachable",
                json!({"tool": "node", "url": "https://nodejs.org/dist", "status": 500}),
                Some((
                    "check_toolchain_mirror",
                    json!({"key": "toolchain.node_dist_url"})
                )),
            )
        );
    }

    fn result(id: CheckId, label: &str, status: DiagnosticStatus) -> CheckResult {
        CheckResult {
            id,
            subject: Subject {
                kind: id.subject_kind(),
                id: label.into(),
                label: label.into(),
            },
            finding: Finding::of(status, "x", json!({}), None),
            duration_ms: 0,
        }
    }

    #[test]
    fn a_report_orders_checks_by_catalog_then_label_and_counts_each_status() {
        let report = Report::new(
            Timestamp(0),
            "0.1.0",
            ReportMode::Online,
            vec![
                result(
                    CheckId::ToolchainSources,
                    "instance",
                    DiagnosticStatus::Warn,
                ),
                result(CheckId::AgentLogin, "claude:b", DiagnosticStatus::Fail),
                result(CheckId::AgentLogin, "claude:a", DiagnosticStatus::Pass),
                result(CheckId::ServerReady, "instance", DiagnosticStatus::Pass),
                result(CheckId::AgentQuota, "claude:a", DiagnosticStatus::Skip),
            ],
        );
        let order: Vec<(CheckId, &str)> = report
            .checks
            .iter()
            .map(|c| (c.id, c.subject.label.as_str()))
            .collect();
        assert_eq!(
            order,
            [
                (CheckId::ServerReady, "instance"),
                (CheckId::AgentLogin, "claude:a"),
                (CheckId::AgentLogin, "claude:b"),
                (CheckId::AgentQuota, "claude:a"),
                (CheckId::ToolchainSources, "instance"),
            ]
        );
        assert_eq!(
            report.summary,
            Summary {
                pass: 2,
                warn: 1,
                fail: 1,
                skip: 1
            }
        );
    }

    const DAY: i64 = 86_400_000;
    const NOW: Timestamp = Timestamp(1_800_000_000_000);

    fn link(state: crate::vocab::UserLinkState, refresh_in_days: i64) -> UserLink {
        UserLink {
            id: crate::ids::UserLinkId(uuid::Uuid::nil()),
            owner: "operator:local".into(),
            integration_id: "github".into(),
            provider_user_id: 1,
            login: "octo".into(),
            display_name: None,
            email: "1+octo@users.noreply.github.com".into(),
            state,
            relink_cause: Some(crate::vocab::RelinkCause::Revoked),
            access_expires_at: NOW.plus_ms(DAY),
            refresh_expires_at: NOW.plus_ms(refresh_in_days * DAY),
            linked_at: NOW,
            refreshed_at: None,
            created_at: NOW,
            updated_at: NOW,
        }
    }

    fn pat(expires_in_days: Option<i64>) -> ProjectUserToken {
        ProjectUserToken {
            integration_id: "github".into(),
            identity: crate::authorship::Identity {
                provider_user_id: 7,
                login: "ann".into(),
                display_name: None,
                email: "ann@example.com".into(),
            },
            expires_at: expires_in_days.map(|d| NOW.plus_ms(d * DAY)),
            set_at: NOW,
            invalid_cause: None,
        }
    }

    fn project(config_error: Option<crate::model::ConfigError>) -> Project {
        Project {
            id: crate::ids::ProjectId(uuid::Uuid::nil()),
            name: "shop".into(),
            workspace_repo_id: crate::ids::RepoId(uuid::Uuid::nil()),
            project_path: "docs".into(),
            base_branch: "main".into(),
            config_commit: "c0ffee".into(),
            config_sha256: "sha256:0".into(),
            config_error,
            autonomy: crate::vocab::Autonomy::Standard,
            workflow_preset: crate::vocab::WorkflowPreset::FullLoop,
            repos: Vec::new(),
            created_at: NOW,
            updated_at: NOW,
        }
    }

    fn granted(pairs: &[(&str, &str)]) -> Installation {
        Installation {
            installed: true,
            installation_id: Some(42),
            permissions: pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    const FULL: &[(&str, &str)] = &[
        ("contents", "write"),
        ("pull_requests", "write"),
        ("checks", "read"),
        ("statuses", "write"),
        ("metadata", "read"),
    ];

    fn app(events: &[&str]) -> AppStatus {
        AppStatus {
            app_id: 9,
            slug: "saka-dev".into(),
            events: events.iter().map(|e| (*e).to_string()).collect(),
        }
    }

    fn hook(deliveries: Vec<HookDelivery>) -> WebhookStatus {
        WebhookStatus {
            url_set: true,
            content_type: Some("json".into()),
            secret_set: true,
            deliveries,
        }
    }

    fn delivery(minutes_ago: i64, status_code: u16) -> HookDelivery {
        HookDelivery {
            delivered_at: NOW.plus_ms(-minutes_ago * 60_000),
            status_code,
        }
    }

    fn pinned(tool: &str, version: &str) -> RepoToolchain {
        RepoToolchain {
            tool: tool.into(),
            source: Some("nvmrc".into()),
            value: Some(version.into()),
            pinned: Some(version.into()),
            drift: Vec::new(),
            cached: Some(false),
            host: None,
            proposal: None,
        }
    }

    fn rows(findings: &[Finding]) -> Vec<Row<'_>> {
        findings.iter().map(row).collect()
    }

    #[test]
    fn every_step_two_catalog_row_yields_its_status_code_params_and_fix() {
        use DiagnosticStatus::{Fail, Pass, Skip, Warn};
        const WEB: &str = "https://github.com";
        let grant = Some((
            "grant_app_permissions",
            json!({"url": "https://github.com/settings/apps/saka-dev/permissions"}),
        ));
        let configure = Some(("configure_app_webhook", json!({"url": "/hooks/github"})));
        let relink = Some(("run_scm_link", json!({"command": "saka scm link github"})));
        let replace = Some((
            "replace_user_token",
            json!({"command": "saka project user-token set shop"}),
        ));
        let all_events = ["pull_request", "check_run", "check_suite", "status"];
        let cases: Vec<(Vec<Finding>, Vec<Row<'_>>)> = vec![
            (
                vec![app_ok(&app(&all_events))],
                vec![(
                    Pass,
                    "app_ok",
                    json!({"app_id": 9, "slug": "saka-dev"}),
                    None,
                )],
            ),
            (
                vec![scm_failure(
                    "app_auth_failed",
                    &json!({"status": 401, "app_id": 9, "key_file": "/etc/saka/app.pem"}),
                )],
                vec![(
                    Fail,
                    "app_auth_failed",
                    json!({"status": 401}),
                    Some((
                        "check_app_credentials",
                        json!({"app_id": 9, "key_file": "/etc/saka/app.pem"}),
                    )),
                )],
            ),
            (
                vec![scm_failure(
                    "scm_unavailable",
                    &json!({"status": 502, "detail": "server_error"}),
                )],
                vec![(Fail, "scm_unavailable", json!({"status": 502}), None)],
            ),
            (
                vec![scm_failure(
                    "scm_integration_unknown",
                    &json!({"integration_id": "x"}),
                )],
                vec![(
                    Fail,
                    "check_failed",
                    json!({"error": "scm_integration_unknown"}),
                    None,
                )],
            ),
            (
                vec![unsupported_by_adapter()],
                vec![(Skip, "unsupported_by_adapter", json!({}), None)],
            ),
            (
                vec![installation(
                    &APP,
                    "acme/shop",
                    WEB,
                    "saka-dev",
                    &granted(FULL),
                )],
                vec![(
                    Pass,
                    "app_installed",
                    json!({"installation_id": 42, "permissions": {
                        "checks": "read", "contents": "write", "metadata": "read",
                        "pull_requests": "write", "statuses": "write",
                    }}),
                    None,
                )],
            ),
            (
                vec![installation(
                    &APP,
                    "acme/shop",
                    WEB,
                    "saka-dev",
                    &Installation {
                        installed: false,
                        installation_id: None,
                        permissions: BTreeMap::new(),
                    },
                )],
                vec![(
                    Fail,
                    "app_not_installed",
                    json!({"repo": "acme/shop"}),
                    Some((
                        "install_app",
                        json!({"url": "https://github.com/apps/saka-dev/installations/new"}),
                    )),
                )],
            ),
            (
                vec![installation(
                    &APP,
                    "acme/shop",
                    WEB,
                    "saka-dev",
                    &granted(&[
                        ("contents", "write"),
                        ("pull_requests", "read"),
                        ("checks", "read"),
                        ("statuses", "write"),
                        ("metadata", "read"),
                    ]),
                )],
                vec![(
                    Fail,
                    "app_permissions_missing",
                    json!({"missing": [{"permission": "pull_requests", "required": "write", "granted": "read"}]}),
                    grant.clone(),
                )],
            ),
            (
                vec![installation(
                    &APP,
                    "acme/shop",
                    WEB,
                    "saka-dev",
                    &granted(&[
                        ("contents", "admin"),
                        ("pull_requests", "write"),
                        ("checks", "write"),
                        ("statuses", "read"),
                        ("metadata", "read"),
                    ]),
                )],
                vec![(
                    Warn,
                    "app_permission_recommended",
                    json!({"missing": [{"permission": "statuses", "required": "write", "granted": "read"}]}),
                    grant.clone(),
                )],
            ),
            (
                vec![installation(
                    &APP,
                    "acme/shop",
                    WEB,
                    "saka-dev",
                    &granted(&[("contents", "write"), ("pull_requests", "write")]),
                )],
                vec![(
                    Fail,
                    "app_permissions_missing",
                    json!({"missing": [
                        {"permission": "checks", "required": "read", "granted": null},
                        {"permission": "statuses", "required": "read", "granted": null},
                        {"permission": "metadata", "required": "read", "granted": null},
                    ]}),
                    grant.clone(),
                )],
            ),
            (
                webhook(
                    &APP,
                    "github",
                    &app(&all_events),
                    &hook(vec![delivery(5, 200)]),
                    Some(NOW),
                ),
                vec![(
                    Pass,
                    "webhook_ok",
                    json!({"deliveries_24h": 1, "last_received_at": NOW.to_rfc3339()}),
                    None,
                )],
            ),
            (
                webhook(
                    &APP,
                    "github",
                    &app(&["pull_request", "check_run", "check_suite"]),
                    &hook(Vec::new()),
                    None,
                ),
                vec![(
                    Warn,
                    "app_events_missing",
                    json!({"missing": ["status"]}),
                    Some(("subscribe_app_events", json!({"missing": ["status"]}))),
                )],
            ),
            (
                webhook(
                    &APP,
                    "github",
                    &app(&all_events),
                    &hook(
                        (0..10)
                            .map(|n| {
                                delivery(60 * (10 - n), if n % 3 == 0 && n > 0 { 500 } else { 200 })
                            })
                            .collect(),
                    ),
                    None,
                ),
                vec![(
                    Warn,
                    "webhook_deliveries_failing",
                    json!({
                        "failed": 3, "total": 10, "last_status_code": 500,
                        "last_delivered_at": NOW.plus_ms(-60 * 60_000).to_rfc3339(),
                    }),
                    configure.clone(),
                )],
            ),
            (
                webhook(
                    &APP,
                    "github",
                    &app(&[]),
                    &WebhookStatus {
                        url_set: false,
                        content_type: None,
                        secret_set: false,
                        deliveries: Vec::new(),
                    },
                    None,
                ),
                vec![
                    (
                        Fail,
                        "app_webhook_url_missing",
                        json!({}),
                        configure.clone(),
                    ),
                    (
                        Fail,
                        "app_webhook_secret_missing",
                        json!({}),
                        configure.clone(),
                    ),
                ],
            ),
            (
                webhook(
                    &APP,
                    "github",
                    &app(&all_events),
                    &WebhookStatus {
                        content_type: Some("form".into()),
                        ..hook(Vec::new())
                    },
                    None,
                ),
                vec![(
                    Fail,
                    "app_webhook_content_type",
                    json!({"content_type": "form"}),
                    configure.clone(),
                )],
            ),
            (
                vec![webhook_not_configured()],
                vec![(Skip, "webhook_not_configured", json!({}), None)],
            ),
            (
                user_link(
                    "github",
                    Some(&link(crate::vocab::UserLinkState::Active, 30)),
                    None,
                    &[],
                    NOW,
                ),
                vec![(
                    Pass,
                    "user_link_active",
                    json!({"login": "octo", "refresh_expires_at": NOW.plus_ms(30 * DAY).to_rfc3339()}),
                    None,
                )],
            ),
            (
                user_link(
                    "github",
                    Some(&link(crate::vocab::UserLinkState::Active, 6)),
                    None,
                    &[],
                    NOW,
                ),
                vec![(
                    Warn,
                    "user_link_expiring",
                    json!({"login": "octo", "refresh_expires_at": NOW.plus_ms(6 * DAY).to_rfc3339()}),
                    relink.clone(),
                )],
            ),
            (
                user_link(
                    "github",
                    Some(&link(crate::vocab::UserLinkState::NeedsRelink, 30)),
                    None,
                    &["shop".into()],
                    NOW,
                ),
                vec![(
                    Fail,
                    "user_link_needs_relink",
                    json!({"cause": "revoked"}),
                    relink.clone(),
                )],
            ),
            (
                user_link("github", None, None, &["shop".into(), "web".into()], NOW),
                vec![(
                    Fail,
                    "user_link_missing",
                    json!({"projects": ["shop", "web"]}),
                    relink.clone(),
                )],
            ),
            (
                user_link("github", None, None, &[], NOW),
                vec![(Skip, "user_link_absent", json!({}), None)],
            ),
            (
                user_link(
                    "github",
                    None,
                    Some(AuthorizationFailure::DeviceFlowDisabled),
                    &[],
                    NOW,
                ),
                vec![(
                    Warn,
                    "device_flow_disabled",
                    json!({}),
                    Some(("enable_device_flow", json!({}))),
                )],
            ),
            (
                vec![device_flow_unconfigured()],
                vec![(Skip, "device_flow_unconfigured", json!({}), None)],
            ),
            (
                vec![user_token("shop", Some(&pat(Some(30))), NOW)],
                vec![(
                    Pass,
                    "user_token_active",
                    json!({"login": "ann", "expires_at": NOW.plus_ms(30 * DAY).to_rfc3339()}),
                    None,
                )],
            ),
            (
                vec![user_token("shop", Some(&pat(None)), NOW)],
                vec![(
                    Pass,
                    "user_token_active",
                    json!({"login": "ann", "expires_at": null}),
                    None,
                )],
            ),
            (
                vec![user_token("shop", Some(&pat(Some(3))), NOW)],
                vec![(
                    Warn,
                    "user_token_expiring",
                    json!({"login": "ann", "expires_at": NOW.plus_ms(3 * DAY).to_rfc3339()}),
                    replace.clone(),
                )],
            ),
            (
                vec![user_token(
                    "shop",
                    Some(&ProjectUserToken {
                        invalid_cause: Some(crate::vocab::TokenInvalidCause::Rejected),
                        ..pat(Some(30))
                    }),
                    NOW,
                )],
                vec![(
                    Fail,
                    "user_token_invalid",
                    json!({"cause": "rejected"}),
                    replace.clone(),
                )],
            ),
            (
                vec![user_token("shop", Some(&pat(Some(-1))), NOW)],
                vec![(
                    Fail,
                    "user_token_invalid",
                    json!({"cause": "expired"}),
                    replace.clone(),
                )],
            ),
            (
                vec![user_token("shop", None, NOW)],
                vec![(Skip, "user_token_absent", json!({}), None)],
            ),
            (
                project_config(&project(None), &[]),
                vec![(
                    Pass,
                    "project_config_ok",
                    json!({"config_commit": "c0ffee"}),
                    None,
                )],
            ),
            (
                project_config(
                    &project(Some(crate::model::ConfigError {
                        commit: "bad0".into(),
                        code: "config_invalid".into(),
                        params: json!({"issues": [{"code": "unknown_field"}]}),
                        observed_at: NOW,
                    })),
                    &[ScopeOverlap {
                        repo: "api".into(),
                        other_project: "web".into(),
                        other_repo: "api".into(),
                    }],
                ),
                vec![(
                    Fail,
                    "project_config_rejected",
                    json!({"code": "config_invalid", "commit": "bad0", "params": {"issues": [{"code": "unknown_field"}]}}),
                    Some(("fix_saka_toml", json!({"path": "docs/saka.toml"}))),
                )],
            ),
            (
                project_config(
                    &project(None),
                    &[
                        ScopeOverlap {
                            repo: "api".into(),
                            other_project: "web".into(),
                            other_repo: "backend".into(),
                        },
                        ScopeOverlap {
                            repo: "ui".into(),
                            other_project: "web".into(),
                            other_repo: "ui".into(),
                        },
                    ],
                ),
                vec![
                    (
                        Warn,
                        "path_scope_overlap",
                        json!({"repo": "api", "other_project": "web", "other_repo": "backend"}),
                        Some(("narrow_path_scope", json!({"repo": "api"}))),
                    ),
                    (
                        Warn,
                        "path_scope_overlap",
                        json!({"repo": "ui", "other_project": "web", "other_repo": "ui"}),
                        Some(("narrow_path_scope", json!({"repo": "ui"}))),
                    ),
                ],
            ),
            (
                vec![repo_unreachable(Some(404))],
                vec![(Skip, "repo_unreachable", json!({"status": 404}), None)],
            ),
        ];
        for (findings, expected) in &cases {
            assert_eq!(&rows(findings), expected);
        }
    }

    #[test]
    fn toolchain_repo_fails_unpinned_tools_first_then_warns_then_passes_once() {
        use DiagnosticStatus::{Fail, Pass, Warn};
        let node = pinned("node", "22.11.0");
        assert_eq!(
            rows(&toolchain_repo(std::slice::from_ref(&node))),
            vec![(
                Pass,
                "toolchains_pinned",
                json!({"toolchains": [{"tool": "node", "version": "22.11.0", "installed": false}]}),
                None,
            )]
        );
        let unpinned = RepoToolchain {
            pinned: None,
            source: Some("nvmrc".into()),
            value: Some("22".into()),
            proposal: Some(Fix {
                code: "write_version_file".into(),
                params: json!({"path": ".nvmrc", "version": "22.11.0"}),
            }),
            ..node.clone()
        };
        let python = RepoToolchain {
            tool: "python".into(),
            proposal: None,
            value: Some(">=3.11".into()),
            source: Some("pyproject_requires_python".into()),
            ..unpinned.clone()
        };
        let drifted = RepoToolchain {
            drift: vec![ToolchainDrift {
                source: "dockerfile_from".into(),
                file: "Dockerfile".into(),
                line: 1,
                value: "1.24".into(),
            }],
            ..pinned("go", "1.26.1")
        };
        assert_eq!(
            rows(&toolchain_repo(&[
                unpinned.clone(),
                drifted.clone(),
                python
            ])),
            vec![
                (
                    Fail,
                    "toolchain_unpinned",
                    json!({"tool": "node", "source": "nvmrc", "value": "22"}),
                    Some((
                        "write_version_file",
                        json!({"path": ".nvmrc", "version": "22.11.0"})
                    )),
                ),
                (
                    Fail,
                    "toolchain_unpinned",
                    json!({"tool": "python", "source": "pyproject_requires_python", "value": ">=3.11"}),
                    Some(("run_saka_init", json!({"command": "saka init"}))),
                ),
            ],
            "fails hide the warnings"
        );
        let rust = |host: HostVersion| RepoToolchain {
            cached: None,
            host: Some(host),
            ..pinned("rust", "1.80.0")
        };
        assert_eq!(
            rows(&toolchain_repo(&[
                drifted,
                rust(HostVersion::Found("1.99.1".into())),
                RepoToolchain {
                    tool: "python".into(),
                    ..rust(HostVersion::Missing)
                },
            ])),
            vec![
                (
                    Warn,
                    "toolchain_drift",
                    json!({"tool": "go", "pinned": "1.26.1", "drift": [
                        {"source": "dockerfile_from", "file": "Dockerfile", "line": 1, "value": "1.24"},
                    ]}),
                    None,
                ),
                (
                    Warn,
                    "host_toolchain_mismatch",
                    json!({"tool": "rust", "declared": "1.80.0", "found": "1.99.1"}),
                    None,
                ),
                (
                    Warn,
                    "host_toolchain_missing",
                    json!({"tool": "python", "declared": "1.80.0"}),
                    None,
                ),
            ]
        );
        assert_eq!(
            rows(&toolchain_repo(&[
                rust(HostVersion::Found("1.80.0".into())),
                RepoToolchain {
                    cached: Some(true),
                    ..node
                },
            ])),
            vec![(
                Pass,
                "toolchains_pinned",
                json!({"toolchains": [
                    {"tool": "rust", "version": "1.80.0", "installed": true},
                    {"tool": "node", "version": "22.11.0", "installed": true},
                ]}),
                None,
            )]
        );
        assert_eq!(
            rows(&toolchain_repo(&[])),
            vec![(Pass, "toolchains_pinned", json!({"toolchains": []}), None)]
        );
    }

    #[test]
    fn host_versions_are_read_from_the_first_line_and_match_by_prefix() {
        assert_eq!(
            host_version_of("rustc 1.99.1 (4d215e2 2026-09-01)\n").as_deref(),
            Some("1.99.1")
        );
        assert_eq!(
            host_version_of("Python 3.12.4\n").as_deref(),
            Some("3.12.4")
        );
        assert_eq!(host_version_of("command not found"), None);
        assert_eq!(host_version_of(""), None);
        assert!(host_matches("3.12", "3.12.4"));
        assert!(host_matches("1.80.0", "1.80.0"));
        assert!(!host_matches("1.80.0", "1.99.1"));
        assert!(!host_matches("3.1", "3.12.4"));
    }

    #[test]
    fn a_project_name_that_needs_quoting_is_quoted_in_its_command() {
        assert_eq!(
            user_token_command("shop"),
            "saka project user-token set shop"
        );
        assert_eq!(
            user_token_command("Shop app"),
            "saka project user-token set 'Shop app'"
        );
        assert_eq!(
            user_token_command("it's"),
            r"saka project user-token set 'it'\''s'"
        );
    }

    #[test]
    fn check_ids_are_stable_dotted_names_and_client_checks_come_first() {
        let ids: Vec<&str> = CheckId::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            ids,
            [
                "cli.config",
                "server.reachable",
                "server.version",
                "server.ready",
                "db.migrations",
                "runtime.supervisor",
                "agent.binary",
                "agent.login",
                "agent.quota",
                "toolchain.cache",
                "toolchain.sources",
                "scm.app",
                "scm.installation",
                "scm.webhook",
                "scm.user_link",
                "project.user_token",
                "project.config",
                "toolchain.repo",
            ]
        );
        assert!(CheckId::server().all(|c| !c.is_client()));
        let offline: Vec<CheckId> = CheckId::ALL
            .iter()
            .copied()
            .filter(|c| c.runs_offline())
            .collect();
        assert_eq!(
            offline,
            [
                CheckId::DbMigrations,
                CheckId::RuntimeSupervisor,
                CheckId::AgentBinary,
                CheckId::AgentLogin,
                CheckId::ToolchainCache
            ]
        );
    }
}
