use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::{BoxFuture, FutureExt, Shared, join, join_all};
use futures_util::stream::{self, StreamExt};
use saka_core::diagnostics::{
    self as eval, AppRequirements, AppStatus, CheckResult, Finding, Fix, HostVersion,
    MigrationState, RepoToolchain, Report, ScopeOverlap, SourceFailure, Subject, ToolchainDrift,
};
use saka_core::ids::{ProjectId, RepoId};
use saka_core::model::{Project, ProjectRepo, Repo};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    AuthorizationFailure, AuthorizationState, CheckId, DiagnosticStatus, HostTool, ReportMode,
    SubjectKind, Tool,
};
use saka_ports::{Agents, PortError, PortResult, ReleaseIndex, RepoRef, Supervisor, Toolchains};
use saka_store::authorship as authorship_rows;
use saka_store::user_link as link_rows;
use saka_store::{list, read};
use serde_json::json;
use tokio::time::Instant;

use crate::recipe::RECIPE_PATH;
use crate::versions::{
    self, FileUse, Proposal, ProposalAction, RepoFiles, VersionStatus, VersionTool,
};
use crate::views::scope_warnings;
use crate::{Engine, EngineError, OPERATOR, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticLimits {
    pub concurrency: usize,
    pub check: Duration,
    pub report: Duration,
}

impl Default for DiagnosticLimits {
    fn default() -> DiagnosticLimits {
        DiagnosticLimits {
            concurrency: 8,
            check: Duration::from_secs(10),
            report: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticFilter {
    pub checks: Option<BTreeSet<CheckId>>,
    pub project_id: Option<ProjectId>,
}

impl DiagnosticFilter {
    fn wants(&self, id: CheckId) -> bool {
        self.checks.as_ref().is_none_or(|c| c.contains(&id))
    }
}

pub struct Host {
    pub agents: Arc<dyn Agents>,
    pub supervisor: Arc<dyn Supervisor>,
    pub toolchains: Arc<dyn Toolchains>,
}

struct Planned<'a> {
    id: CheckId,
    subject: Subject,
    run: BoxFuture<'a, Vec<Finding>>,
}

fn subject(id: CheckId, account: &str) -> Subject {
    match id.subject_kind() {
        SubjectKind::AgentAccount => Subject::agent_account(account),
        _ => Subject::instance(),
    }
}

fn one(result: PortResult<Finding>) -> Vec<Finding> {
    vec![result.unwrap_or_else(|e| eval::check_failed(&e.code))]
}

fn source_failure(e: &PortError) -> SourceFailure {
    SourceFailure {
        url: e
            .params
            .get("url")
            .and_then(|u| u.as_str())
            .map(String::from),
        status: e
            .params
            .get("status")
            .and_then(serde_json::Value::as_u64)
            .and_then(|s| u16::try_from(s).ok()),
    }
}

async fn sources(releases: &dyn ReleaseIndex) -> Vec<Finding> {
    let results = join_all(Tool::ALL.iter().map(|tool| async move {
        let failure = releases
            .releases(*tool)
            .await
            .err()
            .map(|e| source_failure(&e));
        (*tool, failure)
    }))
    .await;
    eval::toolchain_sources(&results)
}

fn host_check<'a>(
    host: &'a HostRefs<'a>,
    id: CheckId,
    account: &'a str,
) -> Option<BoxFuture<'a, Vec<Finding>>> {
    Some(match id {
        CheckId::RuntimeSupervisor => {
            async move { one(host.supervisor.probe().await.map(|p| eval::supervisor(&p))) }.boxed()
        }
        CheckId::AgentBinary => async move {
            one(host
                .agents
                .agent_cli()
                .await
                .map(|c| eval::agent_binary(account, &c)))
        }
        .boxed(),
        CheckId::AgentLogin => async move {
            one(host
                .agents
                .agent_login()
                .await
                .map(|l| eval::agent_login(account, &l)))
        }
        .boxed(),
        CheckId::ToolchainCache => async move {
            one(host
                .toolchains
                .cache()
                .await
                .map(|c| eval::toolchain_cache(&c)))
        }
        .boxed(),
        _ => return None,
    })
}

struct HostRefs<'a> {
    agents: &'a dyn Agents,
    supervisor: &'a dyn Supervisor,
    toolchains: &'a dyn Toolchains,
}

async fn run_one(p: Planned<'_>, deadline: Instant, limits: DiagnosticLimits) -> Vec<CheckResult> {
    let started = Instant::now();
    let by_check = started + limits.check;
    let (until, limit) = if by_check <= deadline {
        (by_check, limits.check)
    } else {
        (deadline, limits.report)
    };
    let findings = match tokio::time::timeout_at(until, p.run).await {
        Ok(findings) => findings,
        Err(_) => vec![eval::timed_out(
            u64::try_from(limit.as_millis()).unwrap_or(u64::MAX),
        )],
    };
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    findings
        .into_iter()
        .map(|finding| CheckResult {
            id: p.id,
            subject: p.subject.clone(),
            finding,
            duration_ms,
        })
        .collect()
}

async fn run(planned: Vec<Planned<'_>>, limits: DiagnosticLimits) -> Vec<CheckResult> {
    let deadline = Instant::now() + limits.report;
    let mut runs = Vec::with_capacity(planned.len());
    for p in planned {
        runs.push(run_one(p, deadline, limits));
    }
    stream::iter(runs)
        .buffer_unordered(limits.concurrency.max(1))
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect()
}

fn instant(id: CheckId, account: &str, finding: Finding) -> CheckResult {
    CheckResult {
        id,
        subject: subject(id, account),
        finding,
        duration_ms: 0,
    }
}

pub struct Offline<'a> {
    pub db: &'a Path,
    pub account: &'a str,
    pub server: &'a str,
    pub client_version: &'a str,
    pub now: Timestamp,
}

pub async fn offline(
    host: &Host,
    inputs: &Offline<'_>,
    filter: &DiagnosticFilter,
    limits: DiagnosticLimits,
) -> Report {
    let Offline {
        db,
        account,
        server,
        client_version,
        now,
    } = *inputs;
    let refs = HostRefs {
        agents: &*host.agents,
        supervisor: &*host.supervisor,
        toolchains: &*host.toolchains,
    };
    let db: PathBuf = db.to_path_buf();
    let mut planned = Vec::new();
    let mut skipped = Vec::new();
    for id in CheckId::server().filter(|id| filter.wants(*id)) {
        if !id.runs_offline() {
            skipped.push(instant(id, account, eval::skipped_unreachable(server)));
            continue;
        }
        let run = match id {
            CheckId::DbMigrations => {
                let db = db.clone();
                async move {
                    let state =
                        tokio::task::spawn_blocking(move || saka_store::migration_status(&db))
                            .await;
                    vec![match state {
                        Ok(Ok(state)) => eval::migrations(state),
                        Ok(Err(e)) => eval::check_failed(e.code()),
                        Err(_) => eval::check_failed("store_failed"),
                    }]
                }
                .boxed()
            }
            _ => match host_check(&refs, id, account) {
                Some(run) => run,
                None => continue,
            },
        };
        planned.push(Planned {
            id,
            subject: subject(id, account),
            run,
        });
    }
    let mut checks = run(planned, limits).await;
    checks.extend(skipped);
    Report::new(now, client_version, ReportMode::Offline, checks)
}

fn store_failed(e: &saka_store::StoreError) -> Vec<Finding> {
    vec![eval::check_failed(e.code())]
}

fn scm_finding(e: &PortError) -> Finding {
    eval::scm_failure(&e.code, &e.params)
}

fn ready(finding: Finding) -> BoxFuture<'static, Vec<Finding>> {
    async move { vec![finding] }.boxed()
}

fn status_of(e: &PortError) -> Option<u16> {
    e.params
        .get("status")
        .and_then(serde_json::Value::as_u64)
        .and_then(|s| u16::try_from(s).ok())
}

fn repo_ref(repo: &Repo) -> RepoRef {
    RepoRef {
        repo_id: repo.id,
        integration_id: repo.integration_id.clone(),
        full_name: repo.full_name.clone(),
        url: repo.url.clone(),
    }
}

const WORKFLOW_DIR: &str = ".github/workflows";

fn proposal_fix(p: &Proposal) -> Option<Fix> {
    match p.action {
        ProposalAction::None => None,
        ProposalAction::WriteFile | ProposalAction::ReplaceFile => Some(Fix {
            code: "write_version_file".into(),
            params: json!({ "path": p.path, "version": p.version }),
        }),
        ProposalAction::Manual => {
            let code = p.code?;
            let mut params = p.params.as_object().cloned().unwrap_or_default();
            match &p.command {
                Some(command) => {
                    params.insert("command".into(), json!(command));
                }
                None if params.contains_key("path") => {}
                None => return None,
            }
            if let Some(version) = &p.version {
                params.entry("version").or_insert_with(|| json!(version));
            }
            Some(Fix {
                code: code.into(),
                params: serde_json::Value::Object(params),
            })
        }
    }
}

fn host_tool(tool: VersionTool) -> Option<HostTool> {
    match tool {
        VersionTool::Rust => Some(HostTool::Rust),
        VersionTool::Python => Some(HostTool::Python),
        VersionTool::Go | VersionTool::Node | VersionTool::Pnpm => None,
    }
}

fn ignorable(e: &PortError) -> bool {
    matches!(
        e.code.as_str(),
        "repo_file_too_large" | "repo_file_not_utf8"
    )
}

impl Engine {
    async fn repo_files(&self, repo: &RepoRef, commit: &str) -> PortResult<RepoFiles> {
        let env = &*self.ports.env;
        let mut paths = env.list_repo_dir(repo, commit, "").await?;
        paths.extend(env.list_repo_dir(repo, commit, WORKFLOW_DIR).await?);
        paths.push(RECIPE_PATH.to_string());
        let mut files = RepoFiles::new();
        for path in paths {
            match versions::file_use(&path) {
                Some(FileUse::Content) => match env.read_repo_file(repo, commit, &path).await {
                    Ok(Some(text)) => files.add(&path, text.into_bytes()),
                    Ok(None) => {}
                    Err(e) if ignorable(&e) => {}
                    Err(e) => return Err(e),
                },
                Some(FileUse::Presence) => files.add_present(&path),
                None => {}
            }
        }
        Ok(files)
    }

    async fn repo_toolchains(&self, binding: &ProjectRepo, repo: &Repo) -> Vec<Finding> {
        let repo = repo_ref(repo);
        let tip = match self.ports.env.branch_tip(&repo, &binding.base_branch).await {
            Ok(Some(tip)) => tip,
            Ok(None) => return vec![eval::repo_unreachable(None)],
            Err(e) => return vec![eval::repo_unreachable(status_of(&e))],
        };
        let files = match self.repo_files(&repo, &tip).await {
            Ok(files) => files,
            Err(e) => return vec![eval::check_failed(&e.code)],
        };
        let report = versions::detect(&files, &*self.ports.releases).await;
        let mut cache = None;
        let mut tools = Vec::new();
        for t in &report.toolchains {
            if !matches!(t.status, VersionStatus::Pinned | VersionStatus::Unpinned) {
                continue;
            }
            let Some(version) = t.version.clone() else {
                tools.push(RepoToolchain {
                    tool: t.tool.as_str().into(),
                    source: t.source.map(String::from),
                    value: t.value.clone(),
                    pinned: None,
                    drift: Vec::new(),
                    cached: None,
                    host: None,
                    proposal: proposal_fix(&t.proposal),
                });
                continue;
            };
            let (cached, host) = match (t.tool.provisioned(), host_tool(t.tool)) {
                (Some(tool), _) => {
                    if cache.is_none() {
                        cache = Some(match self.ports.toolchains.cache().await {
                            Ok(c) => c.installed,
                            Err(e) => return vec![eval::check_failed(&e.code)],
                        });
                    }
                    let installed = cache
                        .iter()
                        .flatten()
                        .any(|i| i.tool == tool && i.version == version);
                    (Some(installed), None)
                }
                (None, Some(host)) => {
                    let found = match self.ports.toolchains.host_version(host).await {
                        Ok(Some(v)) => HostVersion::Found(v),
                        Ok(None) | Err(_) => HostVersion::Missing,
                    };
                    (None, Some(found))
                }
                (None, None) => (None, None),
            };
            tools.push(RepoToolchain {
                tool: t.tool.as_str().into(),
                source: t.source.map(String::from),
                value: t.value.clone(),
                pinned: Some(version),
                drift: t
                    .drift
                    .iter()
                    .map(|d| ToolchainDrift {
                        source: d.source.into(),
                        file: d.file.clone(),
                        line: d.line,
                        value: d.value.clone(),
                    })
                    .collect(),
                cached,
                host,
                proposal: None,
            });
        }
        eval::toolchain_repo(&tools)
    }

    async fn webhook_check(
        &self,
        requirements: AppRequirements,
        integration_id: &str,
        app: Shared<BoxFuture<'_, PortResult<AppStatus>>>,
    ) -> Vec<Finding> {
        let scm = &*self.ports.scm;
        match scm.webhook_configured(integration_id) {
            Ok(true) => {}
            Ok(false) => return vec![eval::webhook_not_configured()],
            Err(e) => return vec![eval::check_failed(&e.code)],
        }
        let (app, hook) = join(app, scm.webhook_status(integration_id)).await;
        let (app, hook) = match (app, hook) {
            (Ok(app), Ok(hook)) => (app, hook),
            (Err(e), _) | (_, Err(e)) => return vec![scm_finding(&e)],
        };
        match self
            .store
            .read(|c| read::last_webhook_received_at(c, integration_id))
        {
            Ok(last) => eval::webhook(&requirements, integration_id, &app, &hook, last),
            Err(e) => store_failed(&e),
        }
    }

    fn user_link_check(&self, integration_id: &str, now: Timestamp) -> Vec<Finding> {
        match self.ports.scm.device_flow_configured(integration_id) {
            Ok(true) => {}
            Ok(false) => return vec![eval::device_flow_unconfigured()],
            Err(e) => return vec![eval::check_failed(&e.code)],
        }
        let read = self.store.read(|c| {
            Ok((
                link_rows::user_link(c, OPERATOR, integration_id)?.map(|r| r.link),
                link_rows::latest_authorization(c, OPERATOR, integration_id)?,
                authorship_rows::user_mode_projects_on(c, integration_id)?,
            ))
        });
        let (link, last, projects) = match read {
            Ok(found) => found,
            Err(e) => return store_failed(&e),
        };
        let failure = last
            .filter(|a| a.state == AuthorizationState::Failed)
            .and_then(|a| a.failure)
            .and_then(|f| f.code.parse::<AuthorizationFailure>().ok());
        let names: Vec<String> = projects.into_iter().map(|p| p.name).collect();
        eval::user_link(integration_id, link.as_ref(), failure, &names, now)
    }

    fn project_config_check(&self, project: &Project) -> Vec<Finding> {
        let bound: Vec<RepoId> = project.repos.iter().map(|r| r.repo_id).collect();
        match self
            .store
            .read(|c| list::projects_bound_to(c, &bound, project.id))
        {
            Ok(others) => {
                let overlaps: Vec<ScopeOverlap> = scope_warnings(project, &others)
                    .into_iter()
                    .map(|w| ScopeOverlap {
                        repo: w.repo,
                        other_project: w.other_project,
                        other_repo: w.other_repo,
                    })
                    .collect();
                eval::project_config(project, &overlaps)
            }
            Err(e) => store_failed(&e),
        }
    }

    fn user_token_check(&self, project: &Project, now: Timestamp) -> Vec<Finding> {
        match self
            .store
            .read(|c| authorship_rows::project_authorship(c, project.id))
        {
            Ok(current) => vec![eval::user_token(
                &project.name,
                current.as_ref().and_then(|a| a.token.as_ref()),
                now,
            )],
            Err(e) => store_failed(&e),
        }
    }

    fn diagnosed_workspace(&self, filter: &DiagnosticFilter) -> Result<(Vec<Project>, Vec<Repo>)> {
        let projects = self.store.read(|c| {
            let ids = match filter.project_id {
                Some(id) => vec![id],
                None => list::project_ids(c)?,
            };
            let mut out = Vec::with_capacity(ids.len());
            for id in ids {
                out.extend(read::load_project(c, id)?);
            }
            Ok(out)
        })?;
        let mut ids: Vec<RepoId> = Vec::new();
        for binding in projects.iter().flat_map(|p| &p.repos) {
            if !ids.contains(&binding.repo_id) {
                ids.push(binding.repo_id);
            }
        }
        let repos = self.store.read(|c| list::repos(c, &ids))?;
        Ok((projects, repos))
    }

    fn scm_and_project_checks<'a>(
        &'a self,
        filter: &DiagnosticFilter,
        projects: &'a [Project],
        repos: &'a [Repo],
        now: Timestamp,
    ) -> Vec<Planned<'a>> {
        let scm = &*self.ports.scm;
        let requirements = |id: &str| scm.capabilities(id).and_then(|c| c.diagnostics);
        let mut planned = Vec::new();
        for integration in &self.cfg.integrations {
            let id = integration.id.as_str();
            let supported = requirements(id);
            let app = scm.app_status(id).boxed().shared();
            for check in [CheckId::ScmApp, CheckId::ScmWebhook, CheckId::ScmUserLink] {
                if !filter.wants(check) {
                    continue;
                }
                let run = match supported {
                    None => ready(eval::unsupported_by_adapter()),
                    Some(req) => match check {
                        CheckId::ScmApp => {
                            let app = app.clone();
                            async move {
                                vec![match app.await {
                                    Ok(a) => eval::app_ok(&a),
                                    Err(e) => scm_finding(&e),
                                }]
                            }
                            .boxed()
                        }
                        CheckId::ScmWebhook => self.webhook_check(req, id, app.clone()).boxed(),
                        _ => async move { self.user_link_check(id, now) }.boxed(),
                    },
                };
                planned.push(Planned {
                    id: check,
                    subject: Subject::integration(id),
                    run,
                });
            }
            if !filter.wants(CheckId::ScmInstallation) {
                continue;
            }
            for repo in repos.iter().filter(|r| r.integration_id == id) {
                let run = match supported {
                    None => ready(eval::unsupported_by_adapter()),
                    Some(req) => {
                        let app = app.clone();
                        let web_url = integration.web_url.as_str();
                        async move {
                            let r = repo_ref(repo);
                            match join(app, scm.app_installation(&r)).await {
                                (Ok(a), Ok(found)) => vec![eval::installation(
                                    &req,
                                    &repo.full_name,
                                    web_url,
                                    &a.slug,
                                    &found,
                                )],
                                (Err(e), _) | (_, Err(e)) => vec![scm_finding(&e)],
                            }
                        }
                        .boxed()
                    }
                };
                planned.push(Planned {
                    id: CheckId::ScmInstallation,
                    subject: Subject::repo(&repo.id.0.to_string(), &repo.full_name),
                    run,
                });
            }
        }
        if filter.wants(CheckId::ScmInstallation) {
            for repo in repos.iter().filter(|r| {
                !self
                    .cfg
                    .integrations
                    .iter()
                    .any(|i| i.id == r.integration_id)
            }) {
                planned.push(Planned {
                    id: CheckId::ScmInstallation,
                    subject: Subject::repo(&repo.id.0.to_string(), &repo.full_name),
                    run: ready(eval::unsupported_by_adapter()),
                });
            }
        }
        for project in projects {
            let pid = project.id.0.to_string();
            if filter.wants(CheckId::ProjectUserToken) {
                planned.push(Planned {
                    id: CheckId::ProjectUserToken,
                    subject: Subject::project(&pid, &project.name),
                    run: async move { self.user_token_check(project, now) }.boxed(),
                });
            }
            if filter.wants(CheckId::ProjectConfig) {
                planned.push(Planned {
                    id: CheckId::ProjectConfig,
                    subject: Subject::project(&pid, &project.name),
                    run: async move { self.project_config_check(project) }.boxed(),
                });
            }
            if !filter.wants(CheckId::ToolchainRepo) {
                continue;
            }
            for binding in &project.repos {
                let subject = Subject::project_repo(&pid, &project.name, &binding.name);
                let run = match repos.iter().find(|r| r.id == binding.repo_id) {
                    Some(repo) => self.repo_toolchains(binding, repo).boxed(),
                    None => ready(eval::check_failed("repo_not_found")),
                };
                planned.push(Planned {
                    id: CheckId::ToolchainRepo,
                    subject,
                    run,
                });
            }
        }
        planned
    }
}

impl Engine {
    pub async fn diagnostics(
        &self,
        filter: &DiagnosticFilter,
        server_version: &str,
    ) -> Result<Report> {
        if let Some(project) = filter.project_id
            && self.project(project)?.is_none()
        {
            return Err(EngineError::Invalid {
                field: "project_id",
                code: "unknown".into(),
            });
        }
        let started = Instant::now();
        let now = self.now();
        let account = self.cfg.loop_cfg.account.as_str();
        let refs = HostRefs {
            agents: &*self.ports.agents,
            supervisor: &*self.ports.supervisor,
            toolchains: &*self.ports.toolchains,
        };
        let (projects, repos) = self.diagnosed_workspace(filter)?;
        let mut planned = Vec::new();
        for id in CheckId::server().filter(|id| filter.wants(*id)) {
            let run = match id {
                CheckId::ServerReady => {
                    async move { vec![eval::server_ready(&self.readiness().failing())] }.boxed()
                }
                CheckId::DbMigrations => async move {
                    one(self
                        .store
                        .schema_version()
                        .map(|applied| {
                            eval::migrations(MigrationState {
                                applied: applied.unwrap_or(0),
                                latest: saka_store::latest_migration(),
                            })
                        })
                        .map_err(|e| PortError::permanent(e.code(), serde_json::json!({}))))
                }
                .boxed(),
                CheckId::AgentQuota => async move {
                    one(self
                        .store
                        .read(read::account_quotas)
                        .map(|quotas| {
                            eval::agent_quota(
                                account,
                                &quotas,
                                self.cfg.loop_cfg.rate_limit_pause,
                                now,
                            )
                        })
                        .map_err(|e| PortError::permanent(e.code(), serde_json::json!({}))))
                }
                .boxed(),
                CheckId::ToolchainSources => sources(&*self.ports.releases).boxed(),
                _ => match host_check(&refs, id, account) {
                    Some(run) => run,
                    None => continue,
                },
            };
            planned.push(Planned {
                id,
                subject: subject(id, account),
                run,
            });
        }
        planned.extend(self.scm_and_project_checks(filter, &projects, &repos, now));
        let checks = run(planned, self.cfg.diagnostics).await;
        let report = Report::new(now, server_version, ReportMode::Online, checks);
        for c in &report.checks {
            self.counters
                .diagnostic_check(c.id.as_str(), c.finding.status.as_str());
            if c.finding.status == DiagnosticStatus::Fail {
                tracing::warn!(id = c.id.as_str(), subject = %c.subject.id, code = %c.finding.code, "diagnostics.check_failed");
            }
        }
        let s = report.summary;
        tracing::info!(
            pass = s.pass,
            warn = s.warn,
            fail = s.fail,
            skip = s.skip,
            duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "diagnostics.ran"
        );
        Ok(report)
    }
}
