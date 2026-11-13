mod accounts;
mod action;
mod authorship;
mod chat;
mod commands;
mod config_check;
mod context;
mod decisions;
mod deploy;
mod deploy_run;
pub mod diagnostics;
mod docs;
mod draft;
mod fix_packet;
mod hooks;
mod hooks_run;
mod identity;
mod inbox;
mod incident;
mod incident_actions;
mod incident_ingest;
mod intake;
mod issue_intake;
mod issue_scan;
mod metrics;
pub mod packet;
mod permission_check;
pub mod project_config;
mod quick;
pub mod recipe;
mod reconciler;
mod redo;
mod register;
mod reload;
mod revert;
pub mod review_packet;
mod review_run;
mod runner;
mod secrets;
mod text;
mod timeline;
pub mod toolchain;
pub mod versions;
mod views;
pub mod webhooks;
mod worker;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use saka_core::Ctx;
use saka_core::authorship::GitIdentity;
use saka_core::event::{Event, Prompt};
use saka_core::ids::{ProjectId, WorkItemId};
use saka_core::job::Job;
use saka_core::model::Task;
use saka_core::policy::Policy;
use saka_core::react::react;
use saka_core::rejection::Rejection;
use saka_core::task::{self, LoopConfig, TaskInput};
use saka_core::time::Timestamp;
use saka_ports::{
    Agents, Clock, DocSource, Environments, Ids, IdsSource, PortError, ReleaseIndex, Scm,
    SecretStore, Supervisor, Toolchains,
};
use saka_store::read;
use saka_store::{Batch, EventRecord, JobOutcome, NewJob, Store, StoreError};
use tokio::sync::{Notify, watch};
use uuid::Uuid;

pub use accounts::{AuthorizationView, LinkCredential};
pub use action::{Action, Produced, Reply, Respond, StoredAction};
pub use authorship::{AuthorPreview, AuthorshipView, IntegrationLink, UserTokenView};
pub use commands::{NewBinding, NewProject, NewRepo, RegisteredSpec, SessionCommand};
pub use config_check::{
    CheckConfig, CheckedRepo, CheckedWorkspace, ConfigCheckView, ConfigProblem, RegisteredProject,
    RemoteConfig,
};
pub use deploy::{DeploymentDetail, DeploymentRow, EnvironmentView, RepoInfo, VerificationView};
pub use diagnostics::{DiagnosticFilter, DiagnosticLimits};
pub use docs::{DocContentView, DocTreeView, DocWriteView, Precondition, SpecProblem};
pub use draft::{
    DraftBudgetView, DraftChange, DraftChangesView, DraftContentView, DraftPacketDocument,
    DraftPacketView, DraftView, NewSpecDraft,
};
pub use hooks::HookReport;
pub use identity::{DeviceIssued, PairingIssued, Principal, Registered, SessionIssued, digest};
pub use incident::{IncidentDetail, IncidentQuery, IncidentRow, SourceView, Suspect};
pub use incident_ingest::{IncidentReceipt, ReceiptOutcome};
pub use issue_intake::{IssueDraftView, IssueIntakeView, IssueRepoView, IssueSpecView};
pub use metrics::{MetricsSnapshot, Readiness, SecondsHistogram};
pub use permission_check::{PREVIEW_SCAN, PreviewChange, PreviewView};
pub use quick::{QuickRequest, QuickView};
pub use register::{AddProject, Integration, MAX_CONFIG_BYTES, branch_ok, parse_repo_url};
pub use reload::ReloadView;
pub use saka_ports::{RunnerEvent, RunnerEventKind};
pub use saka_store::StoredEvent;
pub use saka_store::hook_run::HookRunFilter;
pub use saka_store::inbox::{InboxFilter, InboxItem, InboxSummary};
pub use saka_store::list::{
    AnswerKind, DecisionFilter, Key, Page, PermissionFilter, WorkItemFilter,
};
pub use saka_store::read::{RunOf, TimelineCursor, TimelinePage};
pub use secrets::SecretInfo;
pub use views::{
    AccountView, ActiveSessionView, BudgetView, DecisionView, DependencyView, Paged,
    PermissionView, ProjectView, RevisionInfo, RevisionRef, ScopeWarning, SessionView, SpecView,
    StepItem, TaskDetail, TaskView, UsageView,
};
pub use worker::{JobReport, JobResult};

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub loop_cfg: LoopConfig,
    pub max_active_tasks: usize,
    pub policy: Policy,
    pub worker_id: String,
    pub integrations: Vec<Integration>,
    pub env: EnvConfig,
    pub git_identity: GitIdentity,
    pub draft: saka_core::draft::DraftConfig,
    pub architect_model: Option<String>,
    pub config_poll_secs: u32,
    pub diagnostics: DiagnosticLimits,
    pub hooks: Vec<saka_core::hook::Hook>,
    pub environments: Vec<saka_core::environment::Environment>,
    pub incident_sources: Vec<saka_core::incident::Source>,
    pub auth_enabled: bool,
    pub workflow_policy: saka_core::workflow::WorkflowPolicy,
    pub public_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvConfig {
    pub port_first: u16,
    pub port_last: u16,
    pub ports_per_env: u16,
}

impl EnvConfig {
    pub fn port_blocks(&self) -> usize {
        let span = usize::from(self.port_last.saturating_sub(self.port_first)) + 1;
        span / usize::from(self.ports_per_env.max(1))
    }

    pub fn block_start(&self, index: usize) -> Option<u16> {
        (index < self.port_blocks())
            .then(|| u32::from(self.port_first) + index as u32 * u32::from(self.ports_per_env))
            .and_then(|start| u16::try_from(start).ok())
    }
}

#[derive(Clone)]
pub struct Ports {
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn Ids>,
    pub entropy: Arc<dyn saka_ports::Entropy>,
    pub env: Arc<dyn Environments>,
    pub scm: Arc<dyn Scm>,
    pub intake: Arc<dyn saka_ports::IntakeSource>,
    pub agents: Arc<dyn Agents>,
    pub docs: Arc<dyn DocSource>,
    pub toolchains: Arc<dyn Toolchains>,
    pub secrets: Arc<dyn SecretStore>,
    pub supervisor: Arc<dyn Supervisor>,
    pub releases: Arc<dyn ReleaseIndex>,
    pub webhooks: Arc<dyn saka_ports::Webhooks>,
    pub notifier: Arc<dyn saka_ports::Notifier>,
    pub host: Arc<dyn saka_ports::HostCommands>,
    pub signals: Arc<dyn saka_ports::Signals>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Rejected(#[from] Rejection),
    #[error("{field}: {code}")]
    Invalid { field: &'static str, code: String },
    #[error("{field}: {code}")]
    InvalidItem { field: String, code: String },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("port: {}", .0.code)]
    Port(#[from] PortError),
    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cause {
    pub actor: String,
    pub causation_id: Option<Uuid>,
    pub correlation_id: Uuid,
}

pub const OPERATOR: &str = "operator:local";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Command,
    Fact,
}

#[derive(Default)]
struct Extra<'a> {
    outcome: Option<JobOutcome>,
    events: Vec<Event>,
    jobs: Vec<Job>,
    action: Option<(&'a Action<'a>, Shows)>,
    activity: Option<saka_core::model::SessionActivity>,
    offset: Option<(saka_core::ids::SessionId, u64)>,
}

#[derive(Debug, Clone, Copy)]
enum Shows {
    Task,
    Taken(bool),
    Draft,
    Permission(saka_core::ids::PermissionRequestId),
    Decision(saka_core::ids::DecisionRequestId),
    Session(saka_core::ids::SessionId),
}

pub struct Engine {
    store: Arc<Store>,
    ports: Ports,
    cfg: EngineConfig,
    lock: Mutex<()>,
    changed: Notify,
    counters: metrics::Counters,
    boot: Uuid,
    ports_in_flight: Mutex<std::collections::BTreeMap<saka_core::ids::EnvironmentId, u16>>,
    accounts: accounts::Accounts,
    path_filter: intake::PathFilterCache,
    incident_limits: Mutex<std::collections::BTreeMap<String, (i64, u32)>>,
}

impl Engine {
    pub fn new(store: Arc<Store>, ports: Ports, cfg: EngineConfig) -> Engine {
        let boot = ports.ids.new_id();
        Engine {
            boot,
            store,
            ports,
            cfg,
            lock: Mutex::new(()),
            changed: Notify::new(),
            counters: metrics::Counters::default(),
            ports_in_flight: Mutex::new(std::collections::BTreeMap::new()),
            accounts: accounts::Accounts::default(),
            path_filter: intake::PathFilterCache::default(),
            incident_limits: Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn config(&self) -> &EngineConfig {
        &self.cfg
    }

    pub fn now(&self) -> Timestamp {
        self.ports.clock.now()
    }

    fn new_id(&self) -> Uuid {
        self.ports.ids.new_id()
    }

    pub fn cause(&self, actor: &str) -> Cause {
        Cause {
            actor: actor.to_string(),
            causation_id: None,
            correlation_id: self.new_id(),
        }
    }

    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    fn locked(&self) -> Result<MutexGuard<'_, ()>> {
        self.lock
            .lock()
            .map_err(|_| EngineError::Internal("engine lock poisoned".into()))
    }

    fn load_task(&self, id: WorkItemId) -> Result<Task> {
        self.store
            .read(|c| read::load_task(c, id, &self.cfg.workflow_policy))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))
    }

    fn decide(
        &self,
        id: WorkItemId,
        input: Option<TaskInput>,
        mode: Mode,
        cause: &Cause,
        extra: Extra<'_>,
    ) -> Result<Vec<Event>> {
        let _guard = self.locked()?;
        let now = self.now();
        let task = self.load_task(id)?;
        let events = match input {
            Some(input) => {
                let mut ids = IdsSource(&*self.ports.ids);
                let mut ctx = Ctx { now, ids: &mut ids };
                match task::decide(&task, input, &self.cfg.loop_cfg, &mut ctx) {
                    Ok(events) => events,
                    Err(rejection) if mode == Mode::Fact => {
                        tracing::debug!(work_item_id = %id, code = rejection.code(), "fact no longer applies");
                        Vec::new()
                    }
                    Err(rejection) => return Err(rejection.into()),
                }
            }
            None => Vec::new(),
        };
        let mut batch = Batch::new(now);
        batch.job_outcome = extra.outcome;
        batch
            .activities
            .extend(extra.activity.map(|a| (task.item.project_id, a)));
        batch.session_offsets.extend(extra.offset);
        let project = Some(task.item.project_id);
        let rounds_before = task.fix_rounds_used();
        let after = self.apply(&mut batch, task, &events, project, cause, now)?;
        let origin_after = self.finish_revert(&mut batch, &after, &events, project, cause, now)?;
        let revert_outcome = after.revert.as_ref().and_then(|_| revert_outcome(&events));
        for e in &extra.events {
            self.push_event(&mut batch, e, project, cause, now);
        }
        for job in extra.jobs {
            self.push_job(&mut batch, job, project, cause);
        }
        if let Some((action, shows)) = extra.action {
            let reply = match shows {
                Shows::Task | Shows::Draft => (action.respond)(Produced::Task(&after)),
                Shows::Taken(created) => (action.respond)(Produced::Taken(&after, created)),
                Shows::Permission(pid) => {
                    let request = after
                        .permissions
                        .iter()
                        .find(|p| p.id == pid)
                        .cloned()
                        .ok_or_else(|| {
                            EngineError::Internal("answered request not in the task".into())
                        })?;
                    let view = PermissionView {
                        request,
                        project_id: after.item.project_id,
                    };
                    (action.respond)(Produced::Permission(&view))
                }
                Shows::Decision(did) => {
                    let request = after
                        .decisions
                        .iter()
                        .find(|d| d.id == did)
                        .cloned()
                        .ok_or_else(|| {
                            EngineError::Internal("answered question not in the task".into())
                        })?;
                    let view = DecisionView {
                        request,
                        project_id: after.item.project_id,
                    };
                    (action.respond)(Produced::Decision(&view))
                }
                Shows::Session(sid) => {
                    let session = after
                        .session
                        .iter()
                        .chain(after.retired_sessions.iter())
                        .find(|s| s.id == sid)
                        .ok_or_else(|| EngineError::Internal("session not in the task".into()))?;
                    (action.respond)(Produced::Session(session))
                }
            };
            batch.action = Some(action.record(self, cause, project, Some(id.0), reply));
        }
        let verification = metrics::dod_verification(&events, &after);
        let docs_number = after.docs_change.as_ref().map(|c| c.number);
        if !events.is_empty() {
            batch.tasks.push(after);
        }
        batch.tasks.extend(origin_after);
        self.commit(&batch)?;
        if let Some(outcome) = revert_outcome {
            self.counters.revert(outcome);
            tracing::info!(work_item_id = %id, outcome, "delivery.revert.ended");
        }
        for e in &events {
            match e {
                Event::ReviewRecorded {
                    work_item_id,
                    outcome,
                    basis,
                    findings,
                    ..
                } => {
                    self.counters
                        .review_verdict(outcome.as_str(), basis.as_str());
                    tracing::info!(%work_item_id, outcome = outcome.as_str(), basis = basis.as_str(), findings = findings.len(), "review.verdict");
                }
                Event::StageRunCreated {
                    work_item_id,
                    trigger,
                    fix: Some(round),
                    ..
                } if round.number > rounds_before => {
                    let cause = match (trigger, round.cause.code.as_str()) {
                        (saka_core::vocab::Trigger::Manual, _) => "operator",
                        (_, "merge_conflict") => "conflict",
                        _ => "review",
                    };
                    self.counters.fix_round(cause);
                    tracing::info!(%work_item_id, round = round.number, cause, "fix.round");
                }
                Event::CiRerunRequested {
                    work_item_id,
                    head_sha,
                    number,
                    manual,
                    ..
                } => {
                    tracing::info!(%work_item_id, head_sha, number, manual, "delivery.gate.rerun_requested");
                }
                Event::CiRerunSettled {
                    work_item_id,
                    head_sha,
                    number,
                    rerun,
                    unsupported,
                    ..
                } => {
                    if rerun.is_empty() {
                        self.counters.ci_rerun("unsupported");
                    }
                    tracing::info!(%work_item_id, head_sha, number, rerun = rerun.len(), unsupported = unsupported.len(), "delivery.gate.rerun_settled");
                }
                Event::GateObserved {
                    work_item_id,
                    passed_after_rerun: Some(rerun),
                    ..
                } => {
                    self.counters.ci_rerun("passed");
                    tracing::info!(%work_item_id, reruns = rerun.reruns, "delivery.gate.passed_after_rerun");
                }
                Event::WorkItemBlocked {
                    work_item_id,
                    code: saka_core::vocab::BlockerCode::CiRed,
                    params,
                    ..
                } if params.get("source").and_then(|s| s.as_str()) == Some("scm")
                    && params.get("reruns").and_then(|n| n.as_u64()).unwrap_or(0) > 0
                    && params.get("rerun_unsupported").is_none() =>
                {
                    self.counters.ci_rerun("failed");
                    tracing::info!(%work_item_id, "delivery.gate.failed_after_rerun");
                }
                Event::WorkItemBlocked {
                    work_item_id,
                    code: saka_core::vocab::BlockerCode::ReviewRoundsExhausted,
                    ..
                } => {
                    self.counters.fix_exhausted();
                    tracing::info!(%work_item_id, "fix.exhausted");
                }
                Event::ReviewPosted {
                    work_item_id,
                    event,
                    ..
                } => {
                    self.counters.review_post(event.as_str());
                    tracing::info!(%work_item_id, event = event.as_str(), "review.post");
                }
                Event::SessionTurnRequested {
                    prompt: Prompt::VerdictFormat { reason },
                    ..
                } => {
                    self.counters.review_format_rejected(reason);
                    tracing::info!(code = %reason, "review.format_rejected");
                }
                Event::MergeRequested {
                    work_item_id,
                    reason,
                    ..
                } => {
                    let by = match reason.as_deref() {
                        Some(saka_core::task::AUTONOMY_REASON) => "autonomy",
                        Some(saka_core::task::QUEUE_REASON) => "queue",
                        _ => "operator",
                    };
                    self.counters.merge_request(by);
                    tracing::info!(%work_item_id, by, "delivery.merge.requested");
                }
                Event::ChangeRequestUpdateRequested {
                    work_item_id,
                    expected_head_sha,
                    ..
                } => {
                    tracing::info!(%work_item_id, expected_head_sha, "delivery.change_request.update_requested");
                }
                Event::ChangeRequestBranchUpdated {
                    work_item_id,
                    head_sha,
                    ..
                } => {
                    tracing::info!(%work_item_id, head_sha, "delivery.change_request.branch_updated");
                }
                Event::MergeDequeued {
                    work_item_id,
                    reason,
                } => {
                    self.counters.merge_dequeue(reason);
                    tracing::info!(%work_item_id, reason, "delivery.merge.dequeued");
                }
                Event::ChangeRequestOpened {
                    work_item_id,
                    number,
                    opened_with,
                    ..
                } => {
                    self.counters.change_request_opened(opened_with.as_str());
                    tracing::info!(%work_item_id, number, credential = opened_with.as_str(), "scm.change_request.opened");
                }
                Event::ChangeRequestMerged {
                    work_item_id,
                    role: saka_core::vocab::ChangeRole::Docs,
                    merge_commit_sha,
                    merged_by_saka,
                    ..
                } => {
                    self.counters.docs_change("merged");
                    tracing::info!(%work_item_id, number = docs_number, merge_commit_sha, merged_by_saka, "delivery.docs_change.merged");
                }
                Event::DocsChangeSkipped {
                    work_item_id,
                    number,
                    reason,
                    ..
                } => {
                    self.counters.docs_change("skipped");
                    tracing::info!(%work_item_id, number, reason, "delivery.docs_change.skipped");
                }
                _ => {}
            }
        }
        match verification {
            Some(metrics::DodVerification::Outcome(outcome)) => self.counters.dod(outcome),
            Some(metrics::DodVerification::Started { head_sha, sources }) => {
                tracing::info!(work_item_id = %id, head_sha, sources = ?sources, "delivery.dod.verify");
            }
            None => {}
        }
        Ok(events)
    }

    fn apply(
        &self,
        batch: &mut Batch,
        task: Task,
        events: &[Event],
        project: Option<ProjectId>,
        cause: &Cause,
        now: Timestamp,
    ) -> Result<Task> {
        let mut after = task;
        for e in events {
            after = task::evolve(Some(after), e, now).ok_or_else(|| {
                EngineError::Internal(format!("evolve dropped the task on {}", e.kind()))
            })?;
            for job in react(&after, e, &self.cfg.loop_cfg, now) {
                self.push_job(batch, job, project, cause);
            }
            log_runtime(e, &cause.actor);
            let event_id = self.push_event(batch, e, project, cause, now);
            self.plan_task_hooks(batch, &after, e, event_id, cause, now)?;
            self.plan_deployments(batch, &mut after, e, cause, now)?;
        }
        Ok(after)
    }

    fn push_event(
        &self,
        batch: &mut Batch,
        e: &Event,
        project: Option<ProjectId>,
        cause: &Cause,
        now: Timestamp,
    ) -> Uuid {
        let stream = e.stream();
        let id = self.new_id();
        batch.events.push(EventRecord {
            id,
            kind: e.kind(),
            version: Event::VERSION,
            stream_type: stream.kind().to_string(),
            stream_id: stream.id(),
            project_id: project,
            actor: cause.actor.clone(),
            causation_id: cause.causation_id,
            correlation_id: cause.correlation_id,
            occurred_at: now,
            payload: e.payload(),
        });
        id
    }

    fn push_job(&self, batch: &mut Batch, job: Job, project: Option<ProjectId>, cause: &Cause) {
        batch.jobs.push(NewJob {
            id: self.new_id(),
            job,
            project_id: project,
            causation_id: cause.causation_id,
            correlation_id: cause.correlation_id,
        });
    }

    fn commit(&self, batch: &Batch) -> Result<Vec<i64>> {
        let inbox = self.inbox_syncs(batch);
        let seqs = self.store.commit_synced(batch, &inbox, &|opened| {
            self.plan_inbox_hooks(opened, batch.now)
        })?;
        if !batch.events.is_empty()
            || !batch.jobs.is_empty()
            || batch.action.is_some()
            || !batch.activities.is_empty()
        {
            self.changed.notify_waiters();
        }
        Ok(seqs)
    }

    pub async fn run(self: Arc<Self>, workers: usize, mut shutdown: watch::Receiver<bool>) {
        let mut tasks = tokio::task::JoinSet::new();
        self.counters
            .worker_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        for _ in 0..workers.max(1) {
            let engine = Arc::clone(&self);
            let shutdown = shutdown.clone();
            tasks.spawn(async move { engine.worker_loop(shutdown).await });
        }
        self.counters
            .reconciler_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        loop {
            if let Err(e) = self.reconcile() {
                tracing::error!(error = %e, "reconcile failed");
            }
            tokio::select! {
                _ = tokio::time::sleep(RECONCILE_INTERVAL) => {}
                _ = self.changed() => {}
                _ = shutdown.changed() => {}
            }
            if *shutdown.borrow() {
                break;
            }
        }
        tasks.join_all().await;
    }

    async fn worker_loop(&self, mut shutdown: watch::Receiver<bool>) {
        while !*shutdown.borrow() {
            match self.run_next_job().await {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(e) => tracing::error!(error = %e, "job run failed"),
            }
            tokio::select! {
                _ = tokio::time::sleep(IDLE_POLL) => {}
                _ = self.changed() => {}
                _ = shutdown.changed() => {}
            }
        }
    }
}

fn revert_outcome(events: &[Event]) -> Option<&'static str> {
    events.iter().find_map(|e| match e {
        Event::WorkItemDone { .. } => Some("merged"),
        Event::WorkItemCancelled { .. } => Some("cancelled"),
        Event::WorkItemBlocked {
            code: saka_core::vocab::BlockerCode::RevertConflict,
            ..
        } => Some("conflict"),
        _ => None,
    })
}

pub(crate) fn log_runtime(e: &Event, actor: &str) {
    use Event as E;
    match e {
        E::SessionStarted {
            owner,
            session_id,
            resumed_from_session_id: Some(from),
            resume_cause,
            ..
        } => {
            let run_id = owner.run_id();
            tracing::info!(%run_id, %session_id, resumed_from = %from, cause = ?resume_cause, "session resumed")
        }
        E::SessionReattached {
            session_id,
            runner_version,
            from_offset,
            ..
        } => tracing::info!(%session_id, %runner_version, from_offset, "session reattached"),
        E::SessionSteered {
            session_id,
            turn,
            n,
            ..
        } => {
            tracing::info!(%session_id, turn, n, %actor, "session steered");
        }
        E::SessionInterruptRequested {
            session_id,
            actor: by,
            ..
        } => tracing::info!(%session_id, by = %by, %actor, "session interrupted"),
        E::SessionStopRequested { session_id, .. } => {
            tracing::info!(%session_id, %actor, "session stopped");
        }
        E::PermissionAnswered {
            permission_request_id,
            state: saka_core::vocab::PermissionState::Expired,
            answer_source,
            ..
        } if answer_source == saka_core::session::WAIT_LIMIT_SOURCE => {
            tracing::info!(%permission_request_id, "permission request expired");
        }
        E::PermissionRuleAdded { rule_id, tool, .. } => {
            tracing::info!(%rule_id, %tool, %actor, "permission rule added");
        }
        E::PermissionRuleRevoked { rule_id, .. } => {
            tracing::info!(%rule_id, %actor, "permission rule revoked");
        }
        E::BudgetWarned {
            owner,
            dimension,
            used,
            limit,
        } => {
            let run_id = owner.run_id();
            tracing::info!(%run_id, %dimension, %used, %limit, "budget warning");
        }
        E::WorkItemBlocked {
            work_item_id,
            code: saka_core::vocab::BlockerCode::BudgetExhausted,
            params,
            ..
        } => {
            tracing::info!(
                %work_item_id,
                dimension = params["dimension"].as_str().unwrap_or_default(),
                used = %params["used"],
                limit = %params["limit"],
                "budget exhausted"
            )
        }
        _ => {}
    }
}

pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
const IDLE_POLL: Duration = Duration::from_millis(500);
