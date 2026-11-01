//! The operator config: `$XDG_CONFIG_HOME/saka/config.toml`, or the file in `SAKA_CONFIG`.
//! Unknown fields are errors; paths may start with `~/`.

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use saka_core::policy::RuleSpec;
use saka_core::vocab::MergeMethod;
use serde::Deserialize;

/// A config problem: a stable code and what it is about.
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
    pub git: Git,
    #[serde(default)]
    pub permission: Permission,
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
    #[serde(default = "sixty")]
    pub max_running_minutes: i64,
    #[serde(default = "one")]
    pub max_followup_turns: u32,
    #[serde(default)]
    pub limits: Limits,
}

impl Default for Agent {
    fn default() -> Agent {
        Agent {
            cli: claude(),
            command: claude(),
            account_dir: None,
            model: String::new(),
            path: default_path(),
            max_running_minutes: sixty(),
            max_followup_turns: one(),
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
fn sixty() -> i64 {
    60
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

/// Where the config file is.
pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SAKA_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".config")))?;
    Some(base.join("saka").join("config.toml"))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `~/x` → `$HOME/x`.
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
    /// Reads the file; a missing file at the default place is the default config.
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
        if self.agent.max_running_minutes < 1 {
            return Err(err(
                "config_invalid",
                "agent.max_running_minutes must be at least 1",
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

    /// Each private key file must be a file of this user with mode 0600.
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

/// Whether a socket address is loopback.
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
    }
}
