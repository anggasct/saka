use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use saka_core::autonomy::Ceilings;
use saka_core::policy::RuleSpec;
use saka_core::vocab::{Autonomy, MergeMethod};
use saka_scm::github::app::client_id_valid;
use serde::Deserialize;

mod notify;

pub use notify::Notify;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {detail}")]
pub struct ConfigError {
    pub code: &'static str,
    pub detail: String,
}

fn err(code: &'static str, detail: impl Into<String>) -> ConfigError {
    ConfigError {
        code,
        detail: detail.into(),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: Server,
    #[serde(default)]
    pub scheduler: Scheduler,
    #[serde(default)]
    pub workspace: Workspace,
    #[serde(default)]
    pub scm: Vec<ScmEntry>,
    #[serde(default)]
    pub agent: Agent,
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub architect: Architect,
    #[serde(default)]
    pub git: Git,
    #[serde(default)]
    pub permission: Permission,
    #[serde(default)]
    pub env: Env,
    #[serde(default)]
    pub toolchain: Toolchain,
    #[serde(default)]
    pub hooks: Option<Hooks>,
    #[serde(default)]
    pub hook: Vec<saka_core::hook::RawHook>,
    #[serde(default)]
    pub deploy: Vec<saka_core::environment::RawEnvironment>,
    #[serde(default)]
    pub incident: IncidentConfig,
    #[serde(default)]
    pub notify: Notify,
    #[serde(default)]
    pub autonomy: AutonomyConfig,
    #[serde(default)]
    pub auth: Option<Auth>,
    #[serde(default)]
    pub workflow: WorkflowConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IncidentConfig {
    #[serde(default)]
    pub source: Vec<saka_core::incident::RawSource>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowConfig {
    #[serde(default)]
    pub allow_unreviewed_chore: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    pub origin: String,
    pub rp_id: String,
    #[serde(default)]
    pub rp_name: Option<String>,
    #[serde(default)]
    pub session_hours: Option<u32>,
}

const RP_NAME_DEFAULT: &str = "Saka";

pub const MAX_AUTONOMY_REPOS: usize = 256;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutonomyConfig {
    #[serde(default = "standard")]
    pub default: Autonomy,
    #[serde(default)]
    pub repo: Vec<AutonomyRepo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutonomyRepo {
    pub repo: String,
    pub level: Autonomy,
}

impl Default for AutonomyConfig {
    fn default() -> AutonomyConfig {
        AutonomyConfig {
            default: standard(),
            repo: Vec::new(),
        }
    }
}

fn standard() -> Autonomy {
    Autonomy::Standard
}

fn repo_full_name_ok(name: &str) -> bool {
    let mut parts = name.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let part_ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    part_ok(owner) && part_ok(repo)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    pub listen: String,
}

pub const WEBHOOK_SECRET_MIN: usize = 32;
pub const WEBHOOK_SECRET_MAX: usize = 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Toolchain {
    #[serde(default = "go_dl_url")]
    pub go_dl_url: String,
    #[serde(default = "node_dist_url")]
    pub node_dist_url: String,
    #[serde(default = "npm_registry_url")]
    pub npm_registry_url: String,
    #[serde(default = "six_hundred")]
    pub download_timeout_secs: u64,
}

impl Default for Toolchain {
    fn default() -> Toolchain {
        Toolchain {
            go_dl_url: go_dl_url(),
            node_dist_url: node_dist_url(),
            npm_registry_url: npm_registry_url(),
            download_timeout_secs: six_hundred(),
        }
    }
}

fn go_dl_url() -> String {
    "https://go.dev/dl".into()
}
fn node_dist_url() -> String {
    "https://nodejs.org/dist".into()
}
fn npm_registry_url() -> String {
    "https://registry.npmjs.org".into()
}

pub(crate) fn url_ok(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    let loopback = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    url.scheme() == "https" || (url.scheme() == "http" && loopback)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Env {
    #[serde(default = "port_range")]
    pub port_range: String,
    #[serde(default = "ten_u16")]
    pub ports_per_env: u16,
    #[serde(default = "fifteen_minutes")]
    pub setup_timeout_minutes: u32,
    #[serde(default)]
    pub limits: Limits,
}

impl Default for Env {
    fn default() -> Env {
        Env {
            port_range: port_range(),
            ports_per_env: ten_u16(),
            setup_timeout_minutes: fifteen_minutes(),
            limits: Limits::default(),
        }
    }
}

fn port_range() -> String {
    "41000-48999".into()
}
fn ten_u16() -> u16 {
    10
}
fn fifteen_minutes() -> u32 {
    15
}

impl Env {
    pub fn ports(&self) -> Result<(u16, u16), ConfigError> {
        let invalid = || {
            err(
                "config_invalid",
                format!(
                    "env.port_range {:?} must be FIRST-LAST within 1024..=65535",
                    self.port_range
                ),
            )
        };
        let (first, last) = self.port_range.split_once('-').ok_or_else(invalid)?;
        let first: u16 = first.trim().parse().map_err(|_| invalid())?;
        let last: u16 = last.trim().parse().map_err(|_| invalid())?;
        if first < 1024 || first >= last {
            return Err(invalid());
        }
        Ok((first, last))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    #[serde(default)]
    pub data_dir: Option<String>,
}

impl Default for Server {
    fn default() -> Server {
        Server {
            listen: default_listen(),
            allowed_origins: Vec::new(),
            data_dir: None,
        }
    }
}

fn default_listen() -> String {
    "127.0.0.1:7420".into()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scheduler {
    #[serde(default = "two")]
    pub max_active_tasks: usize,
}

impl Default for Scheduler {
    fn default() -> Scheduler {
        Scheduler {
            max_active_tasks: 2,
        }
    }
}

fn two() -> usize {
    2
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    #[serde(default = "five_minutes")]
    pub config_poll_secs: u32,
}

impl Default for Workspace {
    fn default() -> Workspace {
        Workspace {
            config_poll_secs: five_minutes(),
        }
    }
}

fn five_minutes() -> u32 {
    300
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScmEntry {
    pub id: String,
    pub provider: String,
    #[serde(default)]
    pub api_url: String,
    #[serde(default)]
    pub web_url: String,
    #[serde(default)]
    pub app_id: Option<u64>,
    #[serde(default)]
    pub private_key_file: Option<String>,
    #[serde(default)]
    pub token_file: Option<String>,
    #[serde(default = "fifteen")]
    pub poll_interval_secs: u64,
    #[serde(default = "six_hundred")]
    pub ci_appear_timeout_secs: u64,
    #[serde(default = "squash")]
    pub merge_method: MergeMethod,
    #[serde(default)]
    pub webhook_secret_file: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
}

impl ScmEntry {
    fn resolve_urls(&mut self) {
        let gitlab = self.provider == "gitlab";
        if self.web_url.is_empty() {
            self.web_url = if gitlab {
                "https://gitlab.com"
            } else {
                "https://github.com"
            }
            .into();
        }
        if self.api_url.is_empty() {
            self.api_url = if gitlab {
                format!("{}/api/v4", self.web_url.trim_end_matches('/'))
            } else {
                "https://api.github.com".into()
            };
        }
    }

    fn check_provider_fields(&self) -> Result<(), ConfigError> {
        let provider = self.provider.as_str();
        let fields = [
            ("app_id", self.app_id.is_some(), provider == "github"),
            (
                "private_key_file",
                self.private_key_file.is_some(),
                provider == "github",
            ),
            (
                "token_file",
                self.token_file.is_some(),
                provider == "gitlab",
            ),
        ];
        for (name, present, used) in fields {
            if used && !present {
                return Err(err(
                    "config_invalid",
                    format!("scm.{name} is required for provider {provider}"),
                ));
            }
            if present && !used {
                return Err(err(
                    "config_invalid",
                    format!("scm.{name} is not used by provider {provider}"),
                ));
            }
        }
        Ok(())
    }

    fn private_files(&self) -> impl Iterator<Item = &str> {
        [self.private_key_file.as_deref(), self.token_file.as_deref()]
            .into_iter()
            .flatten()
    }
}
fn fifteen() -> u64 {
    15
}
fn six_hundred() -> u64 {
    600
}
fn squash() -> MergeMethod {
    MergeMethod::Squash
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub memory_max: String,
    pub cpu_quota: String,
    pub tasks_max: u32,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            memory_max: "4G".into(),
            cpu_quota: "200%".into(),
            tasks_max: 512,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    #[serde(default = "claude")]
    pub cli: String,
    #[serde(default = "claude")]
    pub command: String,
    #[serde(default)]
    pub account_dir: Option<String>,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_path")]
    pub path: Vec<String>,
    #[serde(default = "one")]
    pub max_followup_turns: u32,
    #[serde(default = "twenty_four")]
    pub permission_wait_hours: i64,
    #[serde(default = "seventy_two")]
    pub decision_wait_hours: i64,
    #[serde(default = "thirty")]
    pub idle_minutes: i64,
    #[serde(default = "rate_limit_pause")]
    pub rate_limit_pause: f64,
    #[serde(default)]
    pub limits: Limits,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetConfig {
    #[serde(default = "eighty")]
    pub warn_percent: u32,
    #[serde(default)]
    pub dev: StageBudget,
    #[serde(default)]
    pub architect: ArchitectBudget,
    #[serde(default)]
    pub review: ReviewBudget,
    #[serde(default)]
    pub fix: FixBudget,
}

impl Default for BudgetConfig {
    fn default() -> BudgetConfig {
        BudgetConfig {
            warn_percent: eighty(),
            dev: StageBudget::default(),
            architect: ArchitectBudget::default(),
            review: ReviewBudget::default(),
            fix: FixBudget::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Architect {
    #[serde(default = "one_usize")]
    pub max_active: usize,
    #[serde(default = "four")]
    pub lint_rounds: u32,
    #[serde(default = "seventy_two")]
    pub session_idle_hours: i64,
    #[serde(default)]
    pub model: String,
}

impl Default for Architect {
    fn default() -> Architect {
        Architect {
            max_active: one_usize(),
            lint_rounds: four(),
            session_idle_hours: seventy_two(),
            model: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchitectBudget {
    #[serde(default = "thirty_minutes")]
    pub running_minutes: u32,
    #[serde(default = "six")]
    pub turns: u32,
    #[serde(default)]
    pub tokens: i64,
    #[serde(default)]
    pub cost_usd: f64,
}

impl Default for ArchitectBudget {
    fn default() -> ArchitectBudget {
        ArchitectBudget {
            running_minutes: thirty_minutes(),
            turns: six(),
            tokens: 0,
            cost_usd: 0.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewBudget {
    #[serde(default = "twenty_minutes")]
    pub running_minutes: u32,
    #[serde(default = "three")]
    pub turns: u32,
    #[serde(default)]
    pub tokens: i64,
    #[serde(default)]
    pub cost_usd: f64,
}

impl Default for ReviewBudget {
    fn default() -> ReviewBudget {
        ReviewBudget {
            running_minutes: twenty_minutes(),
            turns: three(),
            tokens: 0,
            cost_usd: 0.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixBudget {
    #[serde(default = "thirty_minutes")]
    pub running_minutes: u32,
    #[serde(default = "six")]
    pub turns: u32,
    #[serde(default)]
    pub tokens: i64,
    #[serde(default)]
    pub cost_usd: f64,
}

impl Default for FixBudget {
    fn default() -> FixBudget {
        FixBudget {
            running_minutes: thirty_minutes(),
            turns: six(),
            tokens: 0,
            cost_usd: 0.0,
        }
    }
}

fn twenty_minutes() -> u32 {
    20
}
fn three() -> u32 {
    3
}
fn one_usize() -> usize {
    1
}
fn four() -> u32 {
    4
}
fn six() -> u32 {
    6
}
fn thirty_minutes() -> u32 {
    30
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageBudget {
    #[serde(default = "sixty_minutes")]
    pub running_minutes: u32,
    #[serde(default = "ten")]
    pub turns: u32,
    #[serde(default)]
    pub tokens: i64,
    #[serde(default)]
    pub cost_usd: f64,
}

impl Default for StageBudget {
    fn default() -> StageBudget {
        StageBudget {
            running_minutes: sixty_minutes(),
            turns: ten(),
            tokens: 0,
            cost_usd: 0.0,
        }
    }
}

impl Default for Agent {
    fn default() -> Agent {
        Agent {
            cli: claude(),
            command: claude(),
            account_dir: None,
            model: String::new(),
            path: default_path(),
            max_followup_turns: one(),
            permission_wait_hours: twenty_four(),
            decision_wait_hours: seventy_two(),
            idle_minutes: thirty(),
            rate_limit_pause: rate_limit_pause(),
            limits: Limits::default(),
        }
    }
}

fn claude() -> String {
    "claude".into()
}
fn default_path() -> Vec<String> {
    [
        "~/.local/bin",
        "/usr/local/go/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
    ]
    .map(String::from)
    .to_vec()
}
fn sixty_minutes() -> u32 {
    60
}
fn ten() -> u32 {
    10
}
fn eighty() -> u32 {
    80
}
fn twenty_four() -> i64 {
    24
}
fn seventy_two() -> i64 {
    72
}
fn thirty() -> i64 {
    30
}
fn rate_limit_pause() -> f64 {
    0.9
}
fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Git {
    #[serde(default = "git_name")]
    pub name: String,
    #[serde(default = "git_email")]
    pub email: String,
}

impl Default for Git {
    fn default() -> Git {
        Git {
            name: git_name(),
            email: git_email(),
        }
    }
}

fn git_name() -> String {
    "Saka Agent".into()
}
fn git_email() -> String {
    "agent@saka.invalid".into()
}

// The identity ends up in a git author/committer header, where angle brackets and line breaks
// would end the field early.
fn git_name_ok(name: &str) -> bool {
    (1..=255).contains(&name.len()) && !name.contains(['<', '>', '\n', '\r'])
}

fn git_email_ok(email: &str) -> bool {
    (3..=254).contains(&email.len())
        && email.matches('@').count() == 1
        && !email.contains(['<', '>', '\n', '\r'])
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permission {
    #[serde(default)]
    pub rule: Vec<RuleSpec>,
}

pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SAKA_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".config")))?;
    Some(base.join("saka").join("config.toml"))
}

// The key that seals project secrets lives next to config.toml, outside the data dir and its backups.
pub fn secret_key_path() -> Option<PathBuf> {
    path().and_then(|p| p.parent().map(|d| d.join("secret.key")))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn expand(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), home()) {
        (Some(rest), Some(h)) => h.join(rest),
        _ if p == "~" => home().unwrap_or_else(|| PathBuf::from(p)),
        _ => PathBuf::from(p),
    }
}

fn id_ok(id: &str, max: usize) -> bool {
    (1..=max).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Config {
    pub fn load() -> Result<Config, ConfigError> {
        let explicit = std::env::var_os("SAKA_CONFIG").is_some();
        let Some(path) = path() else {
            return Config::parse("");
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let cfg = Config::parse(&text)?;
                cfg.check_hook_file(&path)?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => Config::parse(""),
            Err(e) => Err(err("config_unreadable", format!("{}: {e}", path.display()))),
        }
    }

    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let mut cfg: Config = toml::from_str(text).map_err(|e| {
            let message = e.message().to_string();
            if message.starts_with("unknown field") {
                err("config_unknown_field", message)
            } else {
                err("config_invalid", message)
            }
        })?;
        cfg.scm.iter_mut().for_each(ScmEntry::resolve_urls);
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn hooks_parsed(&self) -> Result<Vec<saka_core::hook::Hook>, ConfigError> {
        let mut hooks = saka_core::hook::parse_all(&self.hook).map_err(|issue| {
            let field = issue.field.map(|f| format!(".{f}")).unwrap_or_default();
            err(
                "config_hook_invalid",
                format!("{}: hook {:?}{field}", issue.code, issue.hook),
            )
        })?;
        for hook in &mut hooks {
            if let saka_core::hook::HookAction::RunHost { cwd: Some(cwd), .. } = &mut hook.action {
                *cwd = expand(cwd).to_string_lossy().into_owned();
            }
        }
        Ok(hooks)
    }

    pub fn environments_parsed(
        &self,
    ) -> Result<Vec<saka_core::environment::Environment>, ConfigError> {
        let mut environments =
            saka_core::environment::parse_all(&self.deploy).map_err(|issue| {
                let field = issue.field.map(|f| format!(".{f}")).unwrap_or_default();
                err(
                    "config_deploy_invalid",
                    format!("{}: environment {:?}{field}", issue.code, issue.environment),
                )
            })?;
        for environment in &mut environments {
            if let saka_core::environment::DeployAction::RunHost { cwd: Some(cwd), .. } =
                &mut environment.action
            {
                *cwd = expand(cwd).to_string_lossy().into_owned();
            }
        }
        Ok(environments)
    }

    pub fn incident_sources_parsed(&self) -> Result<Vec<saka_core::incident::Source>, ConfigError> {
        let mut sources =
            saka_core::incident::parse_all(&self.incident.source).map_err(|issue| {
                let field = issue.field.map(|f| format!(".{f}")).unwrap_or_default();
                err(
                    "config_incident_invalid",
                    format!("{}: source {:?}{field}", issue.code, issue.source_name),
                )
            })?;
        for source in &mut sources {
            source.secret_file = expand(&source.secret_file).to_string_lossy().into_owned();
        }
        Ok(sources)
    }

    pub fn check_incident_secrets(&self) -> Result<(), ConfigError> {
        for source in self.incident_sources_parsed()? {
            let invalid = || err("config_secret_file_invalid", source.name.clone());
            let path = std::path::Path::new(&source.secret_file);
            check_private_file(path).map_err(|_| invalid())?;
            let bytes = std::fs::read(path).map_err(|_| invalid())?;
            if bytes.iter().all(u8::is_ascii_whitespace) {
                return Err(invalid());
            }
        }
        Ok(())
    }

    fn check_hook_file(&self, path: &std::path::Path) -> Result<(), ConfigError> {
        use std::os::unix::fs::MetadataExt;
        let runs_host = self.hook.iter().any(|h| h.action == "run_host")
            || self.deploy.iter().any(|d| d.action == "run_host");
        if !runs_host {
            return Ok(());
        }
        let refused = || err("config_hook_file_writable", path.display().to_string());
        let meta = std::fs::metadata(path).map_err(|_| refused())?;
        let mine = meta.uid() == rustix::process::getuid().as_raw();
        if meta.mode() & 0o022 != 0 || !mine {
            return Err(refused());
        }
        Ok(())
    }

    fn validate_autonomy(&self) -> Result<(), ConfigError> {
        let mut seen = std::collections::BTreeSet::new();
        let valid =
            self.autonomy.repo.len() <= MAX_AUTONOMY_REPOS
                && self.autonomy.repo.iter().all(|r| {
                    repo_full_name_ok(&r.repo) && seen.insert(r.repo.to_ascii_lowercase())
                });
        if valid {
            Ok(())
        } else {
            Err(err(
                "config_invalid",
                "autonomy.repo must list each owner/name once, at most 256",
            ))
        }
    }

    pub fn autonomy_ceilings(&self) -> Ceilings {
        Ceilings {
            default: self.autonomy.default,
            repos: self
                .autonomy
                .repo
                .iter()
                .map(|r| (r.repo.clone(), r.level))
                .collect(),
        }
    }

    fn validate_auth(&self) -> Result<(), ConfigError> {
        let Some(auth) = &self.auth else {
            return Ok(());
        };
        let invalid = |detail: &str| err("config_auth_invalid", detail.to_string());
        let origin = saka_core::identity::parse_origin(&auth.origin)
            .ok_or_else(|| invalid("auth.origin must be https://<domain>[:port]"))?;
        if origin.scheme != saka_core::identity::Scheme::Https {
            let loopback = self.listen().is_ok_and(|a| is_loopback(&a));
            if !(origin.is_dev_localhost() && loopback) {
                return Err(invalid(
                    "auth.origin must be https; http://localhost is only for a loopback listen",
                ));
            }
        }
        if !saka_core::identity::rp_id_valid(&origin, &auth.rp_id) {
            return Err(invalid(
                "auth.rp_id must be the domain of auth.origin or a parent domain of it",
            ));
        }
        let hours = auth
            .session_hours
            .unwrap_or(saka_core::identity::SESSION_HOURS_DEFAULT);
        if !(1..=saka_core::identity::SESSION_HOURS_MAX).contains(&hours) {
            return Err(invalid("auth.session_hours must be 1..=720"));
        }
        if auth
            .rp_name
            .as_deref()
            .is_some_and(|n| !saka_core::identity::name_valid(n))
        {
            return Err(invalid("auth.rp_name must be 1 to 100 characters"));
        }
        Ok(())
    }

    pub fn auth_settings(&self) -> Option<saka_api::AuthSettings> {
        let auth = self.auth.as_ref()?;
        Some(saka_api::AuthSettings {
            origin: saka_core::identity::parse_origin(&auth.origin)?,
            rp_id: auth.rp_id.clone(),
            rp_name: auth
                .rp_name
                .clone()
                .unwrap_or_else(|| RP_NAME_DEFAULT.to_string()),
            session_hours: auth
                .session_hours
                .unwrap_or(saka_core::identity::SESSION_HOURS_DEFAULT),
        })
    }

    fn validate(&self) -> Result<(), ConfigError> {
        self.listen()?;
        self.validate_auth()?;
        if let Some(hooks) = &self.hooks {
            let at = self.hooks_listen()?;
            if Some(at) == self.listen().ok() || hooks.listen.trim() == self.server.listen.trim() {
                return Err(err(
                    "config_invalid",
                    "hooks.listen must differ from server.listen",
                ));
            }
            if self.scm.iter().all(|s| s.webhook_secret_file.is_none()) {
                return Err(err(
                    "config_invalid",
                    "hooks needs at least one scm.webhook_secret_file",
                ));
            }
        }
        if !(1..=16).contains(&self.scheduler.max_active_tasks) {
            return Err(err(
                "config_invalid",
                "scheduler.max_active_tasks must be 1..=16",
            ));
        }
        let poll = self.workspace.config_poll_secs;
        if poll != 0 && !(30..=86_400).contains(&poll) {
            return Err(err(
                "config_invalid",
                "workspace.config_poll_secs must be 0 or 30..=86400",
            ));
        }
        let hooks = self.hooks_parsed()?;
        self.environments_parsed()?;
        self.incident_sources_parsed()?;
        self.notify.validate()?;
        for hook in &hooks {
            if let saka_core::hook::HookAction::Notify { channel } = &hook.action
                && !self.notify.names().any(|n| n == channel)
            {
                return Err(err(
                    "config_notify_invalid",
                    format!("notify_hook_channel_unknown: hook {:?}", hook.name),
                ));
            }
        }
        let mut ids = std::collections::BTreeSet::new();
        for s in &self.scm {
            if !id_ok(&s.id, 32) || !ids.insert(s.id.clone()) {
                return Err(err(
                    "config_invalid",
                    format!("scm.id {:?} is invalid or repeated", s.id),
                ));
            }
            if !matches!(s.provider.as_str(), "github" | "gitlab") {
                return Err(err(
                    "config_invalid",
                    format!("scm.provider {:?} is not supported", s.provider),
                ));
            }
            s.check_provider_fields()?;
            if !(5..=300).contains(&s.poll_interval_secs) {
                return Err(err(
                    "config_invalid",
                    "scm.poll_interval_secs must be 5..=300",
                ));
            }
            if !(60..=3600).contains(&s.ci_appear_timeout_secs) {
                return Err(err(
                    "config_invalid",
                    "scm.ci_appear_timeout_secs must be 60..=3600",
                ));
            }
            if !url_ok(&s.web_url) || !url_ok(&s.api_url) {
                return Err(err(
                    "config_invalid",
                    "scm.web_url and scm.api_url must be https, or http on a loopback host",
                ));
            }
            if s.client_id
                .as_deref()
                .is_some_and(|id| !client_id_valid(id))
            {
                return Err(err(
                    "config_invalid",
                    "scm.client_id must be 1 to 64 characters of A-Z, a-z, 0-9, '.', '_' or '-'",
                ));
            }
        }
        if !git_name_ok(&self.git.name) {
            return Err(err(
                "config_invalid",
                "git.name must be 1 to 255 bytes without '<', '>' or a line break",
            ));
        }
        if !git_email_ok(&self.git.email) {
            return Err(err(
                "config_invalid",
                "git.email must be 3 to 254 bytes with exactly one '@'",
            ));
        }
        self.validate_autonomy()?;
        let mut rule_ids = std::collections::BTreeSet::new();
        for r in &self.permission.rule {
            if !id_ok(&r.id, 64) || !rule_ids.insert(r.id.clone()) {
                return Err(err(
                    "config_invalid",
                    format!("permission.rule.id {:?} is invalid or repeated", r.id),
                ));
            }
        }
        if self.agent.cli != "claude" {
            return Err(err(
                "config_invalid",
                "agent.cli must be claude in this version",
            ));
        }
        let a = &self.agent;
        let b = &self.budget;
        let checks: [(bool, &str); 9] = [
            (
                (1..=168).contains(&a.permission_wait_hours),
                "agent.permission_wait_hours must be 1..=168",
            ),
            (
                (1..=168).contains(&a.decision_wait_hours),
                "agent.decision_wait_hours must be 1..=168",
            ),
            (
                (1..=1440).contains(&a.idle_minutes),
                "agent.idle_minutes must be 1..=1440",
            ),
            (
                (0.5..=1.0).contains(&a.rate_limit_pause),
                "agent.rate_limit_pause must be 0.5..=1.0",
            ),
            (
                (50..=99).contains(&b.warn_percent),
                "budget.warn_percent must be 50..=99",
            ),
            (
                (1..=1440).contains(&b.dev.running_minutes),
                "budget.dev.running_minutes must be 1..=1440",
            ),
            (
                (1..=100).contains(&b.dev.turns),
                "budget.dev.turns must be 1..=100",
            ),
            (b.dev.tokens >= 0, "budget.dev.tokens must be 0 or more"),
            (
                b.dev.cost_usd >= 0.0 && b.dev.cost_usd.is_finite(),
                "budget.dev.cost_usd must be 0 or more",
            ),
        ];
        if let Some((_, message)) = checks.iter().find(|(ok, _)| !ok) {
            return Err(err("config_invalid", *message));
        }
        let r = &self.architect;
        let ab = &b.architect;
        let architect: [(bool, &str); 8] = [
            (
                (1..=4).contains(&r.max_active),
                "architect.max_active must be 1..=4",
            ),
            (
                (1..=10).contains(&r.lint_rounds),
                "architect.lint_rounds must be 1..=10",
            ),
            (
                (1..=168).contains(&r.session_idle_hours),
                "architect.session_idle_hours must be 1..=168",
            ),
            (
                (1..=1440).contains(&ab.running_minutes),
                "budget.architect.running_minutes must be 1..=1440",
            ),
            (
                (1..=100).contains(&ab.turns),
                "budget.architect.turns must be 1..=100",
            ),
            (
                ab.turns >= r.lint_rounds,
                "budget.architect.turns must be at least architect.lint_rounds",
            ),
            (ab.tokens >= 0, "budget.architect.tokens must be 0 or more"),
            (
                ab.cost_usd >= 0.0 && ab.cost_usd.is_finite(),
                "budget.architect.cost_usd must be 0 or more",
            ),
        ];
        if let Some((_, message)) = architect.iter().find(|(ok, _)| !ok) {
            return Err(err("config_invalid", *message));
        }
        let rb = &b.review;
        let review: [(bool, &str); 4] = [
            (
                (1..=1440).contains(&rb.running_minutes),
                "budget.review.running_minutes must be 1..=1440",
            ),
            (
                (1..=100).contains(&rb.turns),
                "budget.review.turns must be 1..=100",
            ),
            (rb.tokens >= 0, "budget.review.tokens must be 0 or more"),
            (
                rb.cost_usd >= 0.0 && rb.cost_usd.is_finite(),
                "budget.review.cost_usd must be 0 or more",
            ),
        ];
        if let Some((_, message)) = review.iter().find(|(ok, _)| !ok) {
            return Err(err("config_invalid", *message));
        }
        let fb = &b.fix;
        let fix: [(bool, &str); 4] = [
            (
                (1..=1440).contains(&fb.running_minutes),
                "budget.fix.running_minutes must be 1..=1440",
            ),
            (
                (1..=100).contains(&fb.turns),
                "budget.fix.turns must be 1..=100",
            ),
            (fb.tokens >= 0, "budget.fix.tokens must be 0 or more"),
            (
                fb.cost_usd >= 0.0 && fb.cost_usd.is_finite(),
                "budget.fix.cost_usd must be 0 or more",
            ),
        ];
        if let Some((_, message)) = fix.iter().find(|(ok, _)| !ok) {
            return Err(err("config_invalid", *message));
        }
        let e = &self.env;
        if !(1..=100).contains(&e.ports_per_env) {
            return Err(err("config_invalid", "env.ports_per_env must be 1..=100"));
        }
        if !(1..=240).contains(&e.setup_timeout_minutes) {
            return Err(err(
                "config_invalid",
                "env.setup_timeout_minutes must be 1..=240",
            ));
        }
        let t = &self.toolchain;
        for (name, value) in [
            ("toolchain.go_dl_url", &t.go_dl_url),
            ("toolchain.node_dist_url", &t.node_dist_url),
            ("toolchain.npm_registry_url", &t.npm_registry_url),
        ] {
            if !url_ok(value) {
                return Err(err(
                    "config_invalid_url",
                    format!("{name} {value:?} must be https, or http on a loopback host"),
                ));
            }
        }
        if !(30..=3600).contains(&t.download_timeout_secs) {
            return Err(err(
                "config_invalid",
                "toolchain.download_timeout_secs must be 30..=3600",
            ));
        }
        let (first, last) = e.ports()?;
        let span = usize::from(last - first) + 1;
        let needed = usize::from(e.ports_per_env) * self.scheduler.max_active_tasks;
        if span < needed {
            return Err(err(
                "config_port_range_too_small",
                format!(
                    "env.port_range holds {span} ports; env.ports_per_env x scheduler.max_active_tasks needs {needed}"
                ),
            ));
        }
        Ok(())
    }

    pub fn listen(&self) -> Result<SocketAddr, ConfigError> {
        self.server.listen.parse().map_err(|_| {
            err(
                "config_invalid",
                format!("server.listen {:?}", self.server.listen),
            )
        })
    }

    pub fn data_dir(&self) -> PathBuf {
        if let Some(d) = &self.server.data_dir {
            return expand(d);
        }
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".local").join("share")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("saka")
    }

    pub fn account_name(&self) -> String {
        self.account_dir().file_name().map_or_else(
            || "default".to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    pub fn account_dir(&self) -> PathBuf {
        match &self.agent.account_dir {
            Some(d) => expand(d),
            None => self
                .data_dir()
                .join("agents")
                .join("claude")
                .join("default"),
        }
    }

    pub fn hooks_listen(&self) -> Result<SocketAddr, ConfigError> {
        let listen = self.hooks.as_ref().map_or("", |h| h.listen.as_str());
        listen
            .parse()
            .map_err(|_| err("config_invalid", format!("hooks.listen {listen:?}")))
    }

    pub fn webhook_secrets(&self) -> Result<Vec<(String, Vec<u8>)>, ConfigError> {
        let mut out = Vec::new();
        for s in &self.scm {
            let Some(file) = &s.webhook_secret_file else {
                continue;
            };
            let path = expand(file);
            check_private_file(&path)?;
            let mut bytes = std::fs::read(&path).map_err(|e| {
                err(
                    "config_key_file_unreadable",
                    format!("{}: {e}", path.display()),
                )
            })?;
            while bytes.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                bytes.pop();
            }
            let fits = (WEBHOOK_SECRET_MIN..=WEBHOOK_SECRET_MAX).contains(&bytes.len());
            if !fits || bytes.contains(&0) {
                return Err(err(
                    "config_webhook_secret_invalid",
                    path.display().to_string(),
                ));
            }
            out.push((s.id.clone(), bytes));
        }
        Ok(out)
    }

    pub fn check_key_files(&self) -> Result<(), ConfigError> {
        for file in self.scm.iter().flat_map(ScmEntry::private_files) {
            check_private_file(&expand(file))?;
        }
        for file in self.notify_token_files() {
            check_private_file(&expand(file))?;
        }
        Ok(())
    }
}

pub(crate) fn check_private_file(path: &std::path::Path) -> Result<(), ConfigError> {
    let meta = std::fs::metadata(path).map_err(|e| {
        err(
            "config_key_file_unreadable",
            format!("{}: {e}", path.display()),
        )
    })?;
    let mine = std::os::unix::fs::MetadataExt::uid(&meta) == rustix::process::getuid().as_raw();
    if !meta.is_file() || meta.permissions().mode() & 0o777 != 0o600 || !mine {
        return Err(err("config_key_file_mode", path.display().to_string()));
    }
    Ok(())
}

pub fn is_loopback(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

pub fn paths_of(path: &[String]) -> Vec<String> {
    path.iter()
        .map(|p| expand(p).to_string_lossy().into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreviewed_chores_are_off_unless_the_operator_allows_them() {
        assert!(!Config::parse("").unwrap().workflow.allow_unreviewed_chore);
        let allowed = Config::parse("[workflow]\nallow_unreviewed_chore = true\n").unwrap();
        assert!(allowed.workflow.allow_unreviewed_chore);
        assert!(Config::parse("[workflow]\nallow_unreviewed = true\n").is_err());
    }

    #[test]
    fn architect_defaults_and_the_field_that_is_out_of_range() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(
            (cfg.architect.max_active, cfg.architect.lint_rounds),
            (1, 4)
        );
        assert_eq!(cfg.architect.session_idle_hours, 72);
        assert!(cfg.architect.model.is_empty());
        let b = &cfg.budget.architect;
        assert_eq!((b.running_minutes, b.turns, b.tokens), (30, 6, 0));
        assert!(b.cost_usd.abs() < f64::EPSILON);
        let partial = Config::parse("[budget.architect]\ntokens = 5\n").unwrap();
        assert_eq!(partial.budget.architect.running_minutes, 30);
        for (bad, field) in [
            ("[architect]\nlint_rounds = 0\n", "architect.lint_rounds"),
            ("[architect]\nlint_rounds = 8\n", "budget.architect.turns"),
            ("[architect]\nmax_active = 9\n", "architect.max_active"),
            (
                "[architect]\nsession_idle_hours = 0\n",
                "architect.session_idle_hours",
            ),
            (
                "[architect]\nsession_idle_hours = 169\n",
                "architect.session_idle_hours",
            ),
        ] {
            let e = Config::parse(bad).unwrap_err();
            assert_eq!(e.code, "config_invalid");
            assert!(e.detail.starts_with(field), "{bad}: {}", e.detail);
        }
    }

    #[test]
    fn the_review_budget_has_defaults_and_ranges() {
        let cfg = Config::parse("").unwrap();
        let b = &cfg.budget.review;
        assert_eq!((b.running_minutes, b.turns, b.tokens), (20, 3, 0));
        let partial = Config::parse("[budget.review]\ntokens = 5\n").unwrap();
        assert_eq!(partial.budget.review.running_minutes, 20);
        for bad in [
            "[budget.review]\nrunning_minutes = 0\n",
            "[budget.review]\nturns = 101\n",
            "[budget.review]\ntokens = -1\n",
            "[budget.review]\ncost_usd = -1.0\n",
        ] {
            let e = Config::parse(bad).unwrap_err();
            assert_eq!(e.code, "config_invalid");
            assert!(e.detail.starts_with("budget.review"), "{bad}: {}", e.detail);
        }
    }

    #[test]
    fn the_fix_budget_has_defaults_and_ranges() {
        let cfg = Config::parse("").unwrap();
        let b = &cfg.budget.fix;
        assert_eq!((b.running_minutes, b.turns, b.tokens), (30, 6, 0));
        let partial = Config::parse("[budget.fix]\ntokens = 5\n").unwrap();
        assert_eq!(partial.budget.fix.running_minutes, 30);
        for bad in [
            "[budget.fix]\nrunning_minutes = 0\n",
            "[budget.fix]\nturns = 101\n",
            "[budget.fix]\ntokens = -1\n",
            "[budget.fix]\ncost_usd = -1.0\n",
        ] {
            let e = Config::parse(bad).unwrap_err();
            assert_eq!(e.code, "config_invalid");
            assert!(e.detail.starts_with("budget.fix"), "{bad}: {}", e.detail);
        }
    }

    #[test]
    fn defaults_unknown_fields_and_ranges() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.server.listen, "127.0.0.1:7420");
        assert_eq!(cfg.scheduler.max_active_tasks, 2);
        assert_eq!(cfg.agent.limits.tasks_max, 512);
        assert_eq!(
            Config::parse("[server]\nport = 1\n").unwrap_err().code,
            "config_unknown_field"
        );
        assert_eq!(
            Config::parse("[scheduler]\nmax_active_tasks = 17\n")
                .unwrap_err()
                .code,
            "config_invalid"
        );
        let scm = "[[scm]]\nid = \"github\"\nprovider = \"github\"\napp_id = 1\nprivate_key_file = \"/k\"\n";
        let cfg = Config::parse(scm).unwrap();
        assert_eq!(cfg.scm[0].merge_method, MergeMethod::Squash);
        assert_eq!(cfg.scm[0].web_url, "https://github.com");
        let twice = format!("{scm}{scm}");
        assert_eq!(Config::parse(&twice).unwrap_err().code, "config_invalid");
        let rules = "[[permission.rule]]\nid = \"read\"\neffect = \"allow\"\ntools = [\"Read\"]\n";
        assert_eq!(Config::parse(rules).unwrap().permission.rule.len(), 1);
        assert_eq!(cfg.agent.permission_wait_hours, 24);
        assert_eq!(cfg.agent.decision_wait_hours, 72);
        assert_eq!(cfg.agent.idle_minutes, 30);
        assert_eq!(cfg.budget.warn_percent, 80);
        assert_eq!(
            (cfg.budget.dev.running_minutes, cfg.budget.dev.turns),
            (60, 10)
        );
        for bad in [
            "[agent]\npermission_wait_hours = 0\n",
            "[agent]\ndecision_wait_hours = 0\n",
            "[agent]\ndecision_wait_hours = 169\n",
            "[agent]\nidle_minutes = 1441\n",
            "[agent]\nrate_limit_pause = 0.4\n",
            "[budget]\nwarn_percent = 100\n",
            "[budget.dev]\nturns = 0\n",
            "[budget.dev]\ncost_usd = -1.0\n",
            "[architect]\nmax_active = 0\n",
            "[architect]\nmax_active = 5\n",
            "[architect]\nlint_rounds = 11\n",
            "[architect]\nsession_idle_hours = 169\n",
            "[budget.architect]\nrunning_minutes = 0\n",
            "[budget.architect]\nturns = 101\n",
            "[budget.architect]\nturns = 3\n",
            "[budget.architect]\ntokens = -1\n",
            "[budget.architect]\ncost_usd = -0.5\n",
        ] {
            assert_eq!(
                Config::parse(bad).unwrap_err().code,
                "config_invalid",
                "{bad}"
            );
        }
        assert_eq!(
            Config::parse("[agent]\nmax_running_minutes = 60\n")
                .unwrap_err()
                .code,
            "config_unknown_field"
        );
    }

    #[test]
    fn the_config_poll_is_off_or_between_thirty_seconds_and_a_day() {
        assert_eq!(Config::parse("").unwrap().workspace.config_poll_secs, 300);
        for ok in [0, 30, 86_400] {
            let text = format!("[workspace]\nconfig_poll_secs = {ok}\n");
            assert_eq!(Config::parse(&text).unwrap().workspace.config_poll_secs, ok);
        }
        for bad in [1, 10, 29, 86_401] {
            let e = Config::parse(&format!("[workspace]\nconfig_poll_secs = {bad}\n")).unwrap_err();
            assert_eq!(e.code, "config_invalid");
            assert!(e.detail.starts_with("workspace.config_poll_secs"), "{bad}");
        }
    }

    #[test]
    fn the_port_range_must_fit_every_active_task() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.env.ports().unwrap(), (41000, 48999));
        assert_eq!(
            (cfg.env.ports_per_env, cfg.env.setup_timeout_minutes),
            (10, 15)
        );
        assert_eq!(
            Config::parse(
                "[env]\nport_range = \"41000-41009\"\n[scheduler]\nmax_active_tasks = 2\n"
            )
            .unwrap_err()
            .code,
            "config_port_range_too_small"
        );
        assert!(Config::parse("[env]\nport_range = \"41000-41019\"\n").is_ok());
        for bad in [
            "[env]\nport_range = \"80-9000\"\n",
            "[env]\nport_range = \"42000-41000\"\n",
            "[env]\nport_range = \"41000\"\n",
            "[env]\nport_range = \"41000-70000\"\n",
            "[env]\nports_per_env = 0\n",
            "[env]\nports_per_env = 101\n",
            "[env]\nsetup_timeout_minutes = 0\n",
        ] {
            assert_eq!(
                Config::parse(bad).unwrap_err().code,
                "config_invalid",
                "{bad}"
            );
        }
    }

    #[test]
    fn the_auth_section_is_checked_with_one_code() {
        let auth = |extra: &str| {
            format!(
                "[auth]\norigin = \"https://saka.example.com\"\nrp_id = \"example.com\"\n{extra}"
            )
        };
        assert!(Config::parse("").unwrap().auth.is_none());
        let cfg = Config::parse(&auth("")).unwrap();
        let settings = cfg.auth_settings().unwrap();
        assert_eq!(
            (settings.rp_name.as_str(), settings.session_hours),
            ("Saka", 168)
        );
        assert!(Config::parse(&auth("session_hours = 1\n")).is_ok());
        assert!(Config::parse(&auth("session_hours = 720\n")).is_ok());
        assert!(Config::parse("[server]\nlisten = \"0.0.0.0:7420\"\n[auth]\norigin = \"https://saka.example.com\"\nrp_id = \"saka.example.com\"\n").is_ok());
        for bad in [
            "[auth]\norigin = \"http://saka.example.com\"\nrp_id = \"example.com\"\n".to_string(),
            "[auth]\norigin = \"https://saka.example.com/app\"\nrp_id = \"example.com\"\n".to_string(),
            "[auth]\norigin = \"https://127.0.0.1\"\nrp_id = \"127.0.0.1\"\n".to_string(),
            "[auth]\norigin = \"https://saka.example.com\"\nrp_id = \"other.org\"\n".to_string(),
            "[auth]\norigin = \"https://saka.example.com\"\nrp_id = \"com\"\n".to_string(),
            auth("session_hours = 0\n"),
            auth("session_hours = 721\n"),
            auth("rp_name = \"\"\n"),
            "[server]\nlisten = \"0.0.0.0:7420\"\n[auth]\norigin = \"http://localhost:5173\"\nrp_id = \"localhost\"\n".to_string(),
        ] {
            assert_eq!(Config::parse(&bad).unwrap_err().code, "config_auth_invalid", "{bad}");
        }
        let dev = "[auth]\norigin = \"http://localhost:5173\"\nrp_id = \"localhost\"\n";
        assert!(Config::parse(dev).is_ok());
        let e = Config::parse(&auth("nope = 1\n")).unwrap_err();
        assert_eq!(e.code, "config_unknown_field");
        let missing = Config::parse("[auth]\norigin = \"https://saka.example.com\"\n").unwrap_err();
        assert_eq!(missing.code, "config_invalid");
    }

    #[test]
    fn toolchain_mirrors_must_be_https_or_loopback() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.toolchain.go_dl_url, "https://go.dev/dl");
        assert_eq!(cfg.toolchain.download_timeout_secs, 600);
        assert!(
            Config::parse("[toolchain]\nnode_dist_url = \"http://127.0.0.1:8080/dist\"\n").is_ok()
        );
        assert!(
            Config::parse("[toolchain]\nnpm_registry_url = \"http://localhost:4873\"\n").is_ok()
        );
        for bad in [
            "[toolchain]\ngo_dl_url = \"http://mirror.example/dl\"\n",
            "[toolchain]\nnode_dist_url = \"ftp://127.0.0.1/dist\"\n",
            "[toolchain]\nnpm_registry_url = \"not a url\"\n",
        ] {
            assert_eq!(
                Config::parse(bad).unwrap_err().code,
                "config_invalid_url",
                "{bad}"
            );
        }
        assert_eq!(
            Config::parse("[toolchain]\ndownload_timeout_secs = 10\n")
                .unwrap_err()
                .code,
            "config_invalid"
        );
    }

    #[test]
    fn scm_urls_must_be_https_or_loopback_http() {
        let scm = |url: &str| {
            format!(
                "[[scm]]\nid = \"github\"\nprovider = \"github\"\napp_id = 1\nprivate_key_file = \"/k\"\nweb_url = {url:?}\napi_url = {url:?}\n"
            )
        };
        for good in [
            "https://ghe.example.com",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "http://localhost:1",
        ] {
            let cfg = Config::parse(&scm(good)).unwrap();
            assert_eq!(cfg.scm[0].web_url, good);
            assert_eq!(cfg.scm[0].api_url, good);
        }
        for bad in ["http://github.com", "ftp://x"] {
            assert_eq!(
                Config::parse(&scm(bad)).unwrap_err().code,
                "config_invalid",
                "{bad}"
            );
        }
    }

    #[test]
    fn a_gitlab_entry_needs_only_a_token_file_and_derives_its_urls() {
        let gitlab = |extra: &str| {
            Config::parse(&format!(
                "[[scm]]\nid = \"gitlab\"\nprovider = \"gitlab\"\ntoken_file = \"/t\"\n{extra}"
            ))
        };
        let cfg = gitlab("").unwrap();
        let s = &cfg.scm[0];
        assert_eq!(s.web_url, "https://gitlab.com");
        assert_eq!(s.api_url, "https://gitlab.com/api/v4");
        assert_eq!(s.token_file.as_deref(), Some("/t"));
        assert_eq!((s.app_id, s.private_key_file.as_deref()), (None, None));
        assert_eq!(s.merge_method, MergeMethod::Squash);

        let own = gitlab("web_url = \"https://git.example.com/gitlab/\"\n").unwrap();
        assert_eq!(own.scm[0].api_url, "https://git.example.com/gitlab/api/v4");
        let set = gitlab(
            "web_url = \"https://git.example.com\"\napi_url = \"https://api.example.com/v4\"\n",
        )
        .unwrap();
        assert_eq!(set.scm[0].api_url, "https://api.example.com/v4");
        assert_eq!(
            gitlab("web_url = \"http://gitlab.example\"\n")
                .unwrap_err()
                .code,
            "config_invalid"
        );
        assert!(gitlab("web_url = \"http://127.0.0.1:8929\"\n").is_ok());
    }

    #[test]
    fn each_provider_requires_its_own_credential_fields_and_refuses_the_others() {
        let parse = |body: &str| Config::parse(&format!("[[scm]]\nid = \"x\"\n{body}"));
        let cases = [
            ("provider = \"gitlab\"\n", "scm.token_file is required"),
            (
                "provider = \"gitlab\"\ntoken_file = \"/t\"\napp_id = 1\n",
                "scm.app_id is not used",
            ),
            (
                "provider = \"gitlab\"\ntoken_file = \"/t\"\nprivate_key_file = \"/k\"\n",
                "scm.private_key_file is not used",
            ),
            (
                "provider = \"github\"\nprivate_key_file = \"/k\"\n",
                "scm.app_id is required",
            ),
            (
                "provider = \"github\"\napp_id = 1\n",
                "scm.private_key_file is required",
            ),
            (
                "provider = \"github\"\napp_id = 1\nprivate_key_file = \"/k\"\ntoken_file = \"/t\"\n",
                "scm.token_file is not used",
            ),
            ("provider = \"bitbucket\"\n", "is not supported"),
        ];
        for (body, expected) in cases {
            let e = parse(body).unwrap_err();
            assert_eq!(e.code, "config_invalid", "{body}");
            assert!(e.detail.contains(expected), "{body}: {}", e.detail);
        }
        let both = "[[scm]]\nid = \"a\"\nprovider = \"github\"\napp_id = 1\nprivate_key_file = \"/k\"\n\n[[scm]]\nid = \"b\"\nprovider = \"gitlab\"\ntoken_file = \"/t\"\n";
        let cfg = Config::parse(both).unwrap();
        assert_eq!(cfg.scm[0].api_url, "https://api.github.com");
        assert_eq!(cfg.scm[1].api_url, "https://gitlab.com/api/v4");
    }

    #[test]
    fn the_token_file_is_checked_like_a_private_key() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let token = dir.path().join("token");
        let cfg = Config::parse(&format!(
            "[[scm]]\nid = \"gitlab\"\nprovider = \"gitlab\"\ntoken_file = {:?}\n",
            token.display().to_string()
        ))
        .unwrap();
        assert_eq!(
            cfg.check_key_files().unwrap_err().code,
            "config_key_file_unreadable"
        );
        std::fs::write(&token, "glpat-x").unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            cfg.check_key_files().unwrap_err().code,
            "config_key_file_mode"
        );
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        cfg.check_key_files().unwrap();
    }

    fn incident_config(extra: &str) -> String {
        format!(
            "[[incident.source]]\nname = \"prod\"\nproject = \"tasks\"\nkind = \"generic\"\nsecret_file = \"/etc/saka/prod.secret\"\n{extra}"
        )
    }

    #[test]
    fn incident_sources_are_checked_with_a_coded_error_that_names_the_source_and_field() {
        assert_eq!(
            Config::parse(&incident_config(""))
                .unwrap()
                .incident
                .source
                .len(),
            1
        );
        for (extra, detail) in [
            (
                "max_open = 0\n",
                "incident_source_limit_invalid: source \"prod\".max_open",
            ),
            (
                "action = \"page\"\n",
                "incident_source_field_invalid: source \"prod\".action",
            ),
            (
                "environment_label = \"env\"\n",
                "incident_source_field_invalid: source \"prod\".environment_label",
            ),
            ("unknown_key = 1\n", ""),
        ] {
            let error = Config::parse(&incident_config(extra)).unwrap_err();
            if detail.is_empty() {
                assert_eq!(error.code, "config_unknown_field");
            } else {
                assert_eq!(
                    (error.code, error.detail.as_str()),
                    ("config_incident_invalid", detail)
                );
            }
        }
        let twice = format!("{}{}", incident_config(""), incident_config(""));
        assert!(
            Config::parse(&twice)
                .unwrap_err()
                .detail
                .starts_with("incident_source_duplicate")
        );
    }

    #[test]
    fn an_incident_secret_file_must_exist_hold_a_secret_and_be_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("prod.secret");
        let cfg = Config::parse(&format!(
            "[[incident.source]]\nname = \"prod\"\nproject = \"tasks\"\nkind = \"generic\"\nsecret_file = {:?}\n",
            secret.display().to_string()
        ))
        .unwrap();
        let refused = |cfg: &Config| {
            let error = cfg.check_incident_secrets().unwrap_err();
            (error.code, error.detail)
        };
        let expected = ("config_secret_file_invalid", "prod".to_string());
        assert_eq!(refused(&cfg), expected, "missing");
        std::fs::write(&secret, "s3cr3t-value\n").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(refused(&cfg), expected, "readable by others");
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
        cfg.check_incident_secrets().unwrap();
        std::fs::write(&secret, "\n").unwrap();
        assert_eq!(refused(&cfg), expected, "empty");
        assert!(
            Config::parse("").unwrap().check_incident_secrets().is_ok(),
            "no sources, nothing to check"
        );
    }

    #[test]
    fn the_example_config_parses_with_every_field_set() {
        let cfg = Config::parse(include_str!("../../../docs/config.example.toml")).unwrap();
        assert_eq!(cfg.scm.len(), 2);
        assert_eq!(cfg.architect.session_idle_hours, 72);
        assert_eq!(cfg.scm[0].app_id, Some(123456));
        let gitlab = &cfg.scm[1];
        assert_eq!(gitlab.provider, "gitlab");
        assert_eq!(
            gitlab.token_file.as_deref(),
            Some("~/.config/saka/scm/gitlab/token")
        );
        assert_eq!(gitlab.api_url, "https://git.example.com/api/v4");
        assert_eq!(gitlab.merge_method, MergeMethod::Merge);
        assert_eq!(cfg.agent.limits.cpu_quota, "200%");
        assert_eq!(cfg.architect.session_idle_hours, 72);
        assert_eq!(cfg.agent.decision_wait_hours, 72);
        assert_eq!(cfg.budget.review.running_minutes, 20);
        assert_eq!(cfg.budget.fix.running_minutes, 30);
        assert_eq!(cfg.budget.fix.turns, 6);
        assert_eq!(cfg.permission.rule.len(), 3);
        assert_eq!(cfg.env.ports_per_env, 10);
        assert_eq!(cfg.env.limits.tasks_max, 512);
        assert_eq!(cfg.toolchain.npm_registry_url, "https://registry.npmjs.org");
        assert_eq!(cfg.workspace.config_poll_secs, 300);
        assert!(!cfg.workflow.allow_unreviewed_chore);
        let auth = cfg.auth_settings().unwrap();
        assert_eq!(auth.origin.serialized(), "https://saka.example.com");
        assert_eq!(
            (
                auth.rp_id.as_str(),
                auth.rp_name.as_str(),
                auth.session_hours
            ),
            ("example.com", "Saka", 168)
        );
        assert_eq!(cfg.autonomy.default, Autonomy::Standard);
        assert_eq!(
            cfg.autonomy_ceilings().allowed("ACME/api"),
            Autonomy::Autonomous
        );
        assert_eq!(cfg.notify.channel[0].name, "ops");
        assert!(cfg.notify.channel[0].actions);
        assert_eq!(cfg.notify.channel[0].allowed_users, ["123456789"]);
        assert_eq!(
            cfg.notify.public_url.as_deref(),
            Some("https://saka.example.com")
        );
        assert_eq!(cfg.notify.language, "en");
        let hooks = cfg.hooks_parsed().unwrap();
        assert_eq!(hooks.len(), 5);
        assert_eq!(hooks[0].name, "announce-merge");
        assert_eq!(hooks[1].filter.codes.len(), 2);
        assert_eq!(hooks[2].action.kind().as_str(), "run");
        assert_eq!(hooks[3].action.kind().as_str(), "run_host");
        assert_eq!(hooks[4].mode.as_str(), "gate");
        let environments = cfg.environments_parsed().unwrap();
        assert_eq!(environments.len(), 2);
        assert_eq!(environments[0].name, "staging");
        assert_eq!(environments[0].timeout_ms, 900_000);
        assert!(environments[0].gate && !environments[1].gate);
        assert_eq!(environments[1].action.kind().as_str(), "webhook");
        let sources = cfg.incident_sources_parsed().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].name, "prod-alerts");
        assert_eq!(sources[0].environment_label, "env");
        assert_eq!(
            sources[0].environment_of("prod").as_deref(),
            Some("production")
        );
        assert!(
            sources[0]
                .secret_file
                .ends_with("/.config/saka/prod-alerts.secret")
        );
        assert!(!sources[0].secret_file.starts_with('~'));
        assert_eq!(sources[1].kind.as_str(), "sentry");
        assert_eq!(sources[1].action.as_str(), "draft");
        assert_eq!(
            cfg.scm[0].client_id.as_deref(),
            Some("Iv23liAbCdEfGh123456")
        );
        assert_eq!(
            (cfg.git.name.as_str(), cfg.git.email.as_str()),
            ("Saka Agent", "agent@saka.invalid")
        );
    }

    #[test]
    fn hooks_are_checked_with_a_coded_error_that_names_the_hook_and_field() {
        let hook = |extra: &str| {
            format!(
                "[[notify.channel]]\nname = \"ops\"\nkind = \"telegram\"\ntoken_file = \"/t\"\nchat = \"-1\"\n[[hook]]\nname = \"tell\"\nproject = \"tasks\"\non = \"merged\"\naction = \"notify\"\nchannel = \"ops\"\n{extra}"
            )
        };
        assert_eq!(Config::parse(&hook("")).unwrap().hook.len(), 1);
        for (extra, code) in [
            ("timeout_secs = 99\n", "hook_timeout_invalid"),
            ("command = \"ls\"\n", "hook_field_invalid"),
            ("codes = [\"ci_red\"]\n", "hook_filter_invalid"),
            ("mode = \"gate\"\ncodes = [\"x\"]\n", "hook_filter_invalid"),
        ] {
            let e = Config::parse(&hook(extra)).unwrap_err();
            assert_eq!(e.code, "config_hook_invalid", "{extra}");
            assert!(e.detail.starts_with(code), "{extra}: {}", e.detail);
        }
        let twice = format!("{}{}", hook(""), hook("").replace("[[notify.channel]]\nname = \"ops\"\nkind = \"telegram\"\ntoken_file = \"/t\"\nchat = \"-1\"\n", ""));
        let e = Config::parse(&twice).unwrap_err();
        assert!(e.detail.starts_with("hook_name_duplicate"), "{}", e.detail);
        let e = Config::parse(&hook("nope = 1\n")).unwrap_err();
        assert_eq!(e.code, "config_unknown_field");
    }

    #[test]
    fn deploy_environments_are_checked_with_a_coded_error_that_names_the_environment_and_field() {
        let deploy = |extra: &str| {
            format!(
                "[[deploy]]\nproject = \"tasks\"\nenvironment = \"staging\"\naction = \"run_host\"\ncommand = \"./deploy.sh\"\n{extra}"
            )
        };
        assert_eq!(
            Config::parse(&deploy("on_failure = \"revert\"\n"))
                .unwrap()
                .deploy
                .len(),
            1
        );
        for (extra, expected) in [
            ("", "deploy_on_failure_required"),
            (
                "on_failure = \"fix\"\nsecrets = [\"X\"]\n",
                "deploy_field_invalid",
            ),
            (
                "on_failure = \"fix\"\ntimeout_secs = 3601\n",
                "deploy_timeout_invalid",
            ),
            (
                "on_failure = \"fix\"\ntarget_url = \"http://example.com\"\n",
                "deploy_url_invalid",
            ),
        ] {
            let e = Config::parse(&deploy(extra)).unwrap_err();
            assert_eq!(e.code, "config_deploy_invalid", "{extra}");
            assert!(e.detail.starts_with(expected), "{extra}: {}", e.detail);
        }
        let twice = format!(
            "{}{}",
            deploy("on_failure = \"fix\"\n"),
            deploy("on_failure = \"fix\"\n")
        );
        let e = Config::parse(&twice).unwrap_err();
        assert!(
            e.detail.starts_with("deploy_environment_duplicate"),
            "{}",
            e.detail
        );
        let e = Config::parse(&deploy("on_failure = \"fix\"\nnope = 1\n")).unwrap_err();
        assert_eq!(e.code, "config_unknown_field");
    }

    #[test]
    fn host_hooks_need_a_config_file_nobody_else_can_write_and_expand_their_directory() {
        use std::os::unix::fs::PermissionsExt;
        let text = "[[hook]]\nname = \"host\"\nproject = \"tasks\"\non = \"merged\"\naction = \"run_host\"\ncommand = \"./x\"\ncwd = \"~/deploy\"\n";
        let cfg = Config::parse(text).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).unwrap();
        for (mode, ok) in [
            (0o600, true),
            (0o644, true),
            (0o664, false),
            (0o666, false),
            (0o602, false),
        ] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            let checked = cfg.check_hook_file(&path);
            assert_eq!(checked.is_ok(), ok, "{mode:o}");
            if !ok {
                assert_eq!(checked.unwrap_err().code, "config_hook_file_writable");
            }
        }
        let plain = Config::parse("[[notify.channel]]\nname = \"ops\"\nkind = \"telegram\"\ntoken_file = \"/t\"\nchat = \"-1\"\n[[hook]]\nname = \"t\"\nproject = \"tasks\"\non = \"merged\"\naction = \"notify\"\nchannel = \"ops\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(
            plain.check_hook_file(&path).is_ok(),
            "only run_host needs the check"
        );

        let hooks = cfg.hooks_parsed().unwrap();
        let saka_core::hook::HookAction::RunHost { cwd, .. } = &hooks[0].action else {
            panic!("not a host hook");
        };
        let cwd = cwd.as_deref().unwrap();
        assert!(cwd.ends_with("/deploy") && !cwd.starts_with('~'), "{cwd}");
    }

    #[test]
    fn the_client_id_and_the_git_identity_are_checked() {
        let scm = |extra: &str| {
            format!(
                "[[scm]]\nid = \"github\"\nprovider = \"github\"\napp_id = 1\nprivate_key_file = \"/k\"\n{extra}"
            )
        };
        let cfg = Config::parse(&scm("client_id = \"Iv1.abc_DEF-9\"\n")).unwrap();
        assert_eq!(cfg.scm[0].client_id.as_deref(), Some("Iv1.abc_DEF-9"));
        assert_eq!(Config::parse(&scm("")).unwrap().scm[0].client_id, None);
        let long = "a".repeat(65);
        for bad in ["", "Iv1 abc", "Iv1/abc", long.as_str()] {
            let e = Config::parse(&scm(&format!("client_id = {bad:?}\n"))).unwrap_err();
            assert_eq!(e.code, "config_invalid", "{bad}");
            assert!(e.detail.starts_with("scm.client_id"), "{}", e.detail);
        }
        let git = Config::parse("[git]\nname = \"Ann\"\nemail = \"ann@example.test\"\n").unwrap();
        assert_eq!(
            (git.git.name.as_str(), git.git.email.as_str()),
            ("Ann", "ann@example.test")
        );
        for bad in [
            "[git]\nname = \"\"\n",
            "[git]\nname = \"Ann <a@b>\"\n",
            "[git]\nname = \"Ann\\nEve\"\n",
            "[git]\nemail = \"no-at.example\"\n",
            "[git]\nemail = \"a@b@c\"\n",
            "[git]\nemail = \"a@\"\n",
        ] {
            let e = Config::parse(bad).unwrap_err();
            assert_eq!(e.code, "config_invalid", "{bad}");
            assert!(e.detail.starts_with("git."), "{}", e.detail);
        }
        for old in [
            "[git]\nauthor_name = \"Saka Agent\"\n",
            "[git]\nauthor_email = \"agent@saka.invalid\"\n",
        ] {
            assert_eq!(
                Config::parse(old).unwrap_err().code,
                "config_unknown_field",
                "{old}"
            );
        }
    }

    #[test]
    fn the_autonomy_ceiling_defaults_to_standard_and_lists_repos_once() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.autonomy_ceilings().allowed("a/b"), Autonomy::Standard);
        let listed = "[autonomy]\ndefault = \"strict\"\n[[autonomy.repo]]\nrepo = \"Acme/Api\"\nlevel = \"autonomous\"\n";
        let ceilings = Config::parse(listed).unwrap().autonomy_ceilings();
        assert_eq!(ceilings.allowed("acme/api"), Autonomy::Autonomous);
        assert_eq!(ceilings.allowed("acme/web"), Autonomy::Strict);
        for bad in [
            "[autonomy]\ndefault = \"reckless\"\n",
            "[[autonomy.repo]]\nrepo = \"acme\"\nlevel = \"strict\"\n",
            "[[autonomy.repo]]\nrepo = \"acme/api\"\nlevel = \"loud\"\n",
            "[[autonomy.repo]]\nrepo = \"acme/api\"\nlevel = \"strict\"\n[[autonomy.repo]]\nrepo = \"ACME/API\"\nlevel = \"standard\"\n",
        ] {
            assert_eq!(
                Config::parse(bad).unwrap_err().code,
                "config_invalid",
                "{bad}"
            );
        }
        assert_eq!(
            Config::parse("[autonomy]\nrepos = []\n").unwrap_err().code,
            "config_unknown_field"
        );
    }
}
