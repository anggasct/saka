use std::collections::BTreeSet;

use globset::GlobBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::blocker::primary_action;
use crate::deploy::Deployment;
use crate::event::Event;
use crate::ids::{
    BlockerId, DeploymentId, HookRunId, IdSource, InboxItemId, ProjectId, RepoId, ReviewId, SpecId,
    WorkItemId,
};
use crate::model::{Coded, Spec, Task};
use crate::rejection::Rejection;
use crate::time::Timestamp;
use crate::vocab::{
    BlockerAction, BlockerCode, ChangeRole, DeployPhase, DeploymentState, HookActionKind,
    HookEvent, HookMode, HookRunState, InboxKind, InboxSubject, ReviewOutcome,
};

pub const MAX_PER_PROJECT: usize = 32;
pub const MAX_TOTAL: usize = 128;
pub const MAX_FILTER_ENTRIES: usize = 16;
pub const MAX_URL_BYTES: usize = 2048;
pub const OUTPUT_TAIL_BYTES: usize = 8 * 1024;
pub(crate) const COMMAND_TIMEOUT_DEFAULT_SECS: u64 = 300;
pub(crate) const COMMAND_TIMEOUT_MAX_SECS: u64 = 3600;
pub(crate) const CALL_TIMEOUT_DEFAULT_SECS: u64 = 10;
pub(crate) const CALL_TIMEOUT_MAX_SECS: u64 = 30;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawHook {
    pub name: String,
    pub project: String,
    pub on: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub branches: Vec<String>,
    #[serde(default)]
    pub codes: Vec<String>,
    #[serde(default)]
    pub environments: Vec<String>,
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
    pub channel: Option<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectSelector {
    Id(ProjectId),
    Name(String),
}

impl ProjectSelector {
    pub fn parse(raw: &str) -> Option<ProjectSelector> {
        if raw.is_empty() || raw.chars().count() > 80 || raw.chars().any(char::is_control) {
            return None;
        }
        Some(match Uuid::parse_str(raw) {
            Ok(id) => ProjectSelector::Id(ProjectId(id)),
            Err(_) => ProjectSelector::Name(raw.to_string()),
        })
    }

    pub fn selects(&self, id: ProjectId, name: &str) -> bool {
        match self {
            ProjectSelector::Id(own) => *own == id,
            ProjectSelector::Name(own) => own == name,
        }
    }

    pub fn key(&self) -> String {
        match self {
            ProjectSelector::Id(id) => id.to_string(),
            ProjectSelector::Name(name) => name.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookFilter {
    pub repos: Vec<String>,
    pub branches: Vec<String>,
    pub codes: Vec<BlockerCode>,
    pub environments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookAction {
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
    Notify {
        channel: String,
    },
}

impl HookAction {
    pub fn kind(&self) -> HookActionKind {
        match self {
            HookAction::Run { .. } => HookActionKind::Run,
            HookAction::RunHost { .. } => HookActionKind::RunHost,
            HookAction::Webhook { .. } => HookActionKind::Webhook,
            HookAction::Notify { .. } => HookActionKind::Notify,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub name: String,
    pub project: ProjectSelector,
    pub on: HookEvent,
    pub mode: HookMode,
    pub filter: HookFilter,
    pub timeout_ms: u64,
    pub action: HookAction,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: hook {hook}")]
pub struct HookIssue {
    pub code: &'static str,
    pub hook: String,
    pub field: Option<&'static str>,
}

fn issue(code: &'static str, hook: &str, field: Option<&'static str>) -> HookIssue {
    HookIssue {
        code,
        hook: hook.to_string(),
        field,
    }
}

pub fn slug_ok(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 32
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub(crate) fn secret_name_ok(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
        && name.len() <= 64
        && bytes.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost" || host == "::1" || host.starts_with("127.")
}

pub fn url_ok(url: &str) -> bool {
    if url.len() > MAX_URL_BYTES || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return false;
    }
    let host = if let Some(end) = authority.find(']') {
        &authority[..=end]
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    match scheme {
        "https" => !host.is_empty(),
        "http" => loopback_host(host),
        _ => false,
    }
}

pub fn display_url(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or_default().to_string()
}

fn filter_list<'a>(
    hook: &str,
    field: &'static str,
    items: &'a [String],
) -> Result<&'a [String], HookIssue> {
    if items.len() > MAX_FILTER_ENTRIES
        || items
            .iter()
            .any(|i| i.is_empty() || i.len() > 100 || i.chars().any(char::is_control))
    {
        return Err(issue("hook_filter_invalid", hook, Some(field)));
    }
    Ok(items)
}

fn field<T>(
    hook: &str,
    name: &'static str,
    value: Option<T>,
    allowed: bool,
    required: bool,
) -> Result<Option<T>, HookIssue> {
    match (&value, allowed, required) {
        (Some(_), false, _) | (None, true, true) => {
            Err(issue("hook_field_invalid", hook, Some(name)))
        }
        _ => Ok(value),
    }
}

fn parse_one(raw: &RawHook) -> Result<Hook, HookIssue> {
    let name = raw.name.as_str();
    if !slug_ok(name) {
        return Err(issue("hook_name_invalid", name, Some("name")));
    }
    let project = ProjectSelector::parse(&raw.project)
        .ok_or_else(|| issue("hook_field_invalid", name, Some("project")))?;
    let on: HookEvent = raw
        .on
        .parse()
        .map_err(|_| issue("hook_event_unsupported", name, Some("on")))?;
    let mode: HookMode = match raw.mode.as_deref() {
        None => HookMode::After,
        Some(m) => m
            .parse()
            .map_err(|_| issue("hook_mode_invalid", name, Some("mode")))?,
    };
    if mode == HookMode::Gate && on != HookEvent::Merged {
        return Err(issue("hook_gate_event_invalid", name, Some("mode")));
    }
    let kind: HookActionKind = raw
        .action
        .parse()
        .map_err(|_| issue("hook_action_invalid", name, Some("action")))?;
    let run = kind == HookActionKind::Run;
    let run_host = kind == HookActionKind::RunHost;
    let webhook = kind == HookActionKind::Webhook;
    let notify = kind == HookActionKind::Notify;
    let command = field(name, "command", raw.command.clone(), run || run_host, true)?;
    let secrets = field(name, "secrets", raw.secrets.clone(), run, false)?;
    let cwd = field(name, "cwd", raw.cwd.clone(), run_host, false)?;
    let url = field(name, "url", raw.url.clone(), webhook, true)?;
    let secret = field(name, "secret", raw.secret.clone(), webhook, true)?;
    let channel = field(name, "channel", raw.channel.clone(), notify, true)?;

    let repos = filter_list(name, "repos", &raw.repos)?;
    if repos.iter().any(|r| !slug_ok(r)) {
        return Err(issue("hook_filter_invalid", name, Some("repos")));
    }
    if on == HookEvent::SpecShipped && !repos.is_empty() && !run {
        return Err(issue("hook_filter_invalid", name, Some("repos")));
    }
    let branches = filter_list(name, "branches", &raw.branches)?;
    if on == HookEvent::InboxItemOpened && !(repos.is_empty() && branches.is_empty()) {
        return Err(issue("hook_filter_invalid", name, Some("repos")));
    }
    for pattern in branches {
        let built = GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(false)
            .build();
        if built.is_err() {
            return Err(issue("hook_filter_invalid", name, Some("branches")));
        }
    }
    let environments = filter_list(name, "environments", &raw.environments)?;
    let on_deploy = matches!(on, HookEvent::Deployed | HookEvent::DeployFailed);
    if environments.iter().any(|e| !slug_ok(e)) || (!environments.is_empty() && !on_deploy) {
        return Err(issue("hook_filter_invalid", name, Some("environments")));
    }
    let codes_raw = filter_list(name, "codes", &raw.codes)?;
    if !codes_raw.is_empty() && on != HookEvent::Blocked {
        return Err(issue("hook_filter_invalid", name, Some("codes")));
    }
    let codes = codes_raw
        .iter()
        .map(|c| c.parse::<BlockerCode>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| issue("hook_filter_invalid", name, Some("codes")))?;
    if run {
        let commit_event = matches!(
            on,
            HookEvent::ChangeOpened
                | HookEvent::Merged
                | HookEvent::TaskDone
                | HookEvent::Deployed
                | HookEvent::DeployFailed
        );
        let tip_event = on == HookEvent::SpecShipped && repos.len() == 1;
        if !commit_event && !tip_event {
            return Err(issue("hook_run_event_invalid", name, Some("on")));
        }
    }
    if let Some(url) = &url
        && !url_ok(url)
    {
        return Err(issue("hook_url_invalid", name, Some("url")));
    }
    let secret_names: Vec<&String> = secrets.iter().flatten().chain(secret.iter()).collect();
    if secret_names.iter().any(|s| !secret_name_ok(s))
        || secrets.iter().flatten().collect::<BTreeSet<_>>().len()
            != secrets.iter().flatten().count()
    {
        return Err(issue("hook_secret_name_invalid", name, Some("secret")));
    }
    if let Some(command) = &command
        && (command.trim().is_empty() || command.len() > 4096 || command.contains('\0'))
    {
        return Err(issue("hook_field_invalid", name, Some("command")));
    }
    if let Some(channel) = &channel
        && !slug_ok(channel)
    {
        return Err(issue("hook_field_invalid", name, Some("channel")));
    }
    if let Some(cwd) = &cwd
        && (!(cwd.starts_with('/') || cwd.starts_with("~/")) || cwd.contains('\0'))
    {
        return Err(issue("hook_field_invalid", name, Some("cwd")));
    }
    let (default, max) = if run || run_host {
        (COMMAND_TIMEOUT_DEFAULT_SECS, COMMAND_TIMEOUT_MAX_SECS)
    } else {
        (CALL_TIMEOUT_DEFAULT_SECS, CALL_TIMEOUT_MAX_SECS)
    };
    let timeout = raw.timeout_secs.unwrap_or(default);
    if timeout == 0 || timeout > max {
        return Err(issue("hook_timeout_invalid", name, Some("timeout_secs")));
    }
    let action = match kind {
        HookActionKind::Run => HookAction::Run {
            command: command.unwrap_or_default(),
            secrets: secrets.unwrap_or_default(),
        },
        HookActionKind::RunHost => HookAction::RunHost {
            command: command.unwrap_or_default(),
            cwd,
        },
        HookActionKind::Webhook => HookAction::Webhook {
            url: url.unwrap_or_default(),
            secret: secret.unwrap_or_default(),
        },
        HookActionKind::Notify => HookAction::Notify {
            channel: channel.unwrap_or_default(),
        },
    };
    Ok(Hook {
        name: raw.name.clone(),
        project,
        on,
        mode,
        filter: HookFilter {
            repos: repos.to_vec(),
            branches: branches.to_vec(),
            codes,
            environments: environments.to_vec(),
        },
        timeout_ms: timeout * 1000,
        action,
    })
}

pub fn parse_all(raw: &[RawHook]) -> Result<Vec<Hook>, HookIssue> {
    let mut hooks = Vec::with_capacity(raw.len());
    let mut seen = BTreeSet::new();
    let mut per_project: std::collections::BTreeMap<String, usize> = Default::default();
    for r in raw {
        let hook = parse_one(r)?;
        if !seen.insert((hook.project.key(), hook.name.clone())) {
            return Err(issue("hook_name_duplicate", &hook.name, Some("name")));
        }
        let count = per_project.entry(hook.project.key()).or_default();
        *count += 1;
        if *count > MAX_PER_PROJECT || hooks.len() + 1 > MAX_TOTAL {
            return Err(issue("hook_limit_exceeded", &hook.name, None));
        }
        hooks.push(hook);
    }
    Ok(hooks)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedRef {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRef {
    pub id: RepoId,
    pub name: String,
    pub full_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRef {
    pub id: WorkItemId,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecRef {
    pub id: SpecId,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRef {
    pub number: u64,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockerRef {
    pub id: BlockerId,
    pub code: BlockerCode,
    pub action: Option<BlockerAction>,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxRef {
    pub id: InboxItemId,
    pub kind: InboxKind,
    pub subject: InboxSubject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRef {
    pub id: ReviewId,
    pub outcome: ReviewOutcome,
    pub head_sha: String,
    pub findings: usize,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentRef {
    pub id: DeploymentId,
    pub environment: String,
    pub state: DeploymentState,
    pub phase: Option<DeployPhase>,
    pub sha: Option<String>,
    pub previous_sha: Option<String>,
    pub target_url: Option<String>,
}

pub const REVIEW_SUMMARY_CHARS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookContext {
    pub event: HookEvent,
    pub project: NamedRef,
    pub repo: Option<RepoRef>,
    pub task: Option<TaskRef>,
    pub revert_of: Option<TaskRef>,
    pub spec: Option<SpecRef>,
    pub change: Option<ChangeRef>,
    pub branch: Option<String>,
    pub sha: Option<String>,
    pub blocker: Option<BlockerRef>,
    pub review: Option<Box<ReviewRef>>,
    pub inbox: Option<InboxRef>,
    pub deployment: Option<DeploymentRef>,
    pub occurred_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub project_id: ProjectId,
    pub project_name: String,
    pub repo: Option<RepoRef>,
    pub spec_path: Option<String>,
}

pub fn task_context(
    event: &Event,
    task: &Task,
    facts: &Facts,
    now: Timestamp,
) -> Option<HookContext> {
    let code_change = task
        .change_request
        .as_ref()
        .filter(|c| c.role == ChangeRole::Code);
    let mut review = None;
    let (on, sha, blocker) = match event {
        Event::ChangeRequestOpened {
            role: ChangeRole::Code,
            head_sha,
            ..
        } => (HookEvent::ChangeOpened, Some(head_sha.clone()), None),
        Event::ChangeRequestMerged {
            role: ChangeRole::Code,
            merge_commit_sha,
            ..
        } => (HookEvent::Merged, Some(merge_commit_sha.clone()), None),
        Event::WorkItemDone {
            merge_commit_sha, ..
        } => (HookEvent::TaskDone, Some(merge_commit_sha.clone()), None),
        Event::ReviewRecorded {
            review_id,
            head_sha,
            outcome,
            summary,
            findings,
            ..
        } => {
            review = Some(Box::new(ReviewRef {
                id: *review_id,
                outcome: *outcome,
                head_sha: head_sha.clone(),
                findings: findings.len(),
                summary: summary.chars().take(REVIEW_SUMMARY_CHARS).collect(),
            }));
            (HookEvent::ReviewDone, Some(head_sha.clone()), None)
        }
        Event::WorkItemBlocked {
            blocker_id,
            code,
            params,
            ..
        } if *code != BlockerCode::HookFailed => (
            HookEvent::Blocked,
            None,
            Some(BlockerRef {
                id: *blocker_id,
                code: *code,
                action: primary_action(task).map(|a| a.kind),
                params: shared_params(*code, params),
            }),
        ),
        _ => return None,
    };
    let item = &task.item;
    Some(HookContext {
        event: on,
        project: NamedRef {
            id: facts.project_id.0,
            name: facts.project_name.clone(),
        },
        repo: facts.repo.clone(),
        task: Some(TaskRef {
            id: item.id,
            key: item.task_key.clone(),
        }),
        revert_of: task.revert.as_ref().map(|r| TaskRef {
            id: r.of,
            key: r.of_key.clone(),
        }),
        spec: item
            .spec_id
            .zip(facts.spec_path.clone())
            .map(|(id, path)| SpecRef { id, path }),
        change: code_change.map(|c| ChangeRef {
            number: c.number,
            url: c.url.clone(),
        }),
        branch: code_change.map(|c| c.base_branch.clone()),
        sha,
        blocker,
        review,
        inbox: None,
        deployment: None,
        occurred_at: now.to_rfc3339(),
    })
}

fn shared_params(code: BlockerCode, params: &Value) -> Option<Value> {
    if code != BlockerCode::DeployUnverified {
        return None;
    }
    let shared: serde_json::Map<String, Value> = ["environment", "deployment_id", "phase", "code"]
        .iter()
        .filter_map(|key| params.get(*key).map(|v| ((*key).to_string(), v.clone())))
        .collect();
    Some(Value::Object(shared))
}

pub fn spec_context(spec: &Spec, project_name: &str, now: Timestamp) -> HookContext {
    HookContext {
        event: HookEvent::SpecShipped,
        project: NamedRef {
            id: spec.project_id.0,
            name: project_name.to_string(),
        },
        repo: None,
        task: None,
        revert_of: None,
        spec: Some(SpecRef {
            id: spec.id,
            path: spec.path.clone(),
        }),
        change: None,
        branch: None,
        sha: None,
        blocker: None,
        review: None,
        inbox: None,
        deployment: None,
        occurred_at: now.to_rfc3339(),
    }
}

pub fn inbox_context(
    inbox: InboxRef,
    project: NamedRef,
    task: Option<TaskRef>,
    spec: Option<SpecRef>,
    now: Timestamp,
) -> HookContext {
    HookContext {
        event: HookEvent::InboxItemOpened,
        project,
        repo: None,
        task,
        revert_of: None,
        spec,
        change: None,
        branch: None,
        sha: None,
        blocker: None,
        review: None,
        inbox: Some(inbox),
        deployment: None,
        occurred_at: now.to_rfc3339(),
    }
}

pub fn deployment_context(
    deployment: &Deployment,
    facts: &Facts,
    event: HookEvent,
    now: Timestamp,
) -> HookContext {
    HookContext {
        event,
        project: NamedRef {
            id: facts.project_id.0,
            name: facts.project_name.clone(),
        },
        repo: facts.repo.clone(),
        task: None,
        revert_of: None,
        spec: None,
        change: None,
        branch: Some(deployment.branch.clone()),
        sha: deployment.sha.clone(),
        blocker: None,
        review: None,
        inbox: None,
        deployment: Some(DeploymentRef {
            id: deployment.id,
            environment: deployment.environment.clone(),
            state: deployment.state,
            phase: deployment.phase,
            sha: deployment.sha.clone(),
            previous_sha: deployment.previous_sha.clone(),
            target_url: deployment.target_url.clone(),
        }),
        occurred_at: now.to_rfc3339(),
    }
}

pub(crate) fn clean(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}

impl HookContext {
    pub fn env_vars(&self, run: HookRunId, hook: &str) -> Vec<(String, String)> {
        let mut vars = vec![
            ("SAKA_EVENT".to_string(), self.event.as_str().to_string()),
            ("SAKA_PROJECT".to_string(), clean(&self.project.name)),
            ("SAKA_HOOK".to_string(), clean(hook)),
            ("SAKA_HOOK_RUN".to_string(), run.to_string()),
        ];
        let mut set = |name: &str, value: Option<String>| {
            if let Some(value) = value {
                vars.push((name.to_string(), clean(&value)));
            }
        };
        set("SAKA_REPO", self.repo.as_ref().map(|r| r.name.clone()));
        set("SAKA_SHA", self.sha.clone());
        set(
            "SAKA_PR",
            self.change.as_ref().map(|c| c.number.to_string()),
        );
        set("SAKA_TASK_KEY", self.task.as_ref().map(|t| t.key.clone()));
        set("SAKA_SPEC", self.spec.as_ref().map(|s| s.path.clone()));
        set("SAKA_BRANCH", self.branch.clone());
        set(
            "SAKA_INBOX_KIND",
            self.inbox.as_ref().map(|i| i.kind.as_str().to_string()),
        );
        set(
            "SAKA_INBOX_SUBJECT",
            self.inbox.as_ref().map(|i| i.subject.as_str().to_string()),
        );
        set(
            "SAKA_REVIEW_OUTCOME",
            self.review.as_ref().map(|r| r.outcome.as_str().to_string()),
        );
        set(
            "SAKA_DEPLOY_ID",
            self.deployment.as_ref().map(|d| d.id.to_string()),
        );
        set(
            "SAKA_DEPLOY_ENV",
            self.deployment.as_ref().map(|d| d.environment.clone()),
        );
        set(
            "SAKA_DEPLOY_URL",
            self.deployment.as_ref().and_then(|d| d.target_url.clone()),
        );
        set(
            "SAKA_PREVIOUS_SHA",
            self.deployment
                .as_ref()
                .and_then(|d| d.previous_sha.clone()),
        );
        vars
    }

    pub fn payload(&self, run: HookRunId, hook: &str) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_else(|_| json!({}));
        value["id"] = json!(run);
        value["hook"] = json!(hook);
        value
    }
}

pub fn matches(hook: &Hook, ctx: &HookContext) -> bool {
    if !hook
        .project
        .selects(ProjectId(ctx.project.id), &ctx.project.name)
        || hook.on != ctx.event
    {
        return false;
    }
    let f = &hook.filter;
    if ctx.event != HookEvent::SpecShipped
        && !f.repos.is_empty()
        && !ctx.repo.as_ref().is_some_and(|r| f.repos.contains(&r.name))
    {
        return false;
    }
    if !f.branches.is_empty() {
        let hit = ctx.branch.as_deref().is_some_and(|branch| {
            f.branches.iter().any(|p| {
                GlobBuilder::new(p)
                    .literal_separator(true)
                    .backslash_escape(false)
                    .build()
                    .is_ok_and(|g| g.compile_matcher().is_match(branch))
            })
        });
        if !hit {
            return false;
        }
    }
    if !f.environments.is_empty()
        && !ctx
            .deployment
            .as_ref()
            .is_some_and(|d| f.environments.contains(&d.environment))
    {
        return false;
    }
    if !f.codes.is_empty() {
        return ctx
            .blocker
            .as_ref()
            .is_some_and(|b| f.codes.contains(&b.code));
    }
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookRun {
    pub id: HookRunId,
    pub project_id: ProjectId,
    pub hook: String,
    pub action: HookActionKind,
    pub mode: HookMode,
    pub trigger: HookEvent,
    pub state: HookRunState,
    pub work_item_id: Option<WorkItemId>,
    pub spec_id: Option<SpecId>,
    pub repo_id: Option<RepoId>,
    pub sha: Option<String>,
    pub context: HookContext,
    pub source_event_id: Uuid,
    pub rerun_of: Option<HookRunId>,
    pub attempts: u32,
    pub result: Option<Value>,
    pub error: Option<Coded>,
    pub queued_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub duration_ms: Option<i64>,
}

fn queued(
    hook: &Hook,
    ctx: &HookContext,
    source_event_id: Uuid,
    rerun_of: Option<HookRunId>,
    ids: &mut dyn IdSource,
) -> Event {
    Event::HookRunQueued {
        hook_run_id: HookRunId::new(ids),
        project_id: ProjectId(ctx.project.id),
        hook: hook.name.clone(),
        action: hook.action.kind(),
        mode: hook.mode,
        trigger: ctx.event,
        work_item_id: ctx.task.as_ref().map(|t| t.id),
        spec_id: ctx.spec.as_ref().map(|s| s.id),
        repo_id: ctx.repo.as_ref().map(|r| r.id),
        sha: ctx.sha.clone(),
        context: ctx.clone(),
        source_event_id,
        rerun_of,
    }
}

pub fn plan(
    hooks: &[Hook],
    ctx: &HookContext,
    source_event_id: Uuid,
    ids: &mut dyn IdSource,
) -> Vec<Event> {
    hooks
        .iter()
        .filter(|h| matches(h, ctx))
        .map(|h| queued(h, ctx, source_event_id, None, ids))
        .collect()
}

pub fn rerun(
    run: &HookRun,
    current: Option<&Hook>,
    ids: &mut dyn IdSource,
) -> Result<Event, Rejection> {
    if run.state == HookRunState::Pending {
        return Err(Rejection::HookRunPending);
    }
    let hook = current
        .filter(|h| h.on == run.trigger && h.mode == run.mode && h.action.kind() == run.action)
        .ok_or_else(|| Rejection::HookNotConfigured {
            hook: run.hook.clone(),
        })?;
    Ok(queued(
        hook,
        &run.context,
        run.source_event_id,
        Some(run.id),
        ids,
    ))
}

pub fn skipped(run: &HookRun, ids: &mut dyn IdSource) -> [Event; 2] {
    let id = HookRunId::new(ids);
    [
        Event::HookRunQueued {
            hook_run_id: id,
            project_id: run.project_id,
            hook: run.hook.clone(),
            action: run.action,
            mode: run.mode,
            trigger: run.trigger,
            work_item_id: run.work_item_id,
            spec_id: run.spec_id,
            repo_id: run.repo_id,
            sha: run.sha.clone(),
            context: run.context.clone(),
            source_event_id: run.source_event_id,
            rerun_of: Some(run.id),
        },
        Event::HookRunFinished {
            hook_run_id: id,
            outcome: HookRunState::Succeeded,
            attempts: 0,
            duration_ms: 0,
            result: json!({
                "exit_code": null,
                "timed_out": false,
                "http_status": null,
                "log": null,
                "output_tail": null,
                "skipped": "hook_removed",
            }),
            error: None,
        },
    ]
}

pub fn finished(
    run: &HookRun,
    outcome: HookRunState,
    attempts: u32,
    duration_ms: i64,
    result: Value,
    error: Option<Coded>,
) -> Option<Event> {
    (run.state == HookRunState::Pending && outcome != HookRunState::Pending).then_some(
        Event::HookRunFinished {
            hook_run_id: run.id,
            outcome,
            attempts,
            duration_ms,
            result,
            error,
        },
    )
}

pub fn evolve(run: Option<HookRun>, event: &Event, now: Timestamp) -> Option<HookRun> {
    match (run, event) {
        (
            None,
            Event::HookRunQueued {
                hook_run_id,
                project_id,
                hook,
                action,
                mode,
                trigger,
                work_item_id,
                spec_id,
                repo_id,
                sha,
                context,
                source_event_id,
                rerun_of,
            },
        ) => Some(HookRun {
            id: *hook_run_id,
            project_id: *project_id,
            hook: hook.clone(),
            action: *action,
            mode: *mode,
            trigger: *trigger,
            state: HookRunState::Pending,
            work_item_id: *work_item_id,
            spec_id: *spec_id,
            repo_id: *repo_id,
            sha: sha.clone(),
            context: context.clone(),
            source_event_id: *source_event_id,
            rerun_of: *rerun_of,
            attempts: 0,
            result: None,
            error: None,
            queued_at: now,
            finished_at: None,
            duration_ms: None,
        }),
        (
            Some(mut run),
            Event::HookRunFinished {
                outcome,
                attempts,
                duration_ms,
                result,
                error,
                ..
            },
        ) => {
            run.state = *outcome;
            run.attempts = *attempts;
            run.duration_ms = Some(*duration_ms);
            run.result = Some(result.clone());
            run.error = error.clone();
            run.finished_at = Some(now);
            Some(run)
        }
        (run, _) => run,
    }
}

pub fn tail(text: &str) -> String {
    if text.len() <= OUTPUT_TAIL_BYTES {
        return text.to_string();
    }
    let mut cut = text.len() - OUTPUT_TAIL_BYTES;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text[cut..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(action: &str) -> RawHook {
        RawHook {
            name: "notify-ops".into(),
            project: "saka".into(),
            on: "merged".into(),
            action: action.into(),
            channel: Some("ops".into()),
            ..RawHook::default()
        }
    }

    fn code(raw: RawHook) -> &'static str {
        parse_all(&[raw]).map(|_| "ok").unwrap_or_else(|e| e.code)
    }

    #[test]
    fn the_inbox_event_takes_a_notification_but_no_repo_filter_and_no_command_in_a_checkout() {
        let on_inbox = |action: &str| RawHook {
            on: "inbox_item_opened".into(),
            command: (action != "notify").then(|| "echo".into()),
            channel: (action == "notify").then(|| "ops".into()),
            ..raw(action)
        };
        assert_eq!(code(on_inbox("notify")), "ok");
        assert_eq!(code(on_inbox("run_host")), "ok");
        assert_eq!(code(on_inbox("run")), "hook_run_event_invalid");
        let filtered = RawHook {
            repos: vec!["app".into()],
            ..on_inbox("notify")
        };
        assert_eq!(code(filtered), "hook_filter_invalid");
    }

    #[test]
    fn a_valid_notify_hook_parses_with_defaults() {
        let hooks = parse_all(&[raw("notify")]).unwrap_or_default();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].mode, HookMode::After);
        assert_eq!(hooks[0].timeout_ms, 10_000);
    }

    #[test]
    fn invalid_hooks_get_the_first_matching_code() {
        let mut bad = raw("notify");
        bad.name = "Bad Name".into();
        assert_eq!(code(bad), "hook_name_invalid");
        let mut ok = raw("notify");
        ok.on = "review_done".into();
        assert_eq!(parse_one(&ok).unwrap().on, HookEvent::ReviewDone);
        let mut bad = raw("notify");
        bad.on = "nope".into();
        assert_eq!(code(bad), "hook_event_unsupported");
        let mut bad = raw("notify");
        bad.mode = Some("sometimes".into());
        assert_eq!(code(bad), "hook_mode_invalid");
        let mut bad = raw("notify");
        bad.mode = Some("gate".into());
        bad.on = "task_done".into();
        assert_eq!(code(bad), "hook_gate_event_invalid");
        let mut bad = raw("notify");
        bad.mode = Some("gate".into());
        assert_eq!(code(bad), "ok");
        assert_eq!(code(raw("teleport")), "hook_action_invalid");
        assert_eq!(code(raw("run_host")), "hook_field_invalid");
        let mut bad = raw("notify");
        bad.channel = None;
        assert_eq!(code(bad), "hook_field_invalid");
        let mut bad = raw("notify");
        bad.command = Some("ls".into());
        assert_eq!(code(bad), "hook_field_invalid");
        let mut bad = raw("notify");
        bad.codes = vec!["ci_red".into()];
        assert_eq!(code(bad), "hook_filter_invalid");
        let mut bad = raw("notify");
        bad.timeout_secs = Some(31);
        assert_eq!(code(bad), "hook_timeout_invalid");
        let mut bad = raw("notify");
        bad.project = String::new();
        assert_eq!(code(bad), "hook_field_invalid");
    }

    fn command(action: &str, on: &str) -> RawHook {
        RawHook {
            name: "deploy".into(),
            project: "saka".into(),
            on: on.into(),
            action: action.into(),
            command: Some("./deploy.sh".into()),
            ..RawHook::default()
        }
    }

    #[test]
    fn run_hooks_need_a_commit_to_run_at() {
        for on in ["change_opened", "merged", "task_done"] {
            assert_eq!(code(command("run", on)), "ok", "{on}");
        }
        assert_eq!(code(command("run", "blocked")), "hook_run_event_invalid");
        assert_eq!(
            code(command("run", "spec_shipped")),
            "hook_run_event_invalid"
        );
        let mut tip = command("run", "spec_shipped");
        tip.repos = vec!["api".into()];
        assert_eq!(code(tip.clone()), "ok");
        tip.repos = vec!["api".into(), "web".into()];
        assert_eq!(code(tip), "hook_run_event_invalid");
        let mut other = raw("notify");
        other.on = "spec_shipped".into();
        other.repos = vec!["api".into()];
        assert_eq!(code(other), "hook_filter_invalid");
    }

    #[test]
    fn commands_have_their_own_fields_and_timeouts() {
        let mut run = command("run", "merged");
        run.secrets = Some(vec!["DEPLOY_TOKEN".into()]);
        assert_eq!(code(run.clone()), "ok");
        assert_eq!(hook(run.clone()).timeout_ms, 300_000);
        run.secrets = Some(vec!["DEPLOY_TOKEN".into(), "DEPLOY_TOKEN".into()]);
        assert_eq!(code(run.clone()), "hook_secret_name_invalid");
        run.secrets = Some(vec!["bad".into()]);
        assert_eq!(code(run.clone()), "hook_secret_name_invalid");
        run.secrets = None;
        run.timeout_secs = Some(3601);
        assert_eq!(code(run.clone()), "hook_timeout_invalid");
        run.timeout_secs = Some(3600);
        assert_eq!(code(run.clone()), "ok");
        run.command = None;
        assert_eq!(code(run), "hook_field_invalid");

        let mut host = command("run_host", "task_done");
        host.cwd = Some("~/deploy".into());
        assert_eq!(code(host.clone()), "ok");
        host.cwd = Some("relative/dir".into());
        assert_eq!(code(host.clone()), "hook_field_invalid");
        host.cwd = None;
        host.secrets = Some(vec!["DEPLOY_TOKEN".into()]);
        assert_eq!(
            code(host),
            "hook_field_invalid",
            "run_host never receives secrets"
        );
        let mut host = command("run_host", "blocked");
        assert_eq!(code(host.clone()), "ok", "run_host needs no commit");
        host.command = Some("   ".into());
        assert_eq!(code(host), "hook_field_invalid");
    }

    #[test]
    fn the_environment_of_a_command_carries_only_values_that_exist() {
        let mut ctx = ctx(HookEvent::Merged);
        ctx.sha = Some("abc\ndef".into());
        ctx.change = Some(ChangeRef {
            number: 42,
            url: "u".into(),
        });
        let run = HookRunId(Uuid::from_u128(9));
        let vars: std::collections::BTreeMap<_, _> =
            ctx.env_vars(run, "deploy").into_iter().collect();
        assert_eq!(vars["SAKA_EVENT"], "merged");
        assert_eq!(vars["SAKA_PROJECT"], "saka");
        assert_eq!(vars["SAKA_REPO"], "api");
        assert_eq!(vars["SAKA_SHA"], "abcdef", "control characters are dropped");
        assert_eq!(vars["SAKA_PR"], "42");
        assert_eq!(vars["SAKA_HOOK_RUN"], run.to_string());
        assert_eq!(vars["SAKA_BRANCH"], "release/1");
        assert!(!vars.contains_key("SAKA_TASK_KEY"));
        assert!(!vars.contains_key("SAKA_SPEC"));
        assert!(vars.keys().all(|k| k.starts_with("SAKA_")));
    }

    #[test]
    fn webhook_urls_must_be_https_without_credentials() {
        let mut hook = raw("webhook");
        hook.channel = None;
        hook.secret = Some("SIGNING_KEY".into());
        for (url, ok) in [
            ("https://ci.example.com/saka", true),
            ("https://ci.example.com/saka?token=1", true),
            ("http://127.0.0.1:9000/x", true),
            ("http://localhost/x", true),
            ("http://ci.example.com/x", false),
            ("https://user:pw@ci.example.com/x", false),
            ("https://", false),
            ("ftp://ci.example.com", false),
            ("https://ci.example.com/a b", false),
        ] {
            hook.url = Some(url.into());
            assert_eq!(code(hook.clone()) == "ok", ok, "{url}");
        }
        hook.url = Some("https://ci.example.com/x".into());
        hook.secret = Some("lower".into());
        assert_eq!(code(hook.clone()), "hook_secret_name_invalid");
        hook.secret = None;
        assert_eq!(code(hook), "hook_field_invalid");
    }

    #[test]
    fn urls_are_displayed_without_query_or_fragment() {
        assert_eq!(
            display_url("https://ci.example.com/saka?token=1#x"),
            "https://ci.example.com/saka"
        );
    }

    #[test]
    fn duplicates_and_limits_are_refused() {
        let hook = raw("notify");
        assert_eq!(
            parse_all(&[hook.clone(), hook.clone()])
                .err()
                .map(|e| e.code),
            Some("hook_name_duplicate")
        );
        let many: Vec<RawHook> = (0..=MAX_PER_PROJECT)
            .map(|i| RawHook {
                name: format!("h{i}"),
                ..hook.clone()
            })
            .collect();
        assert_eq!(
            parse_all(&many).err().map(|e| e.code),
            Some("hook_limit_exceeded")
        );
    }

    fn ctx(event: HookEvent) -> HookContext {
        HookContext {
            event,
            project: NamedRef {
                id: Uuid::from_u128(1),
                name: "saka".into(),
            },
            repo: Some(RepoRef {
                id: RepoId(Uuid::from_u128(2)),
                name: "api".into(),
                full_name: "acme/api".into(),
            }),
            task: None,
            revert_of: None,
            spec: None,
            change: None,
            branch: Some("release/1".into()),
            sha: None,
            blocker: Some(BlockerRef {
                id: BlockerId(Uuid::from_u128(3)),
                code: BlockerCode::CiRed,
                action: Some(BlockerAction::Retry),
                params: None,
            }),
            review: None,
            inbox: None,
            deployment: None,
            occurred_at: Timestamp(0).to_rfc3339(),
        }
    }

    fn hook(raw: RawHook) -> Hook {
        parse_all(&[raw]).unwrap_or_default().remove(0)
    }

    #[test]
    fn a_hook_matches_its_project_event_and_filters() {
        let mut base = raw("notify");
        base.project = Uuid::from_u128(1).to_string();
        assert!(matches(&hook(base.clone()), &ctx(HookEvent::Merged)));
        assert!(!matches(&hook(base.clone()), &ctx(HookEvent::TaskDone)));

        let mut other = base.clone();
        other.project = "other".into();
        assert!(!matches(&hook(other.clone()), &ctx(HookEvent::Merged)));
        other.project = "saka".into();
        assert!(matches(&hook(other), &ctx(HookEvent::Merged)));

        let mut f = base.clone();
        f.repos = vec!["web".into()];
        assert!(!matches(&hook(f.clone()), &ctx(HookEvent::Merged)));
        f.repos = vec!["web".into(), "api".into()];
        assert!(matches(&hook(f), &ctx(HookEvent::Merged)));

        let mut f = base.clone();
        f.branches = vec!["main".into()];
        assert!(!matches(&hook(f.clone()), &ctx(HookEvent::Merged)));
        f.branches = vec!["release/*".into()];
        assert!(matches(&hook(f), &ctx(HookEvent::Merged)));

        let mut f = base;
        f.on = "blocked".into();
        f.codes = vec!["ci_red".into()];
        assert!(matches(&hook(f.clone()), &ctx(HookEvent::Blocked)));
        f.codes = vec!["dod_failed".into()];
        assert!(!matches(&hook(f), &ctx(HookEvent::Blocked)));
    }

    fn on_deploy(on: &str, extra: impl FnOnce(&mut RawHook)) -> RawHook {
        let mut hook = raw("notify");
        hook.on = on.into();
        extra(&mut hook);
        hook
    }

    #[test]
    fn deploy_events_take_an_environment_filter_and_never_gate() {
        for on in ["deployed", "deploy_failed"] {
            assert_eq!(
                code(on_deploy(on, |h| h.environments = vec!["staging".into()])),
                "ok",
                "{on}"
            );
            assert_eq!(
                code(on_deploy(on, |h| h.mode = Some("gate".into()))),
                "hook_gate_event_invalid",
                "{on}"
            );
            let mut run = command("run", on);
            run.environments = vec!["staging".into()];
            assert_eq!(code(run), "ok", "a run hook checks out the deployed sha");
        }
        assert_eq!(
            code(on_deploy("merged", |h| h.environments = vec!["staging".into()])),
            "hook_filter_invalid"
        );
        assert_eq!(
            code(on_deploy("deployed", |h| h.environments = vec!["Bad Name".into()])),
            "hook_filter_invalid"
        );
    }

    fn deployed_ctx(environment: &str) -> HookContext {
        let mut ctx = ctx(HookEvent::Deployed);
        ctx.deployment = Some(DeploymentRef {
            id: DeploymentId(Uuid::from_u128(7)),
            environment: environment.into(),
            state: DeploymentState::Succeeded,
            phase: Some(DeployPhase::Verify),
            sha: Some("a".repeat(40)),
            previous_sha: Some("b".repeat(40)),
            target_url: Some("https://staging.example.com".into()),
        });
        ctx
    }

    #[test]
    fn a_deploy_hook_matches_only_its_environments_and_the_context_names_the_deployment() {
        let all = hook(on_deploy("deployed", |_| {}));
        let staging = hook(on_deploy("deployed", |h| {
            h.environments = vec!["staging".into()]
        }));
        let mut other = deployed_ctx("preview");
        other.project.name = "saka".into();
        assert!(matches(&all, &other));
        assert!(!matches(&staging, &other));
        let mut ours = deployed_ctx("staging");
        ours.project.name = "saka".into();
        assert!(matches(&staging, &ours));
        let vars: std::collections::BTreeMap<_, _> = ours
            .env_vars(HookRunId(Uuid::from_u128(9)), "after")
            .into_iter()
            .collect();
        assert_eq!(vars["SAKA_DEPLOY_ENV"], "staging");
        assert_eq!(vars["SAKA_DEPLOY_URL"], "https://staging.example.com");
        assert_eq!(vars["SAKA_DEPLOY_ID"], Uuid::from_u128(7).to_string());
        assert_eq!(vars["SAKA_PREVIOUS_SHA"], "b".repeat(40));
        assert!(
            !ctx(HookEvent::Merged)
                .env_vars(HookRunId(Uuid::from_u128(9)), "x")
                .iter()
                .any(|(k, _)| k.starts_with("SAKA_DEPLOY"))
        );
    }
}
