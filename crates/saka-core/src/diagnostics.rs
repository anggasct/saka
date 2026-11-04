use serde_json::{Value, json};

use crate::admission::paused_windows;
use crate::model::AccountQuota;
use crate::time::Timestamp;
use crate::vocab::{CheckId, DiagnosticStatus, ReportMode, SubjectKind, Tool};

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

#[cfg(test)]
mod tests {
    use super::*;

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
