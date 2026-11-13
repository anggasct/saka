use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::hook::{
    CALL_TIMEOUT_DEFAULT_SECS, CALL_TIMEOUT_MAX_SECS, COMMAND_TIMEOUT_MAX_SECS, ProjectSelector,
    secret_name_ok, slug_ok, url_ok,
};
use crate::model::ProjectRepo;
use crate::vocab::{DeployActionKind, OnFailure};
use crate::workspace::branch_ok;

pub const MAX_PER_PROJECT: usize = 8;
pub const MAX_TOTAL: usize = 64;
pub const MAX_VERIFY_COMMANDS: usize = 10;
pub const MAX_VERIFY_COMMAND_CHARS: usize = 500;
pub const DEPLOY_TIMEOUT_DEFAULT_SECS: u64 = 900;
pub const READY_TIMEOUT_DEFAULT_SECS: u64 = 300;
pub const READY_TIMEOUT_RANGE: (u64, u64) = (10, 3600);
pub const READY_INTERVAL_DEFAULT_SECS: u64 = 15;
pub const READY_INTERVAL_RANGE: (u64, u64) = (5, 300);
pub const VERIFY_TIMEOUT_DEFAULT_SECS: u64 = 300;
pub const VERIFY_TIMEOUT_MAX_SECS: u64 = 3600;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawEnvironment {
    pub project: String,
    pub environment: String,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    pub action: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub secrets: Option<Vec<String>>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub target_url: Option<String>,
    #[serde(default)]
    pub gate: Option<bool>,
    #[serde(default)]
    pub on_failure: Option<String>,
    #[serde(default)]
    pub verify_secrets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployAction {
    Run {
        command: String,
        secrets: Vec<String>,
    },
    RunHost {
        command: String,
        cwd: Option<String>,
    },
    Webhook {
        url: String,
        secret: String,
    },
}

impl DeployAction {
    pub fn kind(&self) -> DeployActionKind {
        match self {
            DeployAction::Run { .. } => DeployActionKind::Run,
            DeployAction::RunHost { .. } => DeployActionKind::RunHost,
            DeployAction::Webhook { .. } => DeployActionKind::Webhook,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    pub project: ProjectSelector,
    pub name: String,
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub action: DeployAction,
    pub timeout_ms: u64,
    pub target_url: Option<String>,
    pub gate: bool,
    pub on_failure: OnFailure,
    pub verify_secrets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: environment {environment}")]
pub struct EnvironmentIssue {
    pub code: &'static str,
    pub environment: String,
    pub field: Option<&'static str>,
}

fn issue(code: &'static str, environment: &str, field: Option<&'static str>) -> EnvironmentIssue {
    EnvironmentIssue {
        code,
        environment: environment.to_string(),
        field,
    }
}

fn allowed<T>(
    environment: &str,
    name: &'static str,
    value: Option<T>,
    allowed: bool,
    required: bool,
) -> Result<Option<T>, EnvironmentIssue> {
    match (&value, allowed, required) {
        (Some(_), false, _) | (None, true, true) => {
            Err(issue("deploy_field_invalid", environment, Some(name)))
        }
        _ => Ok(value),
    }
}

fn parse_one(raw: &RawEnvironment) -> Result<Environment, EnvironmentIssue> {
    let name = raw.environment.as_str();
    if !slug_ok(name) {
        return Err(issue(
            "deploy_environment_invalid",
            name,
            Some("environment"),
        ));
    }
    let project = ProjectSelector::parse(&raw.project)
        .ok_or_else(|| issue("deploy_field_invalid", name, Some("project")))?;
    for (value, field) in [(&raw.repo, "repo"), (&raw.branch, "branch")] {
        let bad = value.as_deref().is_some_and(|v| {
            if field == "repo" {
                !slug_ok(v)
            } else {
                !branch_ok(v)
            }
        });
        if bad {
            return Err(issue("deploy_field_invalid", name, Some(field)));
        }
    }
    let kind = match raw.action.as_str() {
        "run" => DeployActionKind::Run,
        "run_host" => DeployActionKind::RunHost,
        "webhook" => DeployActionKind::Webhook,
        _ => return Err(issue("deploy_action_invalid", name, Some("action"))),
    };
    let run = kind == DeployActionKind::Run;
    let run_host = kind == DeployActionKind::RunHost;
    let webhook = kind == DeployActionKind::Webhook;
    let command = allowed(name, "command", raw.command.clone(), run || run_host, true)?;
    let secrets = allowed(name, "secrets", raw.secrets.clone(), run, false)?;
    let cwd = allowed(name, "cwd", raw.cwd.clone(), run_host, false)?;
    let url = allowed(name, "url", raw.url.clone(), webhook, true)?;
    let secret = allowed(name, "secret", raw.secret.clone(), webhook, true)?;
    if let Some(command) = &command
        && (command.trim().is_empty() || command.len() > 4096 || command.contains('\0'))
    {
        return Err(issue("deploy_field_invalid", name, Some("command")));
    }
    if let Some(cwd) = &cwd
        && (!(cwd.starts_with('/') || cwd.starts_with("~/")) || cwd.contains('\0'))
    {
        return Err(issue("deploy_field_invalid", name, Some("cwd")));
    }
    let on_failure = match raw.on_failure.as_deref() {
        None => {
            return Err(issue(
                "deploy_on_failure_required",
                name,
                Some("on_failure"),
            ));
        }
        Some(value) => value
            .parse::<OnFailure>()
            .map_err(|_| issue("deploy_field_invalid", name, Some("on_failure")))?,
    };
    if let Some(url) = &url
        && !url_ok(url)
    {
        return Err(issue("deploy_url_invalid", name, Some("url")));
    }
    if let Some(target) = &raw.target_url
        && !url_ok(target)
    {
        return Err(issue("deploy_url_invalid", name, Some("target_url")));
    }
    let distinct = |names: &[String]| names.iter().collect::<BTreeSet<_>>().len() == names.len();
    let secret_lists = [
        secrets.as_deref().unwrap_or_default(),
        raw.verify_secrets.as_slice(),
    ];
    if secret_lists
        .iter()
        .any(|list| list.iter().any(|s| !secret_name_ok(s)) || !distinct(list))
        || secret.as_deref().is_some_and(|s| !secret_name_ok(s))
    {
        return Err(issue("hook_secret_name_invalid", name, Some("secret")));
    }
    let (default, max) = if webhook {
        (CALL_TIMEOUT_DEFAULT_SECS, CALL_TIMEOUT_MAX_SECS)
    } else {
        (DEPLOY_TIMEOUT_DEFAULT_SECS, COMMAND_TIMEOUT_MAX_SECS)
    };
    let timeout = raw.timeout_secs.unwrap_or(default);
    if timeout == 0 || timeout > max {
        return Err(issue("deploy_timeout_invalid", name, Some("timeout_secs")));
    }
    let action = match kind {
        DeployActionKind::Run => DeployAction::Run {
            command: command.unwrap_or_default(),
            secrets: secrets.unwrap_or_default(),
        },
        DeployActionKind::RunHost => DeployAction::RunHost {
            command: command.unwrap_or_default(),
            cwd,
        },
        DeployActionKind::Webhook => DeployAction::Webhook {
            url: url.unwrap_or_default(),
            secret: secret.unwrap_or_default(),
        },
    };
    Ok(Environment {
        project,
        name: raw.environment.clone(),
        repo: raw.repo.clone(),
        branch: raw.branch.clone(),
        action,
        timeout_ms: timeout * 1000,
        target_url: raw.target_url.clone(),
        gate: raw.gate.unwrap_or(true),
        on_failure,
        verify_secrets: raw.verify_secrets.clone(),
    })
}

pub fn parse_all(raw: &[RawEnvironment]) -> Result<Vec<Environment>, EnvironmentIssue> {
    let mut environments = Vec::with_capacity(raw.len());
    let mut seen = BTreeSet::new();
    let mut per_project: BTreeMap<String, usize> = BTreeMap::new();
    for r in raw {
        let environment = parse_one(r)?;
        let key = environment.project.key();
        if !seen.insert((key.clone(), environment.name.clone())) {
            return Err(issue(
                "deploy_environment_duplicate",
                &environment.name,
                Some("environment"),
            ));
        }
        let count = per_project.entry(key).or_default();
        *count += 1;
        if *count > MAX_PER_PROJECT || environments.len() + 1 > MAX_TOTAL {
            return Err(issue("deploy_limit_exceeded", &environment.name, None));
        }
        environments.push(environment);
    }
    Ok(environments)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bound<'a> {
    pub repo: &'a ProjectRepo,
    pub branch: String,
}

impl Environment {
    pub fn bind<'a>(&self, repos: &'a [ProjectRepo]) -> Result<Bound<'a>, &'static str> {
        let repo = match (&self.repo, repos) {
            (Some(name), _) => repos
                .iter()
                .find(|r| &r.name == name)
                .ok_or("deploy_repo_unknown")?,
            (None, [only]) => only,
            (None, _) => return Err("deploy_repo_required"),
        };
        let branch = self
            .branch
            .clone()
            .unwrap_or_else(|| repo.base_branch.clone());
        Ok(Bound { repo, branch })
    }

    pub fn is_for(&self, project_id: crate::ids::ProjectId, project_name: &str) -> bool {
        self.project.selects(project_id, project_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifySpec {
    pub ready: Option<String>,
    pub ready_timeout_secs: u64,
    pub ready_interval_secs: u64,
    pub commands: Vec<String>,
    pub timeout_secs: u64,
}

impl Default for VerifySpec {
    fn default() -> VerifySpec {
        VerifySpec {
            ready: None,
            ready_timeout_secs: READY_TIMEOUT_DEFAULT_SECS,
            ready_interval_secs: READY_INTERVAL_DEFAULT_SECS,
            commands: Vec::new(),
            timeout_secs: VERIFY_TIMEOUT_DEFAULT_SECS,
        }
    }
}

pub fn command_ok(command: &str) -> bool {
    let chars = command.chars().count();
    (1..=MAX_VERIFY_COMMAND_CHARS).contains(&chars)
        && !command.trim().is_empty()
        && !command.contains(['\n', '\r', '\0'])
}

pub fn within(value: u64, (low, high): (u64, u64)) -> bool {
    (low..=high).contains(&value)
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::ids::RepoId;

    fn raw() -> RawEnvironment {
        RawEnvironment {
            project: "saka".into(),
            environment: "staging".into(),
            action: "run_host".into(),
            command: Some("./deploy.sh staging".into()),
            on_failure: Some("revert".into()),
            ..RawEnvironment::default()
        }
    }

    fn webhook() -> RawEnvironment {
        RawEnvironment {
            action: "webhook".into(),
            command: None,
            url: Some("https://ci.example.com/hooks/deploy".into()),
            secret: Some("DEPLOY_SIGNING_KEY".into()),
            ..raw()
        }
    }

    fn code(raw: RawEnvironment) -> &'static str {
        parse_all(&[raw]).map(|_| "ok").unwrap_or_else(|e| e.code)
    }

    fn repo(name: &str, base: &str) -> ProjectRepo {
        ProjectRepo {
            repo_id: RepoId(Uuid::from_u128(1)),
            name: name.into(),
            is_default: true,
            base_branch: base.into(),
            path_scope: Vec::new(),
            autonomy: None,
        }
    }

    #[test]
    fn a_valid_environment_parses_with_its_defaults() {
        let parsed = parse_all(&[raw()]).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].gate);
        assert_eq!(parsed[0].timeout_ms, 900_000);
        assert_eq!(parsed[0].on_failure, OnFailure::Revert);
        let hook = parse_all(&[webhook()]).unwrap();
        assert_eq!(hook[0].timeout_ms, 10_000);
        assert_eq!(hook[0].action.kind(), DeployActionKind::Webhook);
    }

    #[test]
    fn invalid_environments_get_the_first_matching_code() {
        let mut bad = raw();
        bad.environment = "Staging".into();
        assert_eq!(code(bad), "deploy_environment_invalid");
        for action in ["notify", "teleport"] {
            let mut bad = raw();
            bad.action = action.into();
            assert_eq!(code(bad), "deploy_action_invalid", "{action}");
        }
        let mut bad = raw();
        bad.command = None;
        assert_eq!(code(bad), "deploy_field_invalid");
        let mut bad = raw();
        bad.secrets = Some(vec!["DEPLOY_TOKEN".into()]);
        assert_eq!(
            code(bad),
            "deploy_field_invalid",
            "run_host never takes secrets"
        );
        let mut bad = webhook();
        bad.secret = None;
        assert_eq!(code(bad), "deploy_field_invalid");
        let mut bad = webhook();
        bad.command = Some("ls".into());
        assert_eq!(code(bad), "deploy_field_invalid");
        let mut bad = raw();
        bad.cwd = Some("relative".into());
        assert_eq!(code(bad), "deploy_field_invalid");
        let mut bad = raw();
        bad.on_failure = None;
        assert_eq!(code(bad), "deploy_on_failure_required");
        let mut bad = raw();
        bad.on_failure = Some("rollback".into());
        assert_eq!(code(bad), "deploy_field_invalid");
        let mut bad = raw();
        bad.branch = Some("a..b".into());
        assert_eq!(code(bad), "deploy_field_invalid");
    }

    #[test]
    fn urls_secrets_and_timeouts_are_checked() {
        for (field, url) in [
            ("url", "http://ci.example.com/x"),
            ("target_url", "https://user:pw@staging.example.com"),
            ("target_url", "ftp://staging.example.com"),
        ] {
            let mut bad = webhook();
            if field == "url" {
                bad.url = Some(url.into());
            } else {
                bad.target_url = Some(url.into());
            }
            let err = parse_all(&[bad]).unwrap_err();
            assert_eq!((err.code, err.field), ("deploy_url_invalid", Some(field)));
        }
        let mut loopback = raw();
        loopback.target_url = Some("http://127.0.0.1:8080".into());
        assert_eq!(code(loopback), "ok");
        let mut bad = raw();
        bad.verify_secrets = vec!["lower".into()];
        assert_eq!(code(bad), "hook_secret_name_invalid");
        let mut bad = raw();
        bad.verify_secrets = vec!["SMOKE".into(), "SMOKE".into()];
        assert_eq!(code(bad), "hook_secret_name_invalid");
        let mut bad = webhook();
        bad.secret = Some("lower".into());
        assert_eq!(code(bad), "hook_secret_name_invalid");
        for (action, timeout, ok) in [
            ("run_host", 3600, true),
            ("run_host", 3601, false),
            ("run_host", 0, false),
            ("webhook", 30, true),
            ("webhook", 31, false),
        ] {
            let mut env = if action == "webhook" {
                webhook()
            } else {
                raw()
            };
            env.timeout_secs = Some(timeout);
            assert_eq!(code(env) == "ok", ok, "{action} {timeout}");
        }
    }

    #[test]
    fn names_are_unique_per_project_and_counted() {
        assert_eq!(
            parse_all(&[raw(), raw()]).unwrap_err().code,
            "deploy_environment_duplicate"
        );
        let other = RawEnvironment {
            project: "other".into(),
            ..raw()
        };
        assert!(parse_all(&[raw(), other]).is_ok());
        let many: Vec<RawEnvironment> = (0..=MAX_PER_PROJECT)
            .map(|i| RawEnvironment {
                environment: format!("e{i}"),
                ..raw()
            })
            .collect();
        assert_eq!(parse_all(&many).unwrap_err().code, "deploy_limit_exceeded");
        let total: Vec<RawEnvironment> = (0..=MAX_TOTAL)
            .map(|i| RawEnvironment {
                project: format!("p{}", i / MAX_PER_PROJECT),
                environment: format!("e{}", i % MAX_PER_PROJECT),
                ..raw()
            })
            .collect();
        assert_eq!(parse_all(&total).unwrap_err().code, "deploy_limit_exceeded");
    }

    #[test]
    fn an_environment_binds_to_a_repo_and_defaults_to_its_base_branch() {
        let env = parse_all(&[raw()]).unwrap().remove(0);
        let one = [repo("app", "trunk")];
        let bound = env.bind(&one).unwrap();
        assert_eq!(
            (bound.repo.name.as_str(), bound.branch.as_str()),
            ("app", "trunk")
        );
        let two = [repo("app", "main"), repo("web", "main")];
        assert_eq!(env.bind(&two).unwrap_err(), "deploy_repo_required");
        let named = Environment {
            repo: Some("api".into()),
            ..env.clone()
        };
        assert_eq!(named.bind(&two).unwrap_err(), "deploy_repo_unknown");
        let named = Environment {
            repo: Some("web".into()),
            branch: Some("release".into()),
            ..env
        };
        assert_eq!(named.bind(&two).unwrap().branch, "release");
    }

    #[test]
    fn verify_commands_are_single_lines_of_bounded_length() {
        assert!(command_ok("npm run smoke"));
        for bad in ["", "   ", "a\nb", &"x".repeat(501)] {
            assert!(!command_ok(bad));
        }
        assert!(within(10, READY_TIMEOUT_RANGE) && !within(9, READY_TIMEOUT_RANGE));
    }
}
