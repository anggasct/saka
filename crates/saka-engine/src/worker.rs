mod changes;
mod environments;
mod merging;
mod sessions;
mod verification;

use saka_core::authorship::UserLink;
use saka_core::draft::DraftInput;
use saka_core::event::Event;
use saka_core::ids::{EnvironmentId, RepoId, SessionId, StageRunId};
use saka_core::job::{Job, JobKind};
use saka_core::model::{
    Coded, DocsCheckout, PermissionRequest, Project, ProjectRepo, RunOwner, Session, Task,
};
use saka_core::scope::{PathScope, ScopeViolation};
use saka_core::session::SessionInput;
use saka_core::task::TaskInput;
use saka_core::time::Timestamp;
use saka_core::vocab::{
    BlockerCode, ChangeCredentialKind, DocsMode, PermissionState, ScopeStage, Stage, StagePhase,
    WorkItemOrigin,
};
use saka_ports::BestEffort;
use saka_ports::{
    ChangeCredential, DocsRequest, ExtraDir, PortError, PortResult, RepoRef, SessionRef,
};
use saka_store::{ClaimedJob, JobOutcome, LEASE_EXTEND_MS, StoreError, read};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tracing::Instrument;

use crate::{Cause, Engine, EngineError, Extra, Mode, Result};

pub const VERIFY_ROUNDS: u32 = 3;
const DOCS_TITLE_MAX: usize = 256;

pub(crate) fn docs_branch(branch: &str) -> String {
    format!("{branch}-docs")
}

fn extra_dirs(docs: Option<&DocsCheckout>) -> Vec<ExtraDir> {
    docs.iter()
        .map(|d| ExtraDir {
            path: d.path.clone(),
            writable: d.mode == DocsMode::Writable,
        })
        .collect()
}
pub const VERIFY_PAUSE_MS: i64 = 30_000;

#[derive(Debug, Clone, PartialEq)]
pub struct JobReport {
    pub kind: JobKind,
    pub idempotency_key: String,
    pub result: JobResult,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobResult {
    Succeeded,
    Skipped,
    Retrying { code: String },
    Exhausted { code: String },
    LeaseLost,
}

#[derive(Debug)]
pub enum Effect {
    Fact(TaskInput),
    Draft {
        input: DraftInput,
        events: Vec<Event>,
    },
    Nothing,
    Skipped,
    Later(Job),
    Hook(crate::hooks::HookReport),
}

pub type Execution = std::result::Result<Effect, PortError>;

enum Input {
    Task(TaskInput),
    Draft(DraftInput),
}

pub(crate) enum OpenedBy {
    App,
    Link(UserLink),
    Token { set_at: Timestamp, login: String },
}

pub(crate) struct Opener {
    credential: ChangeCredential,
    by: OpenedBy,
}

impl OpenedBy {
    fn kind(&self) -> ChangeCredentialKind {
        match self {
            OpenedBy::App => ChangeCredentialKind::App,
            OpenedBy::Link(_) => ChangeCredentialKind::UserLink,
            OpenedBy::Token { .. } => ChangeCredentialKind::UserToken,
        }
    }

    fn login(&self) -> String {
        match self {
            OpenedBy::App => String::new(),
            OpenedBy::Link(link) => link.login.clone(),
            OpenedBy::Token { login, .. } => login.clone(),
        }
    }
}

pub(crate) enum Refusal {
    Block(BlockerCode, Value),
    Fail(PortError),
}

impl Refusal {
    pub(crate) fn into_error(self) -> PortError {
        match self {
            Refusal::Block(code, params) => PortError::permanent(code.as_str(), params),
            Refusal::Fail(e) => e,
        }
    }
}

const CHECK_NAME_MAX: usize = 120;

fn check_name(command: &str) -> String {
    if command.chars().count() <= CHECK_NAME_MAX {
        return command.to_string();
    }
    let cut: String = command.chars().take(CHECK_NAME_MAX - 1).collect();
    format!("{cut}…")
}

pub(crate) fn field<T: DeserializeOwned>(
    payload: &Value,
    name: &str,
) -> std::result::Result<T, PortError> {
    payload
        .get(name)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| PortError::permanent("job_payload_invalid", json!({ "field": name })))
}

fn owner_run(payload: &Value) -> PortResult<StageRunId> {
    saka_core::job::owner_of(payload)
        .and_then(|owner| owner.stage_run_id())
        .ok_or_else(|| PortError::permanent("job_payload_invalid", json!({ "field": "owner" })))
}

pub(crate) fn internal(e: EngineError) -> PortError {
    PortError::retryable("internal", json!({ "error": e.to_string() }))
}

pub(crate) enum SessionJob {
    Input(Box<SessionInput>),
    Done,
    Skipped,
}

impl SessionJob {
    pub(crate) fn effect(self, wrap: impl FnOnce(SessionInput) -> Effect) -> Effect {
        match self {
            SessionJob::Input(input) => wrap(*input),
            SessionJob::Done => Effect::Nothing,
            SessionJob::Skipped => Effect::Skipped,
        }
    }
}

pub(crate) fn session_ref(session: Option<&Session>, id: SessionId) -> Option<SessionRef> {
    session
        .filter(|s| s.id == id && s.state.is_live())
        .map(|s| SessionRef {
            session_id: s.id,
            runner_unit: s.runner_unit.clone(),
            socket_path: s.socket_path.clone(),
        })
}

fn stopping_ref(session: Option<&Session>, id: SessionId) -> Option<SessionRef> {
    session_ref(session, id).or_else(|| {
        session
            .filter(|s| {
                s.id == id && s.interrupt_requested == Some(saka_core::vocab::InterruptBy::Saka)
            })
            .map(|s| SessionRef {
                session_id: s.id,
                runner_unit: s.runner_unit.clone(),
                socket_path: s.socket_path.clone(),
            })
    })
}

impl Engine {
    pub(crate) async fn session_job(
        &self,
        kind: JobKind,
        p: &Value,
        session: Option<&Session>,
        permissions: &[PermissionRequest],
    ) -> std::result::Result<SessionJob, PortError> {
        match kind {
            JobKind::SessionDeliverPermissionAnswer => {
                let id: saka_core::ids::PermissionRequestId = field(p, "permission_request_id")?;
                let Some(request) = permissions.iter().find(|r| {
                    r.id == id && r.delivered_at.is_none() && saka_core::session::awaits_delivery(r)
                }) else {
                    return Ok(SessionJob::Skipped);
                };
                let Some(target) = stopping_ref(session, request.session_id) else {
                    return Ok(SessionJob::Skipped);
                };
                self.ports
                    .agents
                    .answer_permission(
                        &target,
                        &request.cli_request_id,
                        request.state == PermissionState::Allowed,
                        request.message.as_deref(),
                    )
                    .await?;
                Ok(SessionJob::Done)
            }
            JobKind::SessionDeliverDecision => {
                Ok(SessionJob::Input(Box::new(SessionInput::DeliverDecision {
                    decision_id: field(p, "decision_id")?,
                })))
            }
            JobKind::SessionInterrupt => {
                let Some(target) = stopping_ref(session, field(p, "session_id")?) else {
                    return Ok(SessionJob::Skipped);
                };
                self.ports.agents.interrupt(&target).await?;
                Ok(SessionJob::Done)
            }
            JobKind::SessionSteer => {
                let Some(target) = session_ref(session, field(p, "session_id")?) else {
                    return Ok(SessionJob::Skipped);
                };
                let text: String = field(p, "text")?;
                self.ports
                    .agents
                    .steer(&target, field(p, "n")?, &text)
                    .await?;
                Ok(SessionJob::Done)
            }
            JobKind::SessionReattach => {
                let id: SessionId = field(p, "session_id")?;
                let Some(target) = session_ref(session, id) else {
                    return Ok(SessionJob::Skipped);
                };
                if !self.ports.agents.unit_active(&target).await? {
                    return Ok(SessionJob::Input(Box::new(SessionInput::Gone {
                        cause: "runner_exited".into(),
                    })));
                }
                let last = session.map_or(0, |s| s.last_offset);
                let from_offset = if last == 0 { 0 } else { last + 1 };
                match self.ports.agents.reattach(&target, from_offset).await {
                    Ok(r) => Ok(SessionJob::Input(Box::new(SessionInput::Reattached {
                        runner_version: r.runner_version,
                        from_offset,
                    }))),
                    Err(e) if e.code == "version_mismatch" => {
                        self.ports
                            .agents
                            .stop(id, Some(&target.runner_unit))
                            .await
                            .best_effort("stop a runner of another version");
                        Ok(SessionJob::Input(Box::new(SessionInput::Gone {
                            cause: "version_mismatch".into(),
                        })))
                    }
                    Err(e) => Err(e),
                }
            }
            JobKind::SessionStop => {
                let id: SessionId = field(p, "session_id")?;
                let unit: Option<String> = p
                    .get("runner_unit")
                    .and_then(|u| u.as_str().map(String::from));
                self.ports.agents.stop(id, unit.as_deref()).await?;
                Ok(SessionJob::Done)
            }
            _ => Ok(SessionJob::Skipped),
        }
    }
}

impl Engine {
    pub async fn run_next_job(&self) -> Result<Option<JobReport>> {
        let Some(job) = self.claim_job()? else {
            return Ok(None);
        };
        let work_item = saka_core::job::work_item_of(&job.payload)
            .map(|id| id.to_string())
            .unwrap_or_default();
        let span = tracing::info_span!(
            "job",
            correlation_id = %job.correlation_id,
            job_kind = job.kind.as_str(),
            job_id = %job.id,
            work_item_id = %work_item,
        );
        let execution = self.execute_leased(&job).instrument(span.clone()).await;
        let adapter_ok = match &execution {
            Ok(Effect::Skipped) => None,
            Ok(_) => Some(true),
            Err(_) => Some(false),
        };
        let report = span.in_scope(|| self.complete_job(&job, execution))?;
        self.counters.job(job.kind, &report.result, adapter_ok);
        Ok(Some(report))
    }

    pub fn claim_job(&self) -> Result<Option<ClaimedJob>> {
        Ok(self.store.claim_job(&self.cfg.worker_id, self.now())?)
    }

    async fn execute_leased(&self, job: &ClaimedJob) -> Execution {
        let run = self.execute(job);
        tokio::pin!(run);
        let period = std::time::Duration::from_millis(LEASE_EXTEND_MS.unsigned_abs());
        loop {
            tokio::select! {
                result = &mut run => return result,
                _ = tokio::time::sleep(period) => {
                    if let Err(e) = self.store.extend_lease(job.id, &self.cfg.worker_id, self.now()) {
                        tracing::warn!(job_id = %job.id, error = %e, "lease extension failed");
                    }
                }
            }
        }
    }

    pub async fn execute(&self, job: &ClaimedJob) -> Execution {
        if job.is_over_limit() {
            return Err(PortError::permanent(
                "lease_expired",
                json!({ "attempts": job.attempts }),
            ));
        }
        if job.kind == JobKind::ScmWebhookPrune {
            self.prune_webhooks().map_err(internal)?;
            return Ok(Effect::Nothing);
        }
        if job.kind == JobKind::WorkspaceReloadConfig {
            return self.reload_job(job).await;
        }
        if job.kind == JobKind::HookRun {
            return self.hook_job(job).await;
        }
        if job.kind == JobKind::ScmScanChanges {
            return self.scan_job(job).await;
        }
        if job.kind == JobKind::ScmUserLinkPoll {
            return self.user_link_poll_job(job).await;
        }
        if job.kind == JobKind::ScmUserLinkRefresh {
            return self.user_link_refresh_job(job).await;
        }
        let id = match saka_core::job::owner_of(&job.payload) {
            Some(RunOwner::StageRun { work_item_id, .. }) => work_item_id,
            Some(RunOwner::DraftAttempt {
                spec_draft_id,
                attempt_id,
            }) => return self.execute_draft(job, spec_draft_id, attempt_id).await,
            None => field(&job.payload, "work_item_id")?,
        };
        let Some(task) = self
            .store
            .read(|c| read::load_task(c, id))
            .map_err(|e| internal(e.into()))?
        else {
            return Ok(Effect::Skipped);
        };
        let p = &job.payload;
        match job.kind {
            JobKind::EnvPrepare => self.env_prepare(&task, owner_run(p)?).await,
            JobKind::EnvPrepareReview => {
                self.prepare_review(&task, field(p, "stage_run_id")?).await
            }
            JobKind::ScmPostReview => self.post_review_job(&task, p).await,
            JobKind::SessionStart => self.session_start(&task, owner_run(p)?, job.id).await,
            JobKind::SessionSendTurn => {
                self.send_turn(
                    &task,
                    field(p, "session_id")?,
                    field(p, "turn")?,
                    field(p, "prompt")?,
                )
                .await
            }
            JobKind::SessionDeliverPermissionAnswer
            | JobKind::SessionDeliverDecision
            | JobKind::SessionInterrupt
            | JobKind::SessionSteer
            | JobKind::SessionReattach
            | JobKind::SessionStop => Ok(self
                .session_job(job.kind, p, task.session.as_ref(), &task.permissions)
                .await?
                .effect(|i| Effect::Fact(TaskInput::Session(i)))),
            JobKind::EnvInspectWorktree => {
                self.inspect(&task, field(p, "session_id")?, field(p, "turn")?)
                    .await
            }
            JobKind::ScmPushBranch => self.push(&task, field(p, "local_sha")?).await,
            JobKind::ScmPushDocsBranch => self.push_docs(&task, field(p, "local_sha")?).await,
            JobKind::ScmOpenChangeRequest => self.open_change(&task).await,
            JobKind::ScmOpenDocsChangeRequest => self.open_docs_change(&task, job.id).await,
            JobKind::ScmMergeDocs => self.merge_docs(&task).await,
            JobKind::ScmPollChangeRequest => self.poll(&task, field(p, "change_request_id")?).await,
            JobKind::ScmMerge => self.merge(&task, field(p, "head_sha")?).await,
            JobKind::ScmIntegrate => self.integrate(&task).await,
            JobKind::EnvVerifyMerged => self.verify(&task, job).await,
            JobKind::EnvTeardown => self.teardown(&task, field(p, "environment_id")?).await,
            JobKind::ScmWebhookPrune => Ok(Effect::Skipped),
            JobKind::GateVerify => {
                self.gate_verify(&task, field(p, "stage_run_id")?, field(p, "head_sha")?)
                    .await
            }
            JobKind::ScmReportStatus => self.report_status(&task, p).await,
            JobKind::ScmPollCommit => self.poll_commit(&task, field(p, "sha")?).await,
            JobKind::DraftLint
            | JobKind::DraftCommit
            | JobKind::WorkspaceReloadConfig
            | JobKind::ScmScanChanges
            | JobKind::ScmUserLinkPoll
            | JobKind::ScmUserLinkRefresh
            | JobKind::HookRun => Ok(Effect::Skipped),
            JobKind::ScmDeleteBranch => {
                let wanted = task.is_at(Stage::Sync, StagePhase::TearingDown)
                    || task.item.state == saka_core::vocab::WorkItemState::Done;
                if !wanted {
                    return Ok(Effect::Skipped);
                }
                let repo = self.repo_ref_of(field(p, "repo_id")?)?;
                let branch: String = field(p, "branch")?;
                self.ports.scm.delete_branch(&repo, &branch).await?;
                Ok(Effect::Nothing)
            }
        }
    }

    pub fn complete_job(&self, job: &ClaimedJob, execution: Execution) -> Result<JobReport> {
        if job.kind == JobKind::HookRun {
            return self.complete_hook_job(job, execution);
        }
        let owner = self.cfg.worker_id.clone();
        let draft = saka_core::job::spec_draft_of(&job.payload);
        let actor = match job.kind {
            JobKind::DraftCommit => crate::draft::ARCHITECT_ACTOR.to_string(),
            kind => format!("system:worker:{}", kind.as_str()),
        };
        let cause = Cause {
            actor,
            causation_id: Some(job.id),
            correlation_id: job.correlation_id,
        };
        let work_item = saka_core::job::work_item_of(&job.payload);
        let succeeded = || JobOutcome::Succeeded {
            id: job.id,
            owner: owner.clone(),
        };
        let done = |events: Vec<Event>| Extra {
            outcome: Some(succeeded()),
            events,
            ..Extra::default()
        };
        let (input, extra, result) = match execution {
            Ok(Effect::Fact(input)) => {
                (Some(Input::Task(input)), done(vec![]), JobResult::Succeeded)
            }
            Ok(Effect::Draft { input, events }) => (
                Some(Input::Draft(input)),
                done(events),
                JobResult::Succeeded,
            ),
            Ok(Effect::Nothing | Effect::Hook(_)) => (None, done(vec![]), JobResult::Succeeded),
            Ok(Effect::Skipped) => (None, done(vec![]), JobResult::Skipped),
            Ok(Effect::Later(next)) => (
                None,
                Extra {
                    outcome: Some(succeeded()),
                    jobs: vec![next],
                    ..Extra::default()
                },
                JobResult::Succeeded,
            ),
            Err(e) if e.retryable && job.attempts < job.max_attempts => {
                let run_at = e.retry_at.unwrap_or_else(|| job.backoff(self.now()));
                (
                    None,
                    Extra {
                        outcome: Some(JobOutcome::Retry {
                            id: job.id,
                            owner: owner.clone(),
                            error: e.to_json(),
                            run_at,
                        }),
                        ..Extra::default()
                    },
                    JobResult::Retrying {
                        code: e.code.clone(),
                    },
                )
            }
            Err(e) => {
                tracing::warn!(job_id = %job.id, kind = job.kind.as_str(), code = %e.code, params = %e.params, "job exhausted");
                if job.kind == JobKind::WorkspaceReloadConfig {
                    let project = job.payload.get("project_id").cloned().unwrap_or_default();
                    tracing::warn!(project_id = %project, code = %e.code, "workspace.config.reload_failed");
                }
                let last_error = Coded {
                    code: e.code.clone(),
                    params: e.params.clone(),
                };
                let exhausted = Event::JobExhausted {
                    job_id: job.id,
                    kind: job.kind,
                    last_error: last_error.clone(),
                };
                let teardown = field::<EnvironmentId>(&job.payload, "environment_id")
                    .ok()
                    .filter(|_| job.kind == JobKind::EnvTeardown);
                let input = if let (Some(_), Some(environment_id)) = (draft, teardown) {
                    Input::Draft(DraftInput::TeardownFailed {
                        environment_id,
                        error: last_error,
                    })
                } else if draft.is_some() {
                    Input::Draft(DraftInput::JobExhausted {
                        kind: job.kind,
                        error: last_error,
                    })
                } else {
                    Input::Task(TaskInput::JobExhausted {
                        kind: job.kind,
                        error: last_error,
                    })
                };
                (
                    Some(input),
                    Extra {
                        outcome: Some(JobOutcome::Dead {
                            id: job.id,
                            owner: owner.clone(),
                            error: e.to_json(),
                        }),
                        events: vec![exhausted],
                        ..Extra::default()
                    },
                    JobResult::Exhausted { code: e.code },
                )
            }
        };
        let committed = match (draft, work_item, input) {
            (Some(id), _, input) => {
                let input = match input {
                    Some(Input::Draft(i)) => Some(i),
                    _ => None,
                };
                self.decide_draft(id, input, Mode::Fact, &cause, extra)
                    .map(|_| ())
            }
            (None, Some(id), input) => {
                let input = match input {
                    Some(Input::Task(i)) => Some(i),
                    _ => None,
                };
                self.decide(id, input, Mode::Fact, &cause, extra)
                    .map(|_| ())
            }
            (None, None, _) => self.commit_outcome_only(extra),
        };
        // A prepare that will run again keeps its block; any other end frees the reservation,
        // since a committed environment row now holds the block in the store.
        let rerun = match &committed {
            Ok(()) => matches!(result, JobResult::Retrying { .. }),
            Err(e) => matches!(e, EngineError::Store(StoreError::LeaseLost { .. })),
        };
        if job.kind == JobKind::EnvPrepare
            && !rerun
            && let Ok(run) = owner_run(&job.payload)
        {
            self.release_ports(EnvironmentId(run.0));
        }
        let result = match committed {
            Ok(()) => result,
            Err(EngineError::Store(StoreError::LeaseLost { .. })) => JobResult::LeaseLost,
            Err(EngineError::Rejected(saka_core::rejection::Rejection::NotFound)) => {
                self.commit_outcome_only(Extra {
                    outcome: Some(succeeded()),
                    ..Extra::default()
                })?;
                JobResult::Skipped
            }
            Err(e) => return Err(e),
        };
        Ok(JobReport {
            kind: job.kind,
            idempotency_key: job.idempotency_key.clone(),
            result,
        })
    }

    fn commit_outcome_only(&self, extra: Extra) -> Result<()> {
        let now = self.now();
        let mut batch = saka_store::Batch::new(now);
        batch.job_outcome = extra.outcome;
        let cause = self.cause("system:worker");
        for e in &extra.events {
            self.push_event(&mut batch, e, None, &cause, now);
        }
        self.commit(&batch).map(|_| ())
    }

    async fn reload_job(&self, job: &ClaimedJob) -> Execution {
        let project: saka_core::ids::ProjectId = field(&job.payload, "project_id")?;
        let cause = Cause {
            actor: format!("system:worker:{}", job.kind.as_str()),
            causation_id: Some(job.id),
            correlation_id: job.correlation_id,
        };
        match self
            .reload_project_config(project, saka_core::vocab::ReloadTrigger::Poll, &cause, None)
            .await
        {
            Ok(_) | Err(EngineError::Rejected(_)) => Ok(Effect::Nothing),
            Err(EngineError::Port(e)) => Err(e),
            Err(e) => Err(internal(e)),
        }
    }

    async fn scan_job(&self, job: &ClaimedJob) -> Execution {
        let project: saka_core::ids::ProjectId = field(&job.payload, "project_id")?;
        let repo: RepoId = field(&job.payload, "repo_id")?;
        match self.scan_changes(project, repo).await {
            Ok(_) | Err(EngineError::Rejected(_)) => Ok(Effect::Nothing),
            Err(EngineError::Port(e)) => Err(e),
            Err(e) => Err(internal(e)),
        }
    }

    fn repo_ref(&self, task: &Task) -> std::result::Result<RepoRef, PortError> {
        self.repo_ref_of(task.item.repo_id)
    }

    pub(crate) fn repo_ref_of(&self, repo_id: RepoId) -> std::result::Result<RepoRef, PortError> {
        let repo = self
            .store
            .read(|c| read::load_repo(c, repo_id))
            .map_err(|e| internal(e.into()))?
            .ok_or_else(|| PortError::permanent("repo_not_found", json!({ "repo_id": repo_id })))?;
        Ok(RepoRef {
            repo_id: repo.id,
            integration_id: repo.integration_id,
            full_name: repo.full_name,
            url: repo.url,
        })
    }

    fn bound(&self, task: &Task) -> std::result::Result<(Project, ProjectRepo), PortError> {
        let project = self
            .store
            .read(|c| read::load_project(c, task.item.project_id))
            .map_err(|e| internal(e.into()))?
            .ok_or_else(|| PortError::permanent("project_not_found", json!({})))?;
        let binding = project.binding(task.item.repo_id).cloned().ok_or_else(|| {
            PortError::permanent("repo_not_bound", json!({ "repo_id": task.item.repo_id }))
        })?;
        Ok((project, binding))
    }

    fn base_branch(&self, task: &Task) -> std::result::Result<String, PortError> {
        self.bound(task).map(|(_, binding)| binding.base_branch)
    }

    fn docs_request(
        &self,
        task: &Task,
        environment_id: EnvironmentId,
    ) -> std::result::Result<Option<DocsRequest>, PortError> {
        if task.item.origin != WorkItemOrigin::SpecStep {
            return Ok(None);
        }
        let (project, _) = self.bound(task)?;
        if project.workspace_repo_id == task.item.repo_id {
            return Ok(None);
        }
        let pinned = self.pinned(task).map_err(internal)?;
        let start = task.docs_change.as_ref().map_or_else(
            || pinned.source_commit().to_string(),
            |c| c.head_sha.clone(),
        );
        Ok(Some(DocsRequest {
            environment_id,
            repo: self.repo_ref_of(project.workspace_repo_id)?,
            commit: start,
            project_path: project.project_path,
            branch: Some(docs_branch(&task.item.branch)),
        }))
    }

    fn docs_scope(docs: &DocsCheckout) -> Vec<String> {
        if docs.project_path == "." {
            vec![saka_core::scope::EVERYTHING.to_string()]
        } else {
            vec![format!("{}/**", docs.project_path)]
        }
    }

    async fn docs_violation(
        &self,
        task: &Task,
        docs: &DocsCheckout,
        base: &str,
        head_sha: &str,
        fetch: bool,
        stage: ScopeStage,
    ) -> std::result::Result<Option<ScopeViolation>, PortError> {
        let scope = PathScope::parse(&Self::docs_scope(docs))
            .map_err(|_| PortError::permanent("path_scope_invalid", json!({ "target": "docs" })))?;
        if scope.allows_everything() {
            return Ok(None);
        }
        let repo = self.repo_ref_of(docs.repo_id)?;
        let changed = self
            .ports
            .env
            .changed_paths(&repo, base, head_sha, fetch)
            .await?;
        let violation = scope.violation(&changed.paths, changed.truncated);
        if let Some(v) = &violation {
            self.counters.path_scope_violation(stage.as_str());
            tracing::warn!(
                work_item_id = %task.item.id,
                stage = stage.as_str(),
                target = "docs",
                head_sha,
                count = v.count,
                "delivery.path_scope.violated"
            );
        }
        Ok(violation)
    }

    fn effective_scope(&self, task: &Task) -> std::result::Result<Vec<String>, PortError> {
        self.bound(task)
            .map(|(project, binding)| project.effective_scope(&binding))
    }

    async fn scope_violation(
        &self,
        task: &Task,
        base: &str,
        head_sha: &str,
        fetch: bool,
        stage: ScopeStage,
    ) -> std::result::Result<Option<ScopeViolation>, PortError> {
        let patterns = self.effective_scope(task)?;
        let scope = PathScope::parse(&patterns).map_err(|_| {
            PortError::permanent(
                "path_scope_invalid",
                json!({ "repo_id": task.item.repo_id }),
            )
        })?;
        if scope.allows_everything() {
            return Ok(None);
        }
        let repo = self.repo_ref(task)?;
        let changed = self
            .ports
            .env
            .changed_paths(&repo, base, head_sha, fetch)
            .await?;
        let violation = scope.violation(&changed.paths, changed.truncated);
        if let Some(v) = &violation {
            self.counters.path_scope_violation(stage.as_str());
            tracing::warn!(
                work_item_id = %task.item.id,
                stage = stage.as_str(),
                head_sha,
                count = v.count,
                "delivery.path_scope.violated"
            );
        }
        Ok(violation)
    }
}
