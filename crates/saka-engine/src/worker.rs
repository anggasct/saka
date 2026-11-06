use std::collections::BTreeMap;

use saka_core::authorship::{self, CredentialBlock, ResolvedAuthor, SessionAuthor, UserLink};
use saka_core::draft::DraftInput;
use saka_core::event::{Event, Prompt};
use saka_core::gate::{Check, GatePolicy};
use saka_core::ids::{ChangeRequestId, EnvironmentId, RepoId, SessionId, StageRunId};
use saka_core::job::{Job, JobKind};
use saka_core::model::{
    Coded, DocsCheckout, Environment, EnvironmentSetup, PermissionRequest, Project, ProjectRepo,
    RunOwner, Session, Task, ToolchainUse,
};
use saka_core::rejection::Rejection;
use saka_core::scope::{PathScope, ScopeViolation};
use saka_core::session::SessionInput;
use saka_core::task::{DocsInspection, Integration, TaskInput};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    AuthorshipMode, BlockerCode, ChangeCredentialKind, ChangeRole, ChangeState, CheckConclusion,
    CheckKind, CheckStatus, DocsMode, EnvironmentState, PermissionState, RefreshReason, ScopeStage,
    Stage, StagePhase, TokenInvalidCause, UserLinkCause, UserTokenCause, WorkItemOrigin,
};
use saka_ports::{
    ChangeCredential, ChangeOpening, Checkout, DocsRequest, EnqueueOutcome, EnvRef, ExtraDir,
    LaunchProfile, MergeOutcome, MergeRequest, OpenChangeRequest, PortError, PortResult,
    PrepareRequest, PushOutcome, PushRequest, Rebase, RecipeCommand, RecipePhase, RepoRef,
    SessionLaunch, SessionRef, StatusReport, StatusState,
};
use saka_store::{ClaimedJob, JobOutcome, LEASE_EXTEND_MS, StoreError, read};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tracing::Instrument;

use crate::accounts::LinkCredential;
use crate::recipe::{self, Recipe};
use crate::toolchain;
use crate::{Cause, Engine, EngineError, Extra, Mode, Result, text};

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
    // Outside environment preparation a refusal has no blocker of its own; the job fails with it.
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
                        let _ = self.ports.agents.stop(id, Some(&target.runner_unit)).await;
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

    // A task in another repo than the workspace reads the project folder at its spec's commit.
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
        // A docs change already pushed is where the next checkout continues.
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

    // The scope is read at use time, so a reload applies to the next check of a running task.
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

    fn env_ref(&self, task: &Task, env: &Environment) -> std::result::Result<EnvRef, PortError> {
        let recipe = env
            .recipe
            .as_ref()
            .map(Recipe::from_json)
            .unwrap_or_default();
        Ok(EnvRef {
            environment_id: env.id,
            repo: self.repo_ref(task)?,
            branch: env.branch.clone(),
            root_path: env.root_path.clone(),
            worktree_path: env.worktree_path.clone(),
            vars: self.env_vars(
                task,
                env.id,
                env.port_start,
                &recipe,
                &env.toolchains,
                env.docs.as_ref(),
            ),
            path_prefix: env.toolchains.iter().map(|t| t.bin_dir.clone()).collect(),
            secrets: Vec::new(),
            extra_dirs: extra_dirs(env.docs.as_ref()),
        })
    }

    fn env_ref_with_secrets(
        &self,
        task: &Task,
        env: &Environment,
    ) -> std::result::Result<EnvRef, Refusal> {
        let mut env_ref = self.env_ref(task, env).map_err(Refusal::Fail)?;
        let recipe = env
            .recipe
            .as_ref()
            .map(Recipe::from_json)
            .unwrap_or_default();
        env_ref.secrets = self.recipe_secrets(task.item.project_id, &recipe)?;
        Ok(env_ref)
    }

    fn env_vars(
        &self,
        task: &Task,
        environment_id: EnvironmentId,
        port_start: Option<u16>,
        recipe: &Recipe,
        toolchains: &[ToolchainUse],
        docs: Option<&DocsCheckout>,
    ) -> BTreeMap<String, String> {
        let mut vars = recipe.env.clone();
        for t in toolchains {
            vars.extend(t.env.clone());
        }
        if let Some(docs) = docs {
            vars.insert("SAKA_DOCS_DIR".into(), docs.path.clone());
        }
        vars.insert("SAKA_TASK_KEY".into(), task.item.task_key.clone());
        vars.insert("SAKA_ENVIRONMENT_ID".into(), environment_id.to_string());
        let wanted = recipe.ports_or(self.cfg.env.ports_per_env);
        if let Some(start) = port_start.filter(|_| wanted > 0) {
            vars.insert("SAKA_PORT_BASE".into(), start.to_string());
            vars.insert("SAKA_PORT_COUNT".into(), wanted.to_string());
            vars.insert("PORT".into(), start.to_string());
        }
        vars
    }

    fn reserve_ports(&self, environment_id: EnvironmentId) -> std::result::Result<u16, PortError> {
        let live = self
            .store
            .read(read::live_port_blocks)
            .map_err(|e| internal(e.into()))?;
        let mut in_flight = self
            .ports_in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(start) = in_flight.get(&environment_id) {
            return Ok(*start);
        }
        let taken = |start: u16| {
            live.iter().any(|(s, _)| *s == start) || in_flight.values().any(|s| *s == start)
        };
        let start = (0..self.cfg.env.port_blocks())
            .filter_map(|i| self.cfg.env.block_start(i))
            .find(|s| !taken(*s))
            .ok_or_else(|| {
                PortError::retryable(
                    "port_block_unavailable",
                    json!({ "blocks": self.cfg.env.port_blocks() }),
                )
            })?;
        in_flight.insert(environment_id, start);
        Ok(start)
    }

    fn release_ports(&self, environment_id: EnvironmentId) {
        self.ports_in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&environment_id);
    }

    async fn run_teardown_commands(&self, env: &EnvRef, recipe: &Recipe) -> Vec<Value> {
        let mut failures = Vec::new();
        for (index, command) in recipe.teardown.iter().enumerate() {
            let run = RecipeCommand {
                phase: RecipePhase::Teardown,
                index,
                command: command.clone(),
                run: None,
                timeout_ms: None,
            };
            match self.ports.env.run_recipe_command(env, &run).await {
                Ok(o) if o.succeeded() => {}
                Ok(o) if o.timed_out => {
                    failures.push(json!({ "command_index": index, "timed_out": true }));
                }
                Ok(o) => failures.push(json!({ "command_index": index, "exit_code": o.exit_code })),
                Err(e) => failures.push(json!({ "command_index": index, "error": e.code })),
            }
        }
        failures
    }

    async fn abandon_environment(
        &self,
        env: &EnvRef,
        docs: Option<&RepoRef>,
        recipe: &Recipe,
        code: BlockerCode,
        params: Value,
    ) -> Execution {
        let _ = self.run_teardown_commands(env, recipe).await;
        if let Some(repo) = docs {
            self.ports
                .env
                .teardown_docs(env.environment_id, repo)
                .await?;
        }
        self.ports.env.teardown(env).await?;
        Ok(Effect::Fact(TaskInput::EnvironmentFailed { code, params }))
    }

    pub(crate) async fn install_toolchains(
        &self,
        repo: &RepoRef,
        base_sha: &str,
        recipe: &Recipe,
    ) -> std::result::Result<Vec<ToolchainUse>, Refusal> {
        let mut files = toolchain::Files::default();
        for path in toolchain::FILES {
            match self.ports.env.read_repo_file(repo, base_sha, path).await {
                Ok(text) => files.set(path, text),
                Err(e) if !e.retryable => {
                    return Err(Refusal::Block(
                        BlockerCode::EnvSetupFailed,
                        json!({ "error": e.to_json() }),
                    ));
                }
                Err(e) => return Err(Refusal::Fail(e)),
            }
        }
        let wanted = toolchain::detect(&recipe.toolchain, &files)
            .map_err(|u| Refusal::Block(BlockerCode::ToolchainUnpinned, u.to_json()))?;
        let mut installed = Vec::new();
        for (tool, version) in wanted {
            match self.ports.toolchains.install(tool, &version).await {
                Ok(t) => installed.push(t),
                Err(e) if !e.retryable && e.code == "toolchain_unavailable" => {
                    return Err(Refusal::Block(BlockerCode::ToolchainUnavailable, e.params));
                }
                Err(e) => return Err(Refusal::Fail(e)),
            }
        }
        Ok(installed)
    }

    async fn env_prepare(&self, task: &Task, run: StageRunId) -> Execution {
        let current = task.stage_run.as_ref().is_some_and(|r| r.id == run);
        if !current || task.ready_environment().is_some() {
            return Ok(Effect::Skipped);
        }
        // One environment per stage run: a repeated job finds the one it made.
        let environment_id = EnvironmentId(run.0);
        let repo = self.repo_ref(task)?;
        let prepared = self
            .ports
            .env
            .prepare(&PrepareRequest {
                environment_id,
                repo: repo.clone(),
                checkout: Checkout::Branch(task.item.branch.clone()),
                base_branch: self.base_branch(task)?,
            })
            .await?;
        let path_scope = self.effective_scope(task)?;
        let mut env = EnvRef {
            environment_id,
            repo: repo.clone(),
            branch: Some(task.item.branch.clone()),
            root_path: prepared.root_path.clone(),
            worktree_path: prepared.worktree_path.clone(),
            vars: BTreeMap::new(),
            path_prefix: Vec::new(),
            secrets: Vec::new(),
            extra_dirs: Vec::new(),
        };
        if task.item.origin == WorkItemOrigin::ExternalChange
            && let Some(cr) = &task.change_request
        {
            self.ports.env.check_out(&env, &cr.head_sha).await?;
        }
        let request = self.docs_request(task, environment_id)?;
        let docs_repo = request.as_ref().map(|r| &r.repo);
        let docs = match &request {
            None => None,
            Some(request) => match self.ports.env.prepare_docs(request).await {
                Ok(prepared) => Some(DocsCheckout {
                    repo_id: request.repo.repo_id,
                    full_name: request.repo.full_name.clone(),
                    commit: self
                        .pinned(task)
                        .map_err(internal)?
                        .source_commit()
                        .to_string(),
                    project_path: request.project_path.clone(),
                    path: prepared.path,
                    mode: DocsMode::Writable,
                    branch: request.branch.clone(),
                    base_sha: prepared.base_sha,
                    pushed_sha: task.docs_change.as_ref().map(|c| c.head_sha.clone()),
                }),
                Err(e) if !e.retryable => {
                    let params = json!({ "error": e.to_json() });
                    return self
                        .abandon_environment(
                            &env,
                            docs_repo,
                            &Recipe::default(),
                            BlockerCode::EnvSetupFailed,
                            params,
                        )
                        .await;
                }
                Err(e) => return Err(e),
            },
        };
        env.extra_dirs = extra_dirs(docs.as_ref());
        let text = match self.ports.env.read_recipe(&repo, &prepared.base_sha).await {
            Ok(text) => text,
            Err(e) if !e.retryable => {
                let params = json!({ "code": e.code, "field": null, "line": null });
                return self
                    .abandon_environment(
                        &env,
                        docs_repo,
                        &Recipe::default(),
                        BlockerCode::EnvRecipeInvalid,
                        params,
                    )
                    .await;
            }
            Err(e) => return Err(e),
        };
        let parsed = match text.as_deref() {
            None => Ok(Recipe::default()),
            Some(t) => recipe::parse(t, self.cfg.env.ports_per_env),
        };
        let recipe = match parsed {
            Ok(r) => r,
            Err(issues) => {
                let first = issues.first();
                let params = json!({
                    "code": first.map(|i| i.code),
                    "field": first.and_then(|i| i.field.clone()),
                    "line": first.and_then(|i| i.line),
                    "issues": issues.iter().map(|i| i.to_json()).collect::<Vec<_>>(),
                });
                return self
                    .abandon_environment(
                        &env,
                        docs_repo,
                        &Recipe::default(),
                        BlockerCode::EnvRecipeInvalid,
                        params,
                    )
                    .await;
            }
        };
        // Checked before any download or command, so a missing secret costs nothing to report.
        env.secrets = match self.recipe_secrets(task.item.project_id, &recipe) {
            Ok(s) => s,
            Err(Refusal::Block(code, params)) => {
                return self
                    .abandon_environment(&env, docs_repo, &Recipe::default(), code, params)
                    .await;
            }
            Err(Refusal::Fail(e)) => return Err(e),
        };
        let toolchains = match self
            .install_toolchains(&repo, &prepared.base_sha, &recipe)
            .await
        {
            Ok(t) => t,
            Err(Refusal::Block(code, params)) => {
                return self
                    .abandon_environment(&env, docs_repo, &Recipe::default(), code, params)
                    .await;
            }
            Err(Refusal::Fail(e)) => return Err(e),
        };
        let port_start = self.reserve_ports(environment_id)?;
        env.vars = self.env_vars(
            task,
            environment_id,
            Some(port_start),
            &recipe,
            &toolchains,
            docs.as_ref(),
        );
        env.path_prefix = toolchains.iter().map(|t| t.bin_dir.clone()).collect();
        let mut setup = Vec::new();
        for (index, command) in recipe.setup.iter().enumerate() {
            let run = RecipeCommand {
                phase: RecipePhase::Setup,
                index,
                command: command.clone(),
                run: None,
                timeout_ms: None,
            };
            let outcome = self.ports.env.run_recipe_command(&env, &run).await?;
            setup.push(json!({
                "command_index": index,
                "exit_code": outcome.exit_code,
                "timed_out": outcome.timed_out,
                "duration_ms": outcome.duration_ms,
            }));
            if !outcome.succeeded() {
                let mut failed = json!({ "command_index": index, "log": outcome.log_path });
                if outcome.timed_out {
                    failed["timed_out"] = json!(true);
                } else {
                    failed["exit_code"] = json!(outcome.exit_code);
                }
                let params =
                    json!({ "error": { "code": "setup_command_failed", "params": failed } });
                return self
                    .abandon_environment(
                        &env,
                        docs_repo,
                        &recipe,
                        BlockerCode::EnvSetupFailed,
                        params,
                    )
                    .await;
            }
        }
        Ok(Effect::Fact(TaskInput::EnvironmentPrepared {
            environment_id,
            root_path: prepared.root_path,
            worktree_path: prepared.worktree_path,
            base_sha: prepared.base_sha,
            setup: Box::new(EnvironmentSetup {
                port_start: Some(port_start),
                port_count: Some(self.cfg.env.ports_per_env),
                recipe: text.as_ref().map(|_| recipe.to_json()),
                recipe_sha256: text.as_deref().map(recipe::sha256),
                setup,
                toolchains,
                path_scope,
                docs,
            }),
        }))
    }

    async fn session_start(&self, task: &Task, run: StageRunId, job: uuid::Uuid) -> Execution {
        let current = task.stage_run.as_ref().is_some_and(|r| r.id == run);
        let Some(env) = task.ready_environment().filter(|_| current) else {
            return Ok(Effect::Skipped);
        };
        let reviewing = task.is_at(Stage::Review, StagePhase::RunningSession);
        if !(reviewing || task.is_writing_at(StagePhase::RunningSession))
            || task.live_session().is_some()
        {
            return Ok(Effect::Skipped);
        }
        // One start job per session: a repeated start replaces the runner it left behind.
        let session_id = match &task.session {
            Some(previous) if previous.owner.stage_run_id() == Some(run) => SessionId(job),
            _ => SessionId(run.0),
        };
        let Some(view) = saka_core::task::run_view(task) else {
            return Ok(Effect::Skipped);
        };
        let (profile, environment, resolved) = if reviewing {
            let bare = EnvRef {
                environment_id: env.id,
                repo: self.repo_ref(task)?,
                branch: None,
                root_path: env.root_path.clone(),
                worktree_path: env.worktree_path.clone(),
                vars: BTreeMap::new(),
                path_prefix: Vec::new(),
                secrets: Vec::new(),
                extra_dirs: Vec::new(),
            };
            let bot = ResolvedAuthor {
                identity: self.cfg.git_identity.clone(),
                author: SessionAuthor::saka(),
            };
            (LaunchProfile::Reviewer, bare, bot)
        } else {
            let resolved = if task.external.is_some() {
                ResolvedAuthor {
                    identity: self.cfg.git_identity.clone(),
                    author: SessionAuthor::saka(),
                }
            } else {
                match self.session_author(task)? {
                    Ok(resolved) => resolved,
                    Err(block) => {
                        return Ok(Effect::Fact(TaskInput::CredentialBlocked {
                            target: saka_core::vocab::ChangeRole::Code,
                            block,
                        }));
                    }
                }
            };
            let environment = self
                .env_ref_with_secrets(task, env)
                .map_err(Refusal::into_error)?;
            (LaunchProfile::Task, environment, resolved)
        };
        self.align_branch(task, env).await?;
        let launched = self
            .ports
            .agents
            .start(&SessionLaunch {
                session_id,
                owner: view.owner,
                environment,
                resume_cli_session_id: saka_core::resume::plan(&view)
                    .and_then(|p| p.cli_session_id),
                profile,
                model: None,
                author: resolved.identity,
            })
            .await?;
        Ok(Effect::Fact(TaskInput::Session(SessionInput::Launched {
            session_id,
            cli: launched.cli,
            cli_version: launched.cli_version,
            model: launched.model,
            account: launched.account,
            runner_unit: launched.runner_unit,
            socket_path: launched.socket_path,
            log_path: launched.log_path,
            author: resolved.author,
        })))
    }

    // The worktree follows the head the task knows when the operator pushed from elsewhere; commits
    // that are not on the remote branch, or uncommitted work, are left alone.
    async fn align_branch(
        &self,
        task: &Task,
        env: &Environment,
    ) -> std::result::Result<(), PortError> {
        let Some(cr) = task
            .change_request
            .as_ref()
            .filter(|c| c.state == ChangeState::Open)
        else {
            return Ok(());
        };
        let env_ref = self.env_ref(task, env)?;
        let base = env.base_sha.clone().unwrap_or_default();
        let state = self.ports.env.inspect(&env_ref, &base).await?;
        if state.dirty || state.head_sha == cr.head_sha {
            return Ok(());
        }
        let on_remote = self
            .ports
            .env
            .contains_commit(&self.repo_ref(task)?, &state.head_sha, &task.item.branch)
            .await?;
        if on_remote.as_deref() == Some(cr.head_sha.as_str()) {
            self.ports.env.check_out(&env_ref, &cr.head_sha).await?;
        }
        Ok(())
    }

    fn session_author(
        &self,
        task: &Task,
    ) -> std::result::Result<std::result::Result<ResolvedAuthor, CredentialBlock>, PortError> {
        let mode = self
            .authorship_mode(task.item.project_id)
            .map_err(internal)?;
        match mode {
            AuthorshipMode::Bot => Ok(Ok(ResolvedAuthor {
                identity: self.cfg.git_identity.clone(),
                author: SessionAuthor::saka(),
            })),
            AuthorshipMode::User => {
                let integration = self.repo_ref(task)?.integration_id;
                let link = self.operator_link(&integration)?;
                Ok(authorship::user_link_author(&integration, link.as_ref()))
            }
            AuthorshipMode::UserPat => {
                let integration = self.repo_ref(task)?.integration_id;
                let current = self
                    .project_authorship_now(task.item.project_id)
                    .map_err(internal)?;
                Ok(authorship::user_token_author(
                    current.as_ref(),
                    &integration,
                    self.now(),
                ))
            }
        }
    }

    fn operator_link(&self, integration: &str) -> std::result::Result<Option<UserLink>, PortError> {
        self.store
            .read(|c| saka_store::user_link::user_link(c, crate::OPERATOR, integration))
            .map(|row| row.map(|r| r.link))
            .map_err(|e| internal(e.into()))
    }

    // The credential is the project's mode at the moment the change opens; a refresh that is due
    // happens first and is committed before the token is used.
    async fn change_credential(
        &self,
        task: &Task,
        integration: &str,
    ) -> std::result::Result<std::result::Result<Opener, CredentialBlock>, PortError> {
        let mode = self
            .authorship_mode(task.item.project_id)
            .map_err(internal)?;
        let blocked = |cause: UserLinkCause| CredentialBlock::LinkRequired {
            integration_id: integration.to_string(),
            cause,
            phase: StagePhase::OpeningChange,
        };
        match mode {
            AuthorshipMode::Bot => Ok(Ok(Opener {
                credential: ChangeCredential::App,
                by: OpenedBy::App,
            })),
            AuthorshipMode::User => {
                let Some(link) = self.operator_link(integration)? else {
                    return Ok(Err(blocked(UserLinkCause::Missing)));
                };
                match self.refresh_user_link(link.id, RefreshReason::Use).await {
                    Ok(LinkCredential::Ready(token)) => Ok(Ok(Opener {
                        credential: ChangeCredential::User(token),
                        by: OpenedBy::Link(link),
                    })),
                    Ok(LinkCredential::Relink(cause)) => Ok(Err(blocked(cause.into()))),
                    Err(EngineError::Rejected(Rejection::UserLinkNotFound)) => {
                        Ok(Err(blocked(UserLinkCause::Missing)))
                    }
                    Err(EngineError::Port(e)) => Err(e),
                    Err(e) => Err(internal(e)),
                }
            }
            AuthorshipMode::UserPat => self.token_credential(task, integration),
        }
    }

    fn token_credential(
        &self,
        task: &Task,
        integration: &str,
    ) -> std::result::Result<std::result::Result<Opener, CredentialBlock>, PortError> {
        let project = task.item.project_id;
        let phase = StagePhase::OpeningChange;
        let current = self.project_authorship_now(project).map_err(internal)?;
        if let Err(block) =
            authorship::usable_token(current.as_ref(), integration, phase, self.now())
        {
            return Ok(Err(block));
        }
        let rejected = CredentialBlock::TokenInvalid {
            cause: UserTokenCause::Rejected,
            phase,
        };
        let Some(opened) = self.open_user_token(project).map_err(internal)? else {
            return Ok(Err(rejected));
        };
        if opened.token.integration_id != integration {
            return Ok(Err(CredentialBlock::TokenInvalid {
                cause: UserTokenCause::Missing,
                phase,
            }));
        }
        Ok(Ok(Opener {
            credential: ChangeCredential::User(opened.value),
            by: OpenedBy::Token {
                set_at: opened.token.set_at,
                login: opened.token.identity.login,
            },
        }))
    }

    async fn send_turn(
        &self,
        task: &Task,
        session: SessionId,
        turn: u32,
        prompt: Prompt,
    ) -> Execution {
        let Some(session) = session_ref(task.session.as_ref(), session) else {
            return Ok(Effect::Skipped);
        };
        let body = match &prompt {
            Prompt::WorkPacket { retry_of } if task.external.is_some() => {
                let retry = retry_of.as_ref().and_then(crate::text::retry_text);
                let title = task.external.as_ref().map_or("", |e| e.title.as_str());
                let text = crate::fix_packet::render(title, &task.item.branch, retry.as_deref());
                tracing::info!(
                    work_item_id = %task.item.id,
                    session_id = %session.session_id,
                    bytes = text.len(),
                    "fix.packet"
                );
                text
            }
            Prompt::WorkPacket { retry_of } => {
                let retry = retry_of.as_ref().and_then(crate::text::retry_text);
                let packet = self.packet_of(task, retry).map_err(internal)?;
                let bytes = packet.text.len() as u64;
                self.counters.work_packet(bytes);
                tracing::info!(
                    work_item_id = %task.item.id,
                    session_id = %session.session_id,
                    bytes,
                    sha256 = %packet.sha256,
                    documents = packet.documents.len(),
                    "session.work_packet"
                );
                packet.text
            }
            Prompt::ReviewPacket => self.review_packet_text(task).await?,
            other => self.turn_text(task, other).map_err(internal)?,
        };
        self.ports.agents.send_turn(&session, turn, &body).await?;
        Ok(Effect::Fact(TaskInput::Session(
            SessionInput::TurnAccepted { turn },
        )))
    }

    async fn inspect(&self, task: &Task, session: SessionId, turn: u32) -> Execution {
        let (Some(_), Some(env)) = (
            session_ref(task.session.as_ref(), session),
            task.ready_environment(),
        ) else {
            return Ok(Effect::Skipped);
        };
        let base = env.base_sha.clone().unwrap_or_default();
        let state = self
            .ports
            .env
            .inspect(&self.env_ref(task, env)?, &base)
            .await?;
        let outside = if !state.dirty && state.commits_ahead > 0 {
            self.scope_violation(task, &base, &state.head_sha, false, ScopeStage::Dev)
                .await?
        } else {
            None
        };
        let docs = match env.docs.as_ref().filter(|d| d.mode == DocsMode::Writable) {
            None => None,
            Some(d) => {
                let state = self.ports.env.inspect_docs(env.id, &d.base_sha).await?;
                let outside = if !state.dirty && state.commits_ahead > 0 {
                    self.docs_violation(task, d, &d.commit, &state.head_sha, false, ScopeStage::Dev)
                        .await?
                } else {
                    None
                };
                Some(DocsInspection {
                    dirty: state.dirty,
                    commits_ahead: state.commits_ahead,
                    head_sha: state.head_sha,
                    outside,
                })
            }
        };
        Ok(Effect::Fact(TaskInput::WorktreeInspected {
            docs,
            turn,
            dirty: state.dirty,
            commits_ahead: state.commits_ahead,
            head_sha: state.head_sha,
            outside,
        }))
    }

    async fn gate_verify(&self, task: &Task, run: StageRunId, head: String) -> Execution {
        let in_gate = task.is_at(Stage::Merge, StagePhase::AwaitingCi)
            || task.is_at(Stage::Merge, StagePhase::AwaitingMerge);
        let in_sync = task.is_at(Stage::Sync, StagePhase::Verifying);
        let current = task.stage_run.as_ref().is_some_and(|r| {
            r.id == run
                && if in_sync {
                    r.head_sha.as_deref() == Some(head.as_str())
                } else {
                    task.change_request
                        .as_ref()
                        .is_some_and(|c| c.head_sha == head)
                }
        });
        let Some(env) = task
            .ready_environment()
            .filter(|_| (in_gate || in_sync) && current)
        else {
            return Ok(Effect::Skipped);
        };
        let cause = self.cause("system:gate");
        self.decide(
            task.item.id,
            Some(TaskInput::SakaGateStarted {
                head_sha: head.clone(),
            }),
            Mode::Fact,
            &cause,
            Extra::default(),
        )
        .map_err(internal)?;
        let env_ref = self
            .env_ref_with_secrets(task, env)
            .map_err(Refusal::into_error)?;
        let base = env.base_sha.clone().unwrap_or_default();
        let mut worktree = self.ports.env.inspect(&env_ref, &base).await?;
        // A head the operator pushed is not in the worktree; a dirty one is never overwritten.
        if in_sync || (!worktree.dirty && worktree.head_sha != head) {
            self.ports.env.check_out(&env_ref, &head).await?;
            worktree = self.ports.env.inspect(&env_ref, &base).await?;
        }
        if worktree.head_sha != head || worktree.dirty {
            return Ok(Effect::Fact(TaskInput::SakaGateObserved {
                head_sha: head,
                checks: vec![Check {
                    name: saka_core::gate::SAKA_STATUS_CONTEXT.to_string(),
                    kind: CheckKind::Command,
                    status: CheckStatus::Completed,
                    conclusion: Some(CheckConclusion::Failure),
                    url: None,
                }],
                failure: Some(json!({ "reason": "worktree_moved" })),
            }));
        }
        let policy = GatePolicy::from_recipe(env.recipe.as_ref());
        let pinned = self.pinned(task).map_err(internal)?;
        let step_commands = pinned
            .step(task)
            .map(|s| s.commands.clone())
            .unwrap_or_default();
        let label: String = head.chars().take(12).collect();
        let started = std::time::Instant::now();
        let mut checks = Vec::new();
        let mut failure = None;
        for (index, command) in step_commands.iter().chain(&policy.commands).enumerate() {
            let run = RecipeCommand {
                phase: RecipePhase::Gate,
                index,
                command: command.clone(),
                run: Some(label.clone()),
                timeout_ms: Some(u64::from(policy.timeout_minutes) * 60_000),
            };
            let outcome = self.ports.env.run_recipe_command(&env_ref, &run).await?;
            let conclusion = if outcome.timed_out {
                CheckConclusion::TimedOut
            } else if outcome.succeeded() {
                CheckConclusion::Success
            } else {
                CheckConclusion::Failure
            };
            checks.push(Check {
                name: check_name(command),
                kind: CheckKind::Command,
                status: CheckStatus::Completed,
                conclusion: Some(conclusion),
                url: None,
            });
            if !outcome.succeeded() {
                let mut failed = json!({
                    "command": command,
                    "log": outcome.log_path,
                    "log_tail": outcome.output_tail,
                });
                if outcome.timed_out {
                    failed["timed_out"] = json!(true);
                } else {
                    failed["exit_code"] = json!(outcome.exit_code);
                }
                failure = Some(failed);
                break;
            }
        }
        let outcome = if failure.is_some() {
            "failed"
        } else {
            "passed"
        };
        self.counters
            .gate_run("saka", outcome, started.elapsed().as_secs_f64());
        tracing::info!(
            work_item_id = %task.item.id,
            head_sha = %head,
            outcome,
            commands = checks.len(),
            "gate.verify.finished"
        );
        Ok(Effect::Fact(TaskInput::SakaGateObserved {
            head_sha: head,
            checks,
            failure,
        }))
    }

    async fn report_status(&self, task: &Task, p: &Value) -> Execution {
        let sha: String = field(p, "sha")?;
        let description: String = field(p, "description")?;
        let state: String = field(p, "state")?;
        let Some(state) = StatusState::parse(&state) else {
            return Err(PortError::permanent(
                "job_payload_invalid",
                json!({ "field": "state" }),
            ));
        };
        let report = StatusReport {
            context: saka_core::gate::SAKA_STATUS_CONTEXT.to_string(),
            state,
            description,
        };
        let repo = self.repo_ref(task)?;
        match self.ports.scm.report_status(&repo, &sha, &report).await {
            Ok(()) => Ok(Effect::Nothing),
            Err(e) if !e.retryable => {
                tracing::warn!(code = %e.code, work_item_id = %task.item.id, "scm.status.failed");
                Ok(Effect::Nothing)
            }
            Err(e) => Err(e),
        }
    }

    async fn push_docs(&self, task: &Task, local_sha: String) -> Execution {
        let docs = task.environment.as_ref().and_then(|e| e.docs.as_ref());
        let (true, Some(docs)) = (task.is_writing_at(StagePhase::Pushing), docs) else {
            return Ok(Effect::Skipped);
        };
        if saka_core::task::docs_pending(task).as_deref() != Some(local_sha.as_str()) {
            return Ok(Effect::Skipped);
        }
        let outcome = self
            .ports
            .scm
            .push_branch(&PushRequest {
                repo: self.repo_ref_of(docs.repo_id)?,
                branch: docs.branch.clone().unwrap_or_default(),
                local_sha: local_sha.clone(),
                expected_remote_sha: docs.pushed_sha.clone(),
            })
            .await?;
        Ok(Effect::Fact(match outcome {
            PushOutcome::Pushed { sha } => TaskInput::BranchPushed {
                target: ChangeRole::Docs,
                sha,
            },
            PushOutcome::Rejected { remote_sha } => TaskInput::PushRejected {
                target: ChangeRole::Docs,
                remote_sha,
                local_sha,
            },
        }))
    }

    // Read before merging: after a crash the docs change may already be merged or closed.
    async fn merge_docs(&self, task: &Task) -> Execution {
        let Some(change) = task
            .docs_change
            .as_ref()
            .filter(|_| task.is_at(Stage::Sync, StagePhase::MergingDocs))
        else {
            return Ok(Effect::Skipped);
        };
        let docs = task.environment.as_ref().and_then(|e| e.docs.as_ref());
        let repo = self.repo_ref_of(change.repo_id)?;
        let snapshot = self.ports.scm.change_request(&repo, change.number).await?;
        let observed = |snapshot: saka_ports::ChangeSnapshot| {
            Ok(Effect::Fact(TaskInput::ChangeObserved {
                role: ChangeRole::Docs,
                state: snapshot.state,
                head_sha: snapshot.head_sha,
                merge_commit_sha: snapshot.merge_commit_sha,
                merged_at: snapshot.merged_at,
                checks: Vec::new(),
            }))
        };
        match snapshot.state {
            ChangeState::Merged | ChangeState::Closed => return observed(snapshot),
            // A head pushed from outside is recorded first; the merge then names that head.
            ChangeState::Open if snapshot.head_sha != change.head_sha => return observed(snapshot),
            ChangeState::Open => {}
        }
        let head_sha = change.head_sha.clone();
        let base = format!("refs/remotes/origin/{}", change.base_branch);
        // The docs folder bounds what may be merged; without it nothing is merged.
        let Some(docs) = docs else {
            return Err(PortError::permanent("docs_checkout_missing", json!({})));
        };
        if let Some(violation) = self
            .docs_violation(task, docs, &base, &head_sha, true, ScopeStage::Sync)
            .await?
        {
            return Ok(Effect::Fact(TaskInput::PathScopeViolated {
                role: ChangeRole::Docs,
                head_sha,
                violation,
            }));
        }
        let title: String = format!(
            "docs: {} (#{})",
            text::change_title(&task.item.work),
            change.number
        );
        let outcome = self
            .ports
            .scm
            .merge(&MergeRequest {
                repo,
                number: change.number,
                head_sha,
                method: self.cfg.loop_cfg.merge_method,
                commit_title: title,
            })
            .await?;
        Ok(Effect::Fact(match outcome {
            MergeOutcome::Merged {
                merge_commit_sha,
                merged_at,
            } => TaskInput::MergeSucceeded {
                role: ChangeRole::Docs,
                merge_commit_sha,
                merged_at,
            },
            MergeOutcome::Rejected { reason } => {
                let status = saka_core::task::merge_status(&reason);
                tracing::warn!(
                    work_item_id = %task.item.id,
                    number = change.number,
                    status,
                    "delivery.docs_change.merge_failed"
                );
                TaskInput::MergeRejected {
                    role: ChangeRole::Docs,
                    reason,
                }
            }
        }))
    }

    async fn push(&self, task: &Task, local_sha: String) -> Execution {
        if !task.is_writing_at(StagePhase::Pushing) {
            return Ok(Effect::Skipped);
        }
        let outcome = self
            .ports
            .scm
            .push_branch(&PushRequest {
                repo: self.repo_ref(task)?,
                branch: task.item.branch.clone(),
                local_sha: local_sha.clone(),
                expected_remote_sha: task.change_request.as_ref().map(|c| c.head_sha.clone()),
            })
            .await?;
        Ok(Effect::Fact(match outcome {
            PushOutcome::Pushed { sha } => TaskInput::BranchPushed {
                target: saka_core::vocab::ChangeRole::Code,
                sha,
            },
            PushOutcome::Rejected { remote_sha } => TaskInput::PushRejected {
                target: saka_core::vocab::ChangeRole::Code,
                remote_sha,
                local_sha,
            },
        }))
    }

    async fn open_change(&self, task: &Task) -> Execution {
        if !task.is_writing_at(StagePhase::OpeningChange) || task.change_request.is_some() {
            return Ok(Effect::Skipped);
        }
        let base_branch = self.base_branch(task)?;
        let (title, body) = self.change_text(task).map_err(internal)?;
        let request = OpenChangeRequest {
            repo: self.repo_ref(task)?,
            head_branch: task.item.branch.clone(),
            base_branch: base_branch.clone(),
            title,
            body,
            credential: ChangeCredential::App,
        };
        let (opened, opened_with) = match self.open_with(task, request).await? {
            Ok(opened) => opened,
            Err(block) => {
                return Ok(Effect::Fact(TaskInput::CredentialBlocked {
                    block,
                    target: ChangeRole::Code,
                }));
            }
        };
        // One change request per task: a repeated job records the same id.
        Ok(Effect::Fact(TaskInput::ChangeOpened {
            role: ChangeRole::Code,
            change_request_id: ChangeRequestId(task.item.id.0),
            number: opened.number,
            url: opened.url,
            head_sha: opened.head_sha,
            base_branch,
            opened_with,
        }))
    }

    async fn open_docs_change(&self, task: &Task, job: uuid::Uuid) -> Execution {
        let docs = task.environment.as_ref().and_then(|e| e.docs.as_ref());
        let (Some(docs), Some(code)) = (docs, task.change_request.as_ref()) else {
            return Ok(Effect::Skipped);
        };
        if !task.is_writing_at(StagePhase::OpeningChange)
            || !saka_core::task::needs_docs_change(task)
        {
            return Ok(Effect::Skipped);
        }
        let (project, _) = self.bound(task)?;
        let (code_title, _) = self.change_text(task).map_err(internal)?;
        let title: String = format!("docs: {code_title}")
            .chars()
            .take(DOCS_TITLE_MAX)
            .collect();
        let body = format!(
            "Docs for {}\n\nSaka-Task: {}\n",
            code.url, task.item.task_key
        );
        let request = OpenChangeRequest {
            repo: self.repo_ref_of(docs.repo_id)?,
            head_branch: docs.branch.clone().unwrap_or_default(),
            base_branch: project.base_branch.clone(),
            title,
            body,
            credential: ChangeCredential::App,
        };
        let (opened, opened_with) = match self.open_with(task, request).await? {
            Ok(opened) => opened,
            Err(block) => {
                return Ok(Effect::Fact(TaskInput::CredentialBlocked {
                    block,
                    target: ChangeRole::Docs,
                }));
            }
        };
        // The job id is stable across its retries; a repeated fact is dropped by the decider.
        Ok(Effect::Fact(TaskInput::ChangeOpened {
            role: ChangeRole::Docs,
            change_request_id: ChangeRequestId(job),
            number: opened.number,
            url: opened.url,
            head_sha: opened.head_sha,
            base_branch: project.base_branch,
            opened_with,
        }))
    }

    async fn open_with(
        &self,
        task: &Task,
        mut request: OpenChangeRequest,
    ) -> std::result::Result<
        std::result::Result<(saka_ports::OpenedChange, ChangeCredentialKind), CredentialBlock>,
        PortError,
    > {
        let integration = request.repo.integration_id.clone();
        let opener = match self.change_credential(task, &integration).await? {
            Ok(opener) => opener,
            Err(block) => return Ok(Err(block)),
        };
        request.credential = opener.credential;
        let opened = match self.ports.scm.open_change_request(&request).await? {
            ChangeOpening::Opened(opened) => opened,
            ChangeOpening::CredentialRejected => {
                let block = match &opener.by {
                    OpenedBy::Token { set_at, .. } => {
                        self.invalidate_user_token(
                            task.item.project_id,
                            *set_at,
                            TokenInvalidCause::Rejected,
                        )
                        .map_err(internal)?;
                        CredentialBlock::TokenInvalid {
                            cause: UserTokenCause::Rejected,
                            phase: StagePhase::OpeningChange,
                        }
                    }
                    OpenedBy::Link(link) => CredentialBlock::LinkRequired {
                        integration_id: integration,
                        cause: self.revoke_user_link(link)?,
                        phase: StagePhase::OpeningChange,
                    },
                    OpenedBy::App => {
                        return Err(PortError::permanent(
                            "scm_app_auth_refused",
                            json!({ "status": 401 }),
                        ));
                    }
                };
                return Ok(Err(block));
            }
            ChangeOpening::AccessDenied { status } => {
                return Ok(Err(CredentialBlock::AccessDenied {
                    credential: opener.by.kind(),
                    login: opener.by.login(),
                    status,
                }));
            }
        };
        Ok(Ok((opened, opener.by.kind())))
    }

    async fn poll(&self, task: &Task, cr: ChangeRequestId) -> Execution {
        if let Some(docs) = task
            .docs_change
            .as_ref()
            .filter(|c| c.id == cr && saka_core::task::watches_docs_change(task))
        {
            let snapshot = self
                .ports
                .scm
                .change_request(&self.repo_ref_of(docs.repo_id)?, docs.number)
                .await?;
            return Ok(Effect::Fact(TaskInput::ChangeObserved {
                role: ChangeRole::Docs,
                state: snapshot.state,
                head_sha: snapshot.head_sha,
                merge_commit_sha: snapshot.merge_commit_sha,
                merged_at: snapshot.merged_at,
                checks: Vec::new(),
            }));
        }
        let Some(change) = task
            .change_request
            .as_ref()
            .filter(|c| c.id == cr && saka_core::task::watches_change(task))
        else {
            return Ok(Effect::Skipped);
        };
        if task.item.origin == WorkItemOrigin::ExternalChange {
            return self.poll_external(task, change.number).await;
        }
        let snapshot = self
            .ports
            .scm
            .change_request(&self.repo_ref(task)?, change.number)
            .await?;
        if snapshot.state == ChangeState::Merged
            && matches!(task.item.stage, Some(Stage::Dev | Stage::Fix))
            && let Some(docs_head_sha) = saka_core::task::docs_pending(task)
        {
            tracing::info!(
                work_item_id = %task.item.id,
                docs_head_sha,
                "delivery.docs_change.discarded"
            );
        }
        let native = task.is_at(Stage::Merge, StagePhase::Merging)
            && task.stage_run.as_ref().is_some_and(|r| r.queue_native);
        if native && snapshot.state == ChangeState::Open {
            let repo = self.repo_ref(task)?;
            if !self.ports.scm.in_merge_queue(&repo, change.number).await? {
                return Ok(Effect::Fact(TaskInput::QueueLeft));
            }
        }
        Ok(Effect::Fact(TaskInput::ChangeObserved {
            role: ChangeRole::Code,
            state: snapshot.state,
            head_sha: snapshot.head_sha,
            merge_commit_sha: snapshot.merge_commit_sha,
            merged_at: snapshot.merged_at,
            checks: snapshot.checks,
        }))
    }

    async fn poll_external(&self, task: &Task, number: u64) -> Execution {
        let repo = self.repo_ref(task)?;
        let summary = self.ports.scm.change_summary(&repo, number).await?;
        let (merge_commit_sha, merged_at) = if summary.state == ChangeState::Merged {
            let merged = self.ports.scm.change_request(&repo, number).await?;
            (merged.merge_commit_sha, merged.merged_at)
        } else {
            (None, None)
        };
        Ok(Effect::Fact(TaskInput::ChangeObserved {
            role: ChangeRole::Code,
            state: summary.state,
            head_sha: summary.head_sha,
            merge_commit_sha,
            merged_at,
            checks: Vec::new(),
        }))
    }

    async fn poll_commit(&self, task: &Task, sha: String) -> Execution {
        let targeted = task.is_at(Stage::Sync, StagePhase::Verifying)
            && task
                .stage_run
                .as_ref()
                .is_some_and(|r| r.head_sha.as_deref() == Some(sha.as_str()));
        if !targeted {
            return Ok(Effect::Skipped);
        }
        let checks = self
            .ports
            .scm
            .commit_checks(&self.repo_ref(task)?, &sha)
            .await?;
        Ok(Effect::Fact(TaskInput::CommitChecksObserved {
            head_sha: sha,
            checks,
        }))
    }

    async fn integrate(&self, task: &Task) -> Execution {
        let Some(change) = task
            .change_request
            .as_ref()
            .filter(|_| task.is_at(Stage::Merge, StagePhase::Integrating))
        else {
            return Ok(Effect::Skipped);
        };
        let Some(env) = task.ready_environment() else {
            return Err(PortError::permanent(
                "environment_missing",
                json!({ "work_item_id": task.item.id }),
            ));
        };
        let repo = self.repo_ref(task)?;
        let native = self
            .ports
            .scm
            .capabilities(&repo.integration_id)
            .is_some_and(|c| c.merge_queue)
            && self
                .ports
                .scm
                .merge_queue(&repo, &change.base_branch)
                .await?;
        if native {
            return self.enqueue(task, repo).await;
        }
        let env_ref = self.env_ref(task, env)?;
        let head = change.head_sha.clone();
        let (outcome, label) = match self
            .ports
            .env
            .rebase(&env_ref, &change.base_branch, &head)
            .await?
        {
            Rebase::Current { head_sha, base_sha } => (
                Integration::Rebased {
                    from_sha: head,
                    head_sha,
                    base_sha,
                },
                "current",
            ),
            Rebase::Rebased { head_sha, base_sha } => {
                let pushed = self
                    .ports
                    .scm
                    .push_branch(&PushRequest {
                        repo: self.repo_ref(task)?,
                        branch: task.item.branch.clone(),
                        local_sha: head_sha.clone(),
                        expected_remote_sha: Some(head.clone()),
                    })
                    .await?;
                match pushed {
                    PushOutcome::Rejected { remote_sha } if remote_sha != head_sha => {
                        (Integration::Moved { remote_sha }, "moved")
                    }
                    _ => (
                        Integration::Rebased {
                            from_sha: head,
                            head_sha,
                            base_sha,
                        },
                        "rebased",
                    ),
                }
            }
            Rebase::Conflict { base_sha, paths } => {
                tracing::warn!(
                    work_item_id = %task.item.id,
                    count = paths.len(),
                    "delivery.merge.conflict"
                );
                (
                    Integration::Conflict {
                        head_sha: head,
                        base_sha,
                        paths,
                    },
                    "conflict",
                )
            }
            Rebase::Unclean => (Integration::Unclean { head_sha: head }, "unclean"),
        };
        self.counters.merge_turn(label);
        tracing::info!(
            work_item_id = %task.item.id,
            base_branch = change.base_branch,
            outcome = label,
            "delivery.merge.integrated"
        );
        Ok(Effect::Fact(TaskInput::Integrated(outcome)))
    }

    async fn enqueue(&self, task: &Task, repo: RepoRef) -> Execution {
        let Some(change) = task.change_request.as_ref() else {
            return Ok(Effect::Skipped);
        };
        let head = change.head_sha.clone();
        let fact = |outcome| Ok(Effect::Fact(TaskInput::Integrated(outcome)));
        let enqueued = Integration::Enqueued {
            head_sha: head.clone(),
        };
        if self.ports.scm.in_merge_queue(&repo, change.number).await? {
            return fact(enqueued);
        }
        let snapshot = self.ports.scm.change_request(&repo, change.number).await?;
        match snapshot.state {
            ChangeState::Merged => {
                if let Some(merge_commit_sha) = snapshot.merge_commit_sha {
                    return fact(Integration::Merged {
                        merge_commit_sha,
                        merged_at: snapshot.merged_at,
                    });
                }
            }
            ChangeState::Closed => {
                return Ok(Effect::Fact(TaskInput::ChangeObserved {
                    role: saka_core::vocab::ChangeRole::Code,
                    state: snapshot.state,
                    head_sha: snapshot.head_sha,
                    merge_commit_sha: None,
                    merged_at: None,
                    checks: snapshot.checks,
                }));
            }
            ChangeState::Open => {}
        }
        let base = format!("refs/remotes/origin/{}", change.base_branch);
        if let Some(violation) = self
            .scope_violation(task, &base, &head, true, ScopeStage::Merge)
            .await?
        {
            return Ok(Effect::Fact(TaskInput::PathScopeViolated {
                role: saka_core::vocab::ChangeRole::Code,
                head_sha: head,
                violation,
            }));
        }
        let outcome = self
            .ports
            .scm
            .enqueue(&MergeRequest {
                repo,
                number: change.number,
                head_sha: head,
                method: self.cfg.loop_cfg.merge_method,
                commit_title: format!(
                    "{} (#{})",
                    text::change_title(&task.item.work),
                    change.number
                ),
            })
            .await?;
        self.counters.merge_turn("native");
        tracing::info!(
            work_item_id = %task.item.id,
            base_branch = change.base_branch,
            "delivery.merge.enqueued"
        );
        fact(match outcome {
            EnqueueOutcome::Queued => enqueued,
            EnqueueOutcome::Merged {
                merge_commit_sha,
                merged_at,
            } => Integration::Merged {
                merge_commit_sha,
                merged_at,
            },
            EnqueueOutcome::Rejected { reason } => Integration::Rejected { reason },
        })
    }

    async fn merge(&self, task: &Task, head_sha: String) -> Execution {
        let Some(change) = task
            .change_request
            .as_ref()
            .filter(|_| task.is_at(Stage::Merge, StagePhase::Merging))
        else {
            return Ok(Effect::Skipped);
        };
        let repo = self.repo_ref(task)?;
        // Read first: after a crash the merge may already have happened.
        let snapshot = self.ports.scm.change_request(&repo, change.number).await?;
        match snapshot.state {
            ChangeState::Merged => {
                if let Some(merge_commit_sha) = snapshot.merge_commit_sha {
                    return Ok(Effect::Fact(TaskInput::MergeSucceeded {
                        role: saka_core::vocab::ChangeRole::Code,
                        merge_commit_sha,
                        merged_at: snapshot.merged_at,
                    }));
                }
            }
            ChangeState::Closed => {
                return Ok(Effect::Fact(TaskInput::ChangeObserved {
                    role: saka_core::vocab::ChangeRole::Code,
                    state: snapshot.state,
                    head_sha: snapshot.head_sha,
                    merge_commit_sha: None,
                    merged_at: None,
                    checks: snapshot.checks,
                }));
            }
            ChangeState::Open => {}
        }
        if change.head_sha != head_sha {
            return Ok(Effect::Skipped);
        }
        if snapshot.head_sha != head_sha {
            return Ok(Effect::Fact(TaskInput::ChangeObserved {
                role: saka_core::vocab::ChangeRole::Code,
                state: snapshot.state,
                head_sha: snapshot.head_sha,
                merge_commit_sha: None,
                merged_at: None,
                checks: snapshot.checks,
            }));
        }
        if !saka_core::task::merge_allowed(task, &head_sha) {
            return Ok(Effect::Fact(TaskInput::MergeNotReady));
        }
        // The merge names this head, so the head checked here is the head that lands.
        let base = format!("refs/remotes/origin/{}", change.base_branch);
        if let Some(violation) = self
            .scope_violation(task, &base, &head_sha, true, ScopeStage::Merge)
            .await?
        {
            return Ok(Effect::Fact(TaskInput::PathScopeViolated {
                role: saka_core::vocab::ChangeRole::Code,
                head_sha,
                violation,
            }));
        }
        let outcome = self
            .ports
            .scm
            .merge(&MergeRequest {
                repo,
                number: change.number,
                head_sha,
                method: self.cfg.loop_cfg.merge_method,
                commit_title: format!(
                    "{} (#{})",
                    text::change_title(&task.item.work),
                    change.number
                ),
            })
            .await?;
        Ok(Effect::Fact(match outcome {
            MergeOutcome::Merged {
                merge_commit_sha,
                merged_at,
            } => TaskInput::MergeSucceeded {
                role: saka_core::vocab::ChangeRole::Code,
                merge_commit_sha,
                merged_at,
            },
            MergeOutcome::Rejected { reason } => TaskInput::MergeRejected {
                role: saka_core::vocab::ChangeRole::Code,
                reason,
            },
        }))
    }

    async fn verify(&self, task: &Task, job: &ClaimedJob) -> Execution {
        let Some(change) = task
            .change_request
            .as_ref()
            .filter(|_| task.is_at(Stage::Sync, StagePhase::Verifying))
        else {
            return Ok(Effect::Skipped);
        };
        let round: u32 = job
            .idempotency_key
            .rsplit(':')
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(1);
        let base_tip = match &change.merge_commit_sha {
            Some(sha) => {
                self.ports
                    .env
                    .contains_commit(&self.repo_ref(task)?, sha, &change.base_branch)
                    .await?
            }
            None => None,
        };
        let found = base_tip.is_some();
        let docs_found = match task
            .docs_change
            .as_ref()
            .and_then(|c| c.merge_commit_sha.as_ref().map(|sha| (c, sha)))
        {
            Some((docs, sha)) => Some(
                self.ports
                    .env
                    .contains_commit(&self.repo_ref_of(docs.repo_id)?, sha, &docs.base_branch)
                    .await?
                    .is_some(),
            ),
            None => None,
        };
        if (found && docs_found != Some(false)) || round >= VERIFY_ROUNDS {
            return Ok(Effect::Fact(TaskInput::MergeVerified {
                found,
                base_tip,
                docs_found,
            }));
        }
        let run = task.stage_run.as_ref().map(|r| r.id);
        let mut payload = job.payload.clone();
        if let Value::Object(map) = &mut payload {
            map.insert("round".into(), json!(round + 1));
        }
        Ok(Effect::Later(Job::new(
            JobKind::EnvVerifyMerged,
            format!(
                "{}:{}",
                run.map(|r| r.to_string()).unwrap_or_default(),
                round + 1
            ),
            payload,
            self.now().plus_ms(VERIFY_PAUSE_MS),
        )))
    }

    async fn teardown(&self, task: &Task, id: EnvironmentId) -> Execution {
        let Some(env) = task
            .environment
            .as_ref()
            .filter(|e| e.id == id && e.state != EnvironmentState::TornDown)
        else {
            return Ok(Effect::Skipped);
        };
        // Teardown is best effort: a secret removed since setup only leaves its commands without it.
        let env_ref = match self.env_ref_with_secrets(task, env) {
            Ok(r) => r,
            Err(Refusal::Fail(e)) if e.retryable => return Err(e),
            Err(_) => self.env_ref(task, env)?,
        };
        // A cleanup retry after teardown_failed must not run the repo's teardown commands again.
        let failures = if env.state == EnvironmentState::TeardownFailed {
            Vec::new()
        } else {
            let recipe = env
                .recipe
                .as_ref()
                .map(Recipe::from_json)
                .unwrap_or_default();
            self.run_teardown_commands(&env_ref, &recipe).await
        };
        if let Some(docs) = &env.docs {
            let repo = self.repo_ref_of(docs.repo_id)?;
            self.ports.env.teardown_docs(env.id, &repo).await?;
        }
        self.ports.env.teardown(&env_ref).await?;
        Ok(Effect::Fact(TaskInput::EnvironmentTornDown {
            teardown_failures: failures,
        }))
    }
}
