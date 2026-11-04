use std::collections::BTreeMap;

use saka_core::draft::DraftInput;
use saka_core::event::{Event, Prompt};
use saka_core::gate::{Check, GatePolicy};
use saka_core::ids::{ChangeRequestId, EnvironmentId, SessionId, StageRunId};
use saka_core::job::{Job, JobKind};
use saka_core::model::{
    Coded, Environment, EnvironmentSetup, PermissionRequest, Project, ProjectRepo, RunOwner,
    Session, Task, ToolchainUse,
};
use saka_core::scope::{PathScope, ScopeViolation};
use saka_core::session::SessionInput;
use saka_core::task::TaskInput;
use saka_core::vocab::{
    BlockerCode, ChangeState, CheckConclusion, CheckKind, CheckStatus, EnvironmentState,
    PermissionState, ScopeStage, Stage, StagePhase,
};
use saka_ports::{
    Checkout, EnvRef, LaunchProfile, MergeOutcome, MergeRequest, OpenChangeRequest, PortError,
    PortResult, PrepareRequest, PushOutcome, PushRequest, RecipeCommand, RecipePhase, RepoRef,
    SessionLaunch, SessionRef, StatusReport, StatusState,
};
use saka_store::{ClaimedJob, JobOutcome, LEASE_EXTEND_MS, StoreError, read};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tracing::Instrument;

use crate::recipe::{self, Recipe};
use crate::toolchain;
use crate::{Cause, Engine, EngineError, Extra, Mode, Result, text};

pub const VERIFY_ROUNDS: u32 = 3;
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
}

pub type Execution = std::result::Result<Effect, PortError>;

enum Input {
    Task(TaskInput),
    Draft(DraftInput),
}

pub(crate) enum Refusal {
    Block(BlockerCode, Value),
    Fail(PortError),
}

impl Refusal {
    // Outside environment preparation a refusal has no blocker of its own; the job fails with it.
    fn into_error(self) -> PortError {
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
    Input(SessionInput),
    Done,
    Skipped,
}

impl SessionJob {
    pub(crate) fn effect(self, wrap: impl FnOnce(SessionInput) -> Effect) -> Effect {
        match self {
            SessionJob::Input(input) => wrap(input),
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
                    return Ok(SessionJob::Input(SessionInput::Gone {
                        cause: "runner_exited".into(),
                    }));
                }
                let last = session.map_or(0, |s| s.last_offset);
                let from_offset = if last == 0 { 0 } else { last + 1 };
                match self.ports.agents.reattach(&target, from_offset).await {
                    Ok(r) => Ok(SessionJob::Input(SessionInput::Reattached {
                        runner_version: r.runner_version,
                        from_offset,
                    })),
                    Err(e) if e.code == "version_mismatch" => {
                        let _ = self.ports.agents.stop(id, Some(&target.runner_unit)).await;
                        Ok(SessionJob::Input(SessionInput::Gone {
                            cause: "version_mismatch".into(),
                        }))
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
            JobKind::ScmOpenChangeRequest => self.open_change(&task).await,
            JobKind::ScmPollChangeRequest => self.poll(&task, field(p, "change_request_id")?).await,
            JobKind::ScmMerge => self.merge(&task, field(p, "head_sha")?).await,
            JobKind::EnvVerifyMerged => self.verify(&task, job).await,
            JobKind::EnvTeardown => self.teardown(&task, field(p, "environment_id")?).await,
            JobKind::ScmWebhookPrune => Ok(Effect::Skipped),
            JobKind::GateVerify => {
                self.gate_verify(&task, field(p, "stage_run_id")?, field(p, "head_sha")?)
                    .await
            }
            JobKind::ScmReportStatus => self.report_status(&task, p).await,
            JobKind::ScmPollCommit => self.poll_commit(&task, field(p, "sha")?).await,
            JobKind::DraftLint | JobKind::DraftCommit | JobKind::WorkspaceReloadConfig => {
                Ok(Effect::Skipped)
            }
            JobKind::ScmDeleteBranch => {
                let wanted = task.is_at(Stage::Sync, StagePhase::TearingDown)
                    || task.item.state == saka_core::vocab::WorkItemState::Done;
                if !wanted {
                    return Ok(Effect::Skipped);
                }
                let repo = self.repo_ref(&task)?;
                self.ports
                    .scm
                    .delete_branch(&repo, &task.item.branch)
                    .await?;
                Ok(Effect::Nothing)
            }
        }
    }

    pub fn complete_job(&self, job: &ClaimedJob, execution: Execution) -> Result<JobReport> {
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
            Ok(Effect::Nothing) => (None, done(vec![]), JobResult::Succeeded),
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

    fn repo_ref(&self, task: &Task) -> std::result::Result<RepoRef, PortError> {
        let repo = self
            .store
            .read(|c| read::load_repo(c, task.item.repo_id))
            .map_err(|e| internal(e.into()))?
            .ok_or_else(|| {
                PortError::permanent("repo_not_found", json!({ "repo_id": task.item.repo_id }))
            })?;
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
            vars: self.env_vars(task, env.id, env.port_start, &recipe, &env.toolchains),
            path_prefix: env.toolchains.iter().map(|t| t.bin_dir.clone()).collect(),
            secrets: Vec::new(),
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
    ) -> BTreeMap<String, String> {
        let mut vars = recipe.env.clone();
        for t in toolchains {
            vars.extend(t.env.clone());
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
        recipe: &Recipe,
        code: BlockerCode,
        params: Value,
    ) -> Execution {
        let _ = self.run_teardown_commands(env, recipe).await;
        self.ports.env.teardown(env).await?;
        Ok(Effect::Fact(TaskInput::EnvironmentFailed { code, params }))
    }

    async fn install_toolchains(
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
        };
        let text = match self.ports.env.read_recipe(&repo, &prepared.base_sha).await {
            Ok(text) => text,
            Err(e) if !e.retryable => {
                let params = json!({ "code": e.code, "field": null, "line": null });
                return self
                    .abandon_environment(
                        &env,
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
                    .abandon_environment(&env, &Recipe::default(), code, params)
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
                    .abandon_environment(&env, &Recipe::default(), code, params)
                    .await;
            }
            Err(Refusal::Fail(e)) => return Err(e),
        };
        let port_start = self.reserve_ports(environment_id)?;
        env.vars = self.env_vars(task, environment_id, Some(port_start), &recipe, &toolchains);
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
                    .abandon_environment(&env, &recipe, BlockerCode::EnvSetupFailed, params)
                    .await;
            }
        }
        Ok(Effect::Fact(TaskInput::EnvironmentPrepared {
            environment_id,
            root_path: prepared.root_path,
            worktree_path: prepared.worktree_path,
            base_sha: prepared.base_sha,
            setup: EnvironmentSetup {
                port_start: Some(port_start),
                port_count: Some(self.cfg.env.ports_per_env),
                recipe: text.as_ref().map(|_| recipe.to_json()),
                recipe_sha256: text.as_deref().map(recipe::sha256),
                setup,
                toolchains,
                path_scope,
            },
        }))
    }

    async fn session_start(&self, task: &Task, run: StageRunId, job: uuid::Uuid) -> Execution {
        let current = task.stage_run.as_ref().is_some_and(|r| r.id == run);
        let Some(env) = task.ready_environment().filter(|_| current) else {
            return Ok(Effect::Skipped);
        };
        if !task.is_at(Stage::Dev, StagePhase::RunningSession) || task.live_session().is_some() {
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
        let launched = self
            .ports
            .agents
            .start(&SessionLaunch {
                session_id,
                owner: view.owner,
                environment: self
                    .env_ref_with_secrets(task, env)
                    .map_err(Refusal::into_error)?,
                resume_cli_session_id: saka_core::resume::plan(&view)
                    .and_then(|p| p.cli_session_id),
                profile: LaunchProfile::Task,
                model: None,
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
        })))
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
        Ok(Effect::Fact(TaskInput::WorktreeInspected {
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
        if in_sync {
            self.ports.env.check_out(&env_ref, &head).await?;
        }
        let base = env.base_sha.clone().unwrap_or_default();
        let worktree = self.ports.env.inspect(&env_ref, &base).await?;
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

    async fn push(&self, task: &Task, local_sha: String) -> Execution {
        if !task.is_at(Stage::Dev, StagePhase::Pushing) {
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
            PushOutcome::Pushed { sha } => TaskInput::BranchPushed { sha },
            PushOutcome::Rejected { remote_sha } => TaskInput::PushRejected {
                remote_sha,
                local_sha,
            },
        }))
    }

    async fn open_change(&self, task: &Task) -> Execution {
        if !task.is_at(Stage::Dev, StagePhase::OpeningChange) {
            return Ok(Effect::Skipped);
        }
        let base_branch = self.base_branch(task)?;
        let (title, body) = self.change_text(task).map_err(internal)?;
        let opened = self
            .ports
            .scm
            .open_change_request(&OpenChangeRequest {
                repo: self.repo_ref(task)?,
                head_branch: task.item.branch.clone(),
                base_branch: base_branch.clone(),
                title,
                body,
            })
            .await?;
        // One change request per task: a repeated job records the same id.
        Ok(Effect::Fact(TaskInput::ChangeOpened {
            change_request_id: ChangeRequestId(task.item.id.0),
            number: opened.number,
            url: opened.url,
            head_sha: opened.head_sha,
            base_branch,
        }))
    }

    async fn poll(&self, task: &Task, cr: ChangeRequestId) -> Execution {
        let Some(change) = task
            .change_request
            .as_ref()
            .filter(|c| c.id == cr && saka_core::task::watches_change(task))
        else {
            return Ok(Effect::Skipped);
        };
        let snapshot = self
            .ports
            .scm
            .change_request(&self.repo_ref(task)?, change.number)
            .await?;
        Ok(Effect::Fact(TaskInput::ChangeObserved {
            state: snapshot.state,
            head_sha: snapshot.head_sha,
            merge_commit_sha: snapshot.merge_commit_sha,
            merged_at: snapshot.merged_at,
            checks: snapshot.checks,
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
                        merge_commit_sha,
                        merged_at: snapshot.merged_at,
                    }));
                }
            }
            ChangeState::Closed => {
                return Ok(Effect::Fact(TaskInput::ChangeObserved {
                    state: snapshot.state,
                    head_sha: snapshot.head_sha,
                    merge_commit_sha: None,
                    merged_at: None,
                    checks: snapshot.checks,
                }));
            }
            ChangeState::Open => {}
        }
        // The merge names this head, so the head checked here is the head that lands.
        let base = format!("refs/remotes/origin/{}", change.base_branch);
        if let Some(violation) = self
            .scope_violation(task, &base, &head_sha, true, ScopeStage::Merge)
            .await?
        {
            return Ok(Effect::Fact(TaskInput::PathScopeViolated {
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
                merge_commit_sha,
                merged_at,
            },
            MergeOutcome::Rejected { reason } => TaskInput::MergeRejected { reason },
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
        if found || round >= VERIFY_ROUNDS {
            return Ok(Effect::Fact(TaskInput::MergeVerified { found, base_tip }));
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
        self.ports.env.teardown(&env_ref).await?;
        Ok(Effect::Fact(TaskInput::EnvironmentTornDown {
            teardown_failures: failures,
        }))
    }
}
