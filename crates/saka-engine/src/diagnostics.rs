use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::{BoxFuture, FutureExt, join_all};
use futures_util::stream::{self, StreamExt};
use saka_core::diagnostics::{
    self as eval, CheckResult, Finding, MigrationState, Report, SourceFailure, Subject,
};
use saka_core::ids::ProjectId;
use saka_core::time::Timestamp;
use saka_core::vocab::{CheckId, DiagnosticStatus, ReportMode, SubjectKind, Tool};
use saka_ports::{Agents, PortError, PortResult, ReleaseIndex, Supervisor, Toolchains};
use saka_store::read;
use tokio::time::Instant;

use crate::{Engine, EngineError, Result};

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
