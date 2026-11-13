use saka_core::deploy::{self, CheckRecord, Deployment, Input, Verification};
use saka_core::environment::{DeployAction, Environment, VerifySpec};
use saka_core::event::Event;
use saka_core::hook::{self, NamedRef, RepoRef, TaskRef};
use saka_core::ids::DeploymentId;
use saka_core::job::{Job, JobKind};
use saka_core::model::{Coded, Project};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    DeployCheckState, DeployPhase, DeploymentState, HookEvent, HookRunState, VerifyMode,
};
use saka_ports::PortError;
use saka_store::deployment as rows;
use saka_store::{Batch, ClaimedJob, JobOutcome, read};
use serde_json::{Value, json};

use crate::hooks::{HookReport, SignedPost};
use crate::hooks_run::{EnvFailure, EnvSpec, FailureCodes, HostRun};
use crate::worker::{Effect, Execution, JobReport, JobResult, field};
use crate::{Cause, Engine, EngineError, Result};

const DEPLOY_CODES: FailureCodes = FailureCodes {
    secret_missing: "deploy_secret_missing",
    environment_failed: "deploy_environment_failed",
    commit_unavailable: "deploy_environment_failed",
    timed_out: "deploy_timed_out",
    command_failed: "deploy_command_failed",
};

const VERIFY_CODES: FailureCodes = FailureCodes {
    secret_missing: "deploy_secret_missing",
    environment_failed: "verify_environment_failed",
    commit_unavailable: "verify_environment_failed",
    timed_out: "verify_failed",
    command_failed: "verify_failed",
};

#[derive(Debug, Clone, PartialEq)]
pub struct DeployWork {
    pub deployment: DeploymentId,
    pub inputs: Vec<Input>,
    pub jobs: Vec<Job>,
}

impl DeployWork {
    fn new(deployment: DeploymentId) -> DeployWork {
        DeployWork {
            deployment,
            inputs: Vec::new(),
            jobs: Vec::new(),
        }
    }

    fn fail(deployment: DeploymentId, phase: DeployPhase, code: &str, params: Value) -> DeployWork {
        DeployWork {
            inputs: vec![Input::Fail {
                phase,
                error: Coded {
                    code: code.to_string(),
                    params,
                },
            }],
            ..DeployWork::new(deployment)
        }
    }
}

fn check_of(
    phase: DeployPhase,
    ordinal: u32,
    command: Option<String>,
    report: &HookReport,
    started_at: Timestamp,
    finished_at: Timestamp,
    attempts: u32,
) -> CheckRecord {
    let timed_out = report.result["timed_out"].as_bool() == Some(true);
    let state = match (report.outcome, timed_out) {
        (HookRunState::Succeeded, _) => DeployCheckState::Passed,
        (_, true) => DeployCheckState::TimedOut,
        _ => DeployCheckState::Failed,
    };
    CheckRecord {
        phase,
        ordinal,
        command,
        state,
        exit_code: report.result["exit_code"]
            .as_i64()
            .and_then(|c| i32::try_from(c).ok()),
        http_status: report.result["http_status"]
            .as_u64()
            .and_then(|s| u16::try_from(s).ok()),
        attempts,
        duration_ms: report.duration_ms,
        output_tail: hook::tail(report.result["output_tail"].as_str().unwrap_or_default()),
        log: report.result["log"].as_str().map(str::to_string),
        started_at,
        finished_at,
    }
}

fn webhook_code(code: &str) -> String {
    match code {
        "hook_webhook_rejected" => "deploy_webhook_rejected".to_string(),
        "hook_webhook_unavailable" => "deploy_webhook_unavailable".to_string(),
        other => other.to_string(),
    }
}

impl Engine {
    pub(crate) fn environment_of(&self, d: &Deployment) -> Result<Option<(Environment, Project)>> {
        let Some(project) = self.store.read(|c| read::load_project(c, d.project_id))? else {
            return Ok(None);
        };
        let found = self
            .environments_of(&project)
            .into_iter()
            .find(|e| e.name == d.environment)
            .cloned();
        Ok(found.map(|e| (e, project)))
    }

    fn deploy_cause(&self, job: &ClaimedJob) -> Cause {
        Cause {
            actor: format!("system:worker:{}", job.kind.as_str()),
            causation_id: Some(job.id),
            correlation_id: job.correlation_id,
        }
    }

    fn deploy_load(&self, id: DeploymentId) -> Result<Option<Deployment>> {
        Ok(self.store.read(|c| rows::load(c, id))?)
    }

    fn repo_name_of(&self, project: &Project, d: &Deployment) -> String {
        project
            .repos
            .iter()
            .find(|b| b.repo_id == d.repo_id)
            .map_or_else(String::new, |b| b.name.clone())
    }

    pub(crate) async fn deploy_run_job(&self, job: &ClaimedJob) -> Execution {
        let id: DeploymentId = field(&job.payload, "deployment_id")?;
        let Some(mut d) = self.deploy_load(id).map_err(crate::worker::internal)? else {
            return Ok(Effect::Skipped);
        };
        let Some((env, project)) = self.environment_of(&d).map_err(crate::worker::internal)? else {
            return Ok(Effect::Deploy(DeployWork::fail(
                id,
                DeployPhase::Deploy,
                "deploy_environment_failed",
                json!({ "reason": "environment_removed" }),
            )));
        };
        if d.state == DeploymentState::Queued {
            let repo = self.repo_ref_of(d.repo_id)?;
            let tip = match self.ports.env.branch_tip(&repo, &d.branch).await {
                Ok(Some(tip)) => tip,
                Ok(None) => {
                    return Ok(Effect::Deploy(DeployWork::fail(
                        id,
                        DeployPhase::Deploy,
                        "deploy_branch_unavailable",
                        json!({ "branch": d.branch }),
                    )));
                }
                Err(e) if e.retryable => return Err(e),
                Err(e) => {
                    return Ok(Effect::Deploy(DeployWork::fail(
                        id,
                        DeployPhase::Deploy,
                        "deploy_branch_unavailable",
                        json!({ "branch": d.branch, "error": e.to_json() }),
                    )));
                }
            };
            let previous_sha = self
                .store
                .read(|c| rows::last_succeeded(c, d.project_id, &d.environment))
                .map_err(|e| crate::worker::internal(e.into()))?
                .and_then(|p| p.sha);
            let started = self
                .deploy_apply(
                    id,
                    vec![Input::Start {
                        sha: tip,
                        previous_sha,
                    }],
                    None,
                    &self.deploy_cause(job),
                )
                .map_err(crate::worker::internal)?;
            let Some(started) = started else {
                return Ok(Effect::Skipped);
            };
            d = started;
        }
        if d.state != DeploymentState::Deploying {
            return Ok(Effect::Skipped);
        }
        let started_at = self.now();
        let report = self.run_deploy_action(&d, &env, &project).await?;
        let finished_at = self.now();
        let check = check_of(
            DeployPhase::Deploy,
            0,
            None,
            &report,
            started_at,
            finished_at,
            1,
        );
        let mut work = DeployWork::new(id);
        work.inputs.push(Input::Check(check));
        if report.outcome != HookRunState::Succeeded {
            let error = report.error.unwrap_or_else(|| Coded {
                code: "deploy_environment_failed".to_string(),
                params: json!({}),
            });
            work.inputs.push(Input::Fail {
                phase: DeployPhase::Deploy,
                error: Coded {
                    code: webhook_code(&error.code),
                    params: error.params,
                },
            });
            return Ok(Effect::Deploy(work));
        }
        let verify = project.verify.get(&env.name);
        let verification = Verification {
            ready: verify.and_then(|v| v.ready.clone()),
            commands: verify.map(|v| v.commands.clone()).unwrap_or_default(),
            mode: if verify.is_some() {
                VerifyMode::Commands
            } else {
                VerifyMode::None
            },
        };
        let next = if verification.ready.is_some() {
            DeployPhase::Ready
        } else {
            DeployPhase::Verify
        };
        let more = verification.ready.is_some() || !verification.commands.is_empty();
        work.inputs.push(Input::Advance {
            to: next,
            verification: Some(verification),
        });
        if more {
            work.jobs.push(self.verify_job(id, 0, finished_at));
        } else {
            work.inputs.push(Input::Succeed);
        }
        Ok(Effect::Deploy(work))
    }

    fn verify_job(&self, id: DeploymentId, attempt: u32, run_at: Timestamp) -> Job {
        Job::new(
            JobKind::DeployVerify,
            format!("{id}:{attempt}"),
            json!({ "deployment_id": id }),
            run_at,
        )
    }

    async fn run_deploy_action(
        &self,
        d: &Deployment,
        env: &Environment,
        project: &Project,
    ) -> std::result::Result<HookReport, PortError> {
        let repo_name = self.repo_name_of(project, d);
        let vars = deploy::env_vars(d, &project.name, &repo_name);
        let timeout_ms = env.timeout_ms;
        match &env.action {
            DeployAction::RunHost { command, cwd } => {
                self.host_command(
                    HostRun {
                        name: d.id.to_string(),
                        command,
                        cwd: cwd.as_deref(),
                        env: vars.into_iter().collect(),
                        scratch_key: format!("{}/deploy-{}", project.id, env.name),
                        timeout_ms,
                    },
                    &DEPLOY_CODES,
                )
                .await
            }
            DeployAction::Run { command, secrets } => {
                let spec = EnvSpec {
                    id: self.new_id(),
                    project_id: project.id,
                    repo_id: d.repo_id,
                    base_branch: d.branch.clone(),
                    sha: d.sha.clone(),
                    secrets,
                    vars,
                };
                let opened = match self.open_environment(&spec, &DEPLOY_CODES).await {
                    Ok(opened) => opened,
                    Err(EnvFailure::Retry(e)) => return Err(e),
                    Err(EnvFailure::Failed(report)) => return Ok(report),
                };
                let result = self
                    .run_in_environment(&opened, 0, command, timeout_ms, &DEPLOY_CODES)
                    .await;
                self.close_environment(&opened, &d.id.to_string()).await;
                result
            }
            DeployAction::Webhook { url, secret } => {
                let tasks = self
                    .store
                    .read(|c| rows::tasks_of(c, d.id))
                    .map_err(|e| crate::worker::internal(e.into()))?
                    .into_iter()
                    .map(|t| TaskRef {
                        id: t.work_item_id,
                        key: t.task_key,
                    })
                    .collect::<Vec<_>>();
                let repo = self
                    .store
                    .read(|c| read::load_repo(c, d.repo_id))
                    .map_err(|e| crate::worker::internal(e.into()))?;
                let repo_ref = RepoRef {
                    id: d.repo_id,
                    name: repo_name,
                    full_name: repo.map(|r| r.full_name).unwrap_or_default(),
                };
                let named = NamedRef {
                    id: project.id.0,
                    name: project.name.clone(),
                };
                let body = serde_json::to_vec(&deploy::webhook_body(
                    d,
                    &named,
                    &repo_ref,
                    &tasks,
                    self.now(),
                ))
                .map_err(|e| {
                    PortError::permanent(
                        "deploy_payload_invalid",
                        json!({ "detail": e.to_string() }),
                    )
                })?;
                self.signed_webhook(
                    SignedPost {
                        project_id: project.id,
                        event: "deploy".to_string(),
                        delivery_id: d.id.to_string(),
                        body,
                        url,
                        secret_name: secret,
                        timeout_ms,
                    },
                    &DEPLOY_CODES,
                )
                .await
            }
        }
    }

    pub(crate) async fn deploy_verify_job(&self, job: &ClaimedJob) -> Execution {
        let id: DeploymentId = field(&job.payload, "deployment_id")?;
        let Some(d) = self.deploy_load(id).map_err(crate::worker::internal)? else {
            return Ok(Effect::Skipped);
        };
        if !matches!(
            d.state,
            DeploymentState::AwaitingReady | DeploymentState::Verifying
        ) {
            return Ok(Effect::Skipped);
        }
        let Some((env, project)) = self.environment_of(&d).map_err(crate::worker::internal)? else {
            let phase = d.phase.unwrap_or(DeployPhase::Verify);
            return Ok(Effect::Deploy(DeployWork::fail(
                id,
                phase,
                "verify_environment_failed",
                json!({ "reason": "environment_removed" }),
            )));
        };
        let verification = d.verification.clone().unwrap_or(Verification {
            ready: None,
            commands: Vec::new(),
            mode: VerifyMode::None,
        });
        let spec = project
            .verify
            .get(&env.name)
            .cloned()
            .unwrap_or_else(VerifySpec::default);
        let prior = self
            .store
            .read(|c| rows::checks(c, id))
            .map_err(|e| crate::worker::internal(e.into()))?;
        let now = self.now();
        let waiting_ready = d.state == DeploymentState::AwaitingReady;
        let ready_check = prior
            .iter()
            .find(|c| c.phase == DeployPhase::Ready && c.ordinal == 0);
        let ready_started = ready_check.map_or(now, |c| c.started_at);
        let attempts = ready_check.map_or(0, |c| c.attempts);
        if waiting_ready && now.since(ready_started) >= spec.ready_timeout_secs as i64 * 1000 {
            return Ok(Effect::Deploy(DeployWork::fail(
                id,
                DeployPhase::Ready,
                "ready_timed_out",
                json!({ "attempts": attempts }),
            )));
        }
        let repo_name = self.repo_name_of(&project, &d);
        let env_spec = EnvSpec {
            id: self.new_id(),
            project_id: project.id,
            repo_id: d.repo_id,
            base_branch: d.branch.clone(),
            sha: d.sha.clone(),
            secrets: &env.verify_secrets,
            vars: deploy::env_vars(&d, &project.name, &repo_name),
        };
        let phase = if waiting_ready {
            DeployPhase::Ready
        } else {
            DeployPhase::Verify
        };
        let opened = match self.open_environment(&env_spec, &VERIFY_CODES).await {
            Ok(opened) => opened,
            Err(EnvFailure::Retry(e)) => return Err(e),
            Err(EnvFailure::Failed(report)) => {
                let error = report.error.unwrap_or_else(|| Coded {
                    code: "verify_environment_failed".to_string(),
                    params: json!({}),
                });
                return Ok(Effect::Deploy(DeployWork::fail(
                    id,
                    phase,
                    &error.code,
                    error.params,
                )));
            }
        };
        let work = self
            .verify_in_environment(
                &opened,
                &d,
                &verification,
                &spec,
                waiting_ready,
                (ready_started, attempts),
            )
            .await;
        self.close_environment(&opened, &d.id.to_string()).await;
        work.map(Effect::Deploy)
    }

    async fn verify_in_environment(
        &self,
        opened: &crate::hooks_run::OpenEnvironment,
        d: &Deployment,
        verification: &Verification,
        spec: &VerifySpec,
        waiting_ready: bool,
        (ready_started, attempts): (Timestamp, u32),
    ) -> std::result::Result<DeployWork, PortError> {
        let mut work = DeployWork::new(d.id);
        let command_timeout_ms = spec.timeout_secs * 1000;
        if waiting_ready {
            let probe = verification.ready.clone().unwrap_or_default();
            let started_at = self.now();
            let report = self
                .run_in_environment(opened, 0, &probe, command_timeout_ms, &VERIFY_CODES)
                .await?;
            let finished_at = self.now();
            let tries = attempts + 1;
            let mut check = check_of(
                DeployPhase::Ready,
                0,
                Some(probe),
                &report,
                started_at,
                finished_at,
                tries,
            );
            check.started_at = ready_started;
            let passed = check.state == DeployCheckState::Passed;
            work.inputs.push(Input::Check(check));
            if !passed {
                let interval_ms = spec.ready_interval_secs as i64 * 1000;
                let deadline = ready_started.0 + spec.ready_timeout_secs as i64 * 1000;
                if finished_at.0 + interval_ms > deadline {
                    work.inputs.push(Input::Fail {
                        phase: DeployPhase::Ready,
                        error: Coded {
                            code: "ready_timed_out".to_string(),
                            params: json!({ "attempts": tries }),
                        },
                    });
                } else {
                    work.jobs.push(self.verify_job(
                        d.id,
                        tries,
                        Timestamp(finished_at.0 + interval_ms),
                    ));
                }
                return Ok(work);
            }
            work.inputs.push(Input::Advance {
                to: DeployPhase::Verify,
                verification: None,
            });
        }
        let mut failed = 0u32;
        for (index, command) in verification.commands.iter().enumerate() {
            let ordinal = u32::try_from(index).unwrap_or(u32::MAX);
            let started_at = self.now();
            let report = self
                .run_in_environment(
                    opened,
                    index + 1,
                    command,
                    command_timeout_ms,
                    &VERIFY_CODES,
                )
                .await?;
            let finished_at = self.now();
            let check = check_of(
                DeployPhase::Verify,
                ordinal,
                Some(command.clone()),
                &report,
                started_at,
                finished_at,
                1,
            );
            if check.state != DeployCheckState::Passed {
                failed += 1;
            }
            work.inputs.push(Input::Check(check));
        }
        if failed == 0 {
            work.inputs.push(Input::Succeed);
        } else {
            work.inputs.push(Input::Fail {
                phase: DeployPhase::Verify,
                error: Coded {
                    code: "verify_failed".to_string(),
                    params: json!({ "failed": failed, "total": verification.commands.len() }),
                },
            });
        }
        Ok(work)
    }

    pub(crate) fn complete_deploy_job(
        &self,
        job: &ClaimedJob,
        execution: Execution,
    ) -> Result<JobReport> {
        let id: DeploymentId = field(&job.payload, "deployment_id").map_err(|e| {
            EngineError::Internal(format!("deploy job without a deployment: {}", e.code))
        })?;
        let owner = self.cfg.worker_id.clone();
        let cause = self.deploy_cause(job);
        let succeeded = JobOutcome::Succeeded {
            id: job.id,
            owner: owner.clone(),
        };
        let (work, outcome, result) = match execution {
            Ok(Effect::Deploy(work)) => (Some(work), succeeded, JobResult::Succeeded),
            Ok(Effect::Skipped) => (None, succeeded, JobResult::Skipped),
            Ok(_) => (None, succeeded, JobResult::Succeeded),
            Err(e) if e.retryable && job.attempts < job.max_attempts => {
                let run_at = e.retry_at.unwrap_or_else(|| job.backoff(self.now()));
                (
                    None,
                    JobOutcome::Retry {
                        id: job.id,
                        owner,
                        error: e.to_json(),
                        run_at,
                    },
                    JobResult::Retrying { code: e.code },
                )
            }
            Err(e) => {
                let phase = match job.kind {
                    JobKind::DeployRun => DeployPhase::Deploy,
                    _ => self
                        .deploy_load(id)?
                        .and_then(|d| d.phase)
                        .unwrap_or(DeployPhase::Verify),
                };
                let (code, params) = exhausted(job.kind, phase, &e);
                (
                    Some(DeployWork::fail(id, phase, &code, params)),
                    JobOutcome::Dead {
                        id: job.id,
                        owner,
                        error: e.to_json(),
                    },
                    JobResult::Exhausted { code: e.code },
                )
            }
        };
        let (inputs, jobs) = work.map_or_else(|| (Vec::new(), Vec::new()), |w| (w.inputs, w.jobs));
        match self.deploy_commit(id, inputs, Some(outcome), jobs, &cause) {
            Ok(_) => Ok(JobReport {
                kind: job.kind,
                idempotency_key: job.idempotency_key.clone(),
                result,
            }),
            Err(EngineError::Store(saka_store::StoreError::LeaseLost { .. })) => Ok(JobReport {
                kind: job.kind,
                idempotency_key: job.idempotency_key.clone(),
                result: JobResult::LeaseLost,
            }),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn deploy_apply(
        &self,
        id: DeploymentId,
        inputs: Vec<Input>,
        outcome: Option<JobOutcome>,
        cause: &Cause,
    ) -> Result<Option<Deployment>> {
        self.deploy_commit(id, inputs, outcome, Vec::new(), cause)
    }

    pub(crate) fn deploy_commit(
        &self,
        id: DeploymentId,
        inputs: Vec<Input>,
        outcome: Option<JobOutcome>,
        jobs: Vec<Job>,
        cause: &Cause,
    ) -> Result<Option<Deployment>> {
        let _guard = self.locked()?;
        let now = self.now();
        let mut batch = Batch::new(now);
        batch.job_outcome = outcome;
        let Some(mut current) = self.deploy_load(id)? else {
            self.commit(&batch)?;
            return Ok(None);
        };
        let project = Some(current.project_id);
        for input in inputs {
            let events = match deploy::decide(&current, input) {
                Ok(events) => events,
                Err(rejection) => {
                    tracing::info!(deployment_id = %id, code = rejection.code(), "deploy.input.refused");
                    break;
                }
            };
            for event in &events {
                if let Event::DeploymentCheckFinished { check, .. } = event {
                    batch.deployment_checks.push((id, check.clone()));
                    self.counters
                        .deploy_check(check.phase.as_str(), check.state.as_str());
                }
                let event_id = self.push_event(&mut batch, event, project, cause, now);
                if matches!(
                    event,
                    Event::DeploymentSucceeded { .. } | Event::DeploymentFailed { .. }
                ) {
                    let after = deploy::evolve(Some(current.clone()), event, now);
                    if let Some(after) = after {
                        self.plan_deployment_hooks(&mut batch, &after, event_id, cause, now)?;
                        self.note_deployment_finished(&after);
                    }
                }
            }
            if let Some(next) = deploy::evolve_all(Some(current.clone()), &events, now) {
                current = next;
            }
        }
        for job in jobs {
            self.push_job(&mut batch, job, project, cause);
        }
        batch.deployments.push(current.clone());
        self.commit(&batch)?;
        Ok(Some(current))
    }

    fn note_deployment_finished(&self, d: &Deployment) {
        let outcome = d.state.as_str();
        self.counters.deployment(&d.environment, outcome);
        let duration_ms = d
            .started_at
            .zip(d.finished_at)
            .map_or(0, |(started, finished)| finished.since(started));
        let code = d.error.as_ref().map_or("", |e| e.code.as_str());
        tracing::info!(
            environment = %d.environment,
            outcome,
            duration_ms,
            code,
            "deploy.deployment.finished"
        );
    }

    fn plan_deployment_hooks(
        &self,
        batch: &mut Batch,
        d: &Deployment,
        source_event_id: uuid::Uuid,
        cause: &Cause,
        now: Timestamp,
    ) -> Result<()> {
        if self.cfg.hooks.is_empty() {
            return Ok(());
        }
        let on = if d.state == DeploymentState::Succeeded {
            HookEvent::Deployed
        } else {
            HookEvent::DeployFailed
        };
        let Some((facts, skip)) = self.hook_facts(d.project_id, Some(d.repo_id), None)? else {
            return Ok(());
        };
        let ctx = hook::deployment_context(d, &facts, on, now);
        self.queue_hooks(batch, &ctx, &skip, source_event_id, cause, now);
        Ok(())
    }

    pub(crate) fn recover_deployments(&self) -> Result<usize> {
        let unfinished = self.store.read(rows::unfinished)?;
        let mut failed = 0;
        for d in unfinished {
            let interrupted = d.state == DeploymentState::Deploying
                && d.action != saka_core::vocab::DeployActionKind::Webhook;
            if !interrupted {
                continue;
            }
            let cause = self.cause("system:recovery");
            self.deploy_apply(
                d.id,
                vec![Input::Fail {
                    phase: DeployPhase::Deploy,
                    error: Coded {
                        code: "deploy_interrupted".to_string(),
                        params: json!({}),
                    },
                }],
                None,
                &cause,
            )?;
            failed += 1;
        }
        self.skip_removed_gates()?;
        Ok(failed)
    }

    fn skip_removed_gates(&self) -> Result<()> {
        let open = self.store.read(rows::all_open_gates)?;
        let mut batch = Batch::new(self.now());
        let cause = self.cause("system:recovery");
        for (project_id, gate) in open {
            let Some(project) = self.store.read(|c| read::load_project(c, project_id))? else {
                continue;
            };
            if self
                .environments_of(&project)
                .iter()
                .any(|e| e.name == gate.environment)
            {
                continue;
            }
            self.skip_gate(&mut batch, &gate, &cause)?;
        }
        if !batch.events.is_empty() {
            self.commit(&batch)?;
        }
        Ok(())
    }
}

fn exhausted(kind: JobKind, phase: DeployPhase, error: &PortError) -> (String, Value) {
    match (kind, error.code.as_str()) {
        (JobKind::DeployRun, "hook_webhook_unavailable" | "deploy_webhook_unavailable") => (
            "deploy_webhook_unavailable".to_string(),
            json!({ "last": error.to_json() }),
        ),
        (JobKind::DeployRun, _) => (
            "deploy_environment_failed".to_string(),
            json!({ "error": error.to_json() }),
        ),
        (_, _) => (
            "verify_environment_failed".to_string(),
            json!({ "phase": phase, "error": error.to_json() }),
        ),
    }
}
