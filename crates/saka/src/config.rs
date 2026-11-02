use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use saka_core::policy::RuleSpec;
use saka_core::vocab::MergeMethod;
use serde::Deserialize;

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
    pub scm: Vec<ScmEntry>,
    #[serde(default)]
    pub agent: Agent,
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub git: Git,
    #[serde(default)]
    pub permission: Permission,
    #[serde(default)]
    pub env: Env,
    #[serde(default)]
    pub toolchain: Toolchain,
}

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

fn url_ok(raw: &str) -> bool {
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
pub struct ScmEntry {
    pub id: String,
    pub provider: String,
    #[serde(default = "github_api")]
    pub api_url: String,
    #[serde(default = "github_web")]
    pub web_url: String,
    pub app_id: u64,
    pub private_key_file: String,
    #[serde(default = "fifteen")]
    pub poll_interval_secs: u64,
    #[serde(default = "six_hundred")]
    pub ci_appear_timeout_secs: u64,
    #[serde(default = "squash")]
    pub merge_method: MergeMethod,
}

fn github_api() -> String {
    "https://api.github.com".into()
}
fn github_web() -> String {
    "https://github.com".into()
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
}

impl Default for BudgetConfig {
    fn default() -> BudgetConfig {
        BudgetConfig {
            warn_percent: eighty(),
            dev: StageBudget::default(),
        }
    }
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
    #[serde(default = "author_name")]
    pub author_name: String,
    #[serde(default = "author_email")]
    pub author_email: String,
}

impl Default for Git {
    fn default() -> Git {
        Git {
            author_name: author_name(),
            author_email: author_email(),
        }
    }
}

fn author_name() -> String {
    "Saka Agent".into()
}
fn author_email() -> String {
    "agent@saka.invalid".into()
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
            Ok(text) => Config::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => Config::parse(""),
            Err(e) => Err(err("config_unreadable", format!("{}: {e}", path.display()))),
        }
    }

    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let cfg: Config = toml::from_str(text).map_err(|e| {
            let message = e.message().to_string();
            if message.starts_with("unknown field") {
                err("config_unknown_field", message)
            } else {
                err("config_invalid", message)
            }
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        self.listen()?;
        if !(1..=16).contains(&self.scheduler.max_active_tasks) {
            return Err(err(
                "config_invalid",
                "scheduler.max_active_tasks must be 1..=16",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for s in &self.scm {
            if !id_ok(&s.id, 32) || !ids.insert(s.id.clone()) {
                return Err(err(
                    "config_invalid",
                    format!("scm.id {:?} is invalid or repeated", s.id),
                ));
            }
            if s.provider != "github" {
                return Err(err(
                    "config_invalid",
                    format!("scm.provider {:?} is not supported", s.provider),
                ));
            }
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
            if !s.web_url.starts_with("https://") || !s.api_url.starts_with("https://") {
                return Err(err(
                    "config_invalid",
                    "scm.web_url and scm.api_url must be https",
                ));
            }
        }
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
        let checks: [(bool, &str); 8] = [
            (
                (1..=168).contains(&a.permission_wait_hours),
                "agent.permission_wait_hours must be 1..=168",
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

    pub fn check_key_files(&self) -> Result<(), ConfigError> {
        for s in &self.scm {
            let path = expand(&s.private_key_file);
            let meta = std::fs::metadata(&path).map_err(|e| {
                err(
                    "config_key_file_unreadable",
                    format!("{}: {e}", path.display()),
                )
            })?;
            let mine =
                std::os::unix::fs::MetadataExt::uid(&meta) == rustix::process::getuid().as_raw();
            if !meta.is_file() || meta.permissions().mode() & 0o777 != 0o600 || !mine {
                return Err(err("config_key_file_mode", path.display().to_string()));
            }
        }
        Ok(())
    }
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
        assert_eq!(cfg.agent.idle_minutes, 30);
        assert_eq!(cfg.budget.warn_percent, 80);
        assert_eq!(
            (cfg.budget.dev.running_minutes, cfg.budget.dev.turns),
            (60, 10)
        );
        for bad in [
            "[agent]\npermission_wait_hours = 0\n",
            "[agent]\nidle_minutes = 1441\n",
            "[agent]\nrate_limit_pause = 0.4\n",
            "[budget]\nwarn_percent = 100\n",
            "[budget.dev]\nturns = 0\n",
            "[budget.dev]\ncost_usd = -1.0\n",
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
    fn the_example_config_parses_with_every_field_set() {
        let cfg = Config::parse(include_str!("../../../docs/config.example.toml")).unwrap();
        assert_eq!(cfg.scm.len(), 1);
        assert_eq!(cfg.scm[0].app_id, 123456);
        assert_eq!(cfg.agent.limits.cpu_quota, "200%");
        assert_eq!(cfg.permission.rule.len(), 3);
        assert_eq!(cfg.env.ports_per_env, 10);
        assert_eq!(cfg.env.limits.tasks_max, 512);
        assert_eq!(cfg.toolchain.npm_registry_url, "https://registry.npmjs.org");
    }
}
