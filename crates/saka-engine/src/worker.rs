use saka_core::event::{Event, Prompt};
use saka_core::ids::{ChangeRequestId, EnvironmentId, SessionId, StageRunId, WorkItemId};
use saka_core::job::{Job, JobKind};
use saka_core::model::{Coded, Environment, Task};
use saka_core::task::TaskInput;
use saka_core::vocab::{ChangeState, EnvironmentState, PermissionState, Stage, StagePhase};
use saka_ports::{
    EnvRef, MergeOutcome, MergeRequest, OpenChangeRequest, PortError, PrepareRequest, PushOutcome,
    PushRequest, RepoRef, SessionLaunch, SessionRef,
};
use saka_store::{ClaimedJob, JobOutcome, LEASE_EXTEND_MS, StoreError, read};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tracing::Instrument;

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
    Nothing,
    Skipped,
    Later(Job),
}

pub type Execution = std::result::Result<Effect, PortError>;

fn field<T: DeserializeOwned>(payload: &Value, name: &str) -> std::result::Result<T, PortError> {
    payload
        .get(name)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| PortError::permanent("job_payload_invalid", json!({ "field": name })))
}

fn internal(e: EngineError) -> PortError {
    PortError::retryable("internal", json!({ "error": e.to_string() }))
}

impl Engine {
    pub async fn run_next_job(&self) -> Result<Option<JobReport>> {
        let Some(job) = self.claim_job()? else {
            return Ok(None);
        };
        let work_item = job
            .payload
            .get("work_item_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
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
        let id: WorkItemId = field(&job.payload, "work_item_id")?;
        let Some(task) = self
            .store
            .read(|c| read::load_task(c, id))
            .map_err(|e| internal(e.into()))?
        else {
            return Ok(Effect::Skipped);
        };
        let p = &job.payload;
        match job.kind {
            JobKind::EnvPrepare => self.env_prepare(&task, field(p, "stage_run_id")?).await,
            JobKind::SessionStart => {
                self.session_start(&task, field(p, "stage_run_id")?, job.id)
                    .await
            }
            JobKind::SessionSendTurn => {
                self.send_turn(
                    &task,
                    field(p, "session_id")?,
                    field(p, "turn")?,
                    field(p, "prompt")?,
                )
                .await
            }
            JobKind::SessionDeliverPermissionAnswer => {
                self.deliver_answer(&task, field(p, "permission_request_id")?)
                    .await
            }
            JobKind::SessionInterrupt => {
                let id: SessionId = field(p, "session_id")?;
                let stopping = task.session.as_ref().filter(|s| {
                    s.id == id && s.interrupt_requested == Some(saka_core::vocab::InterruptBy::Saka)
                });
                let Some(session) = self.session_ref(&task, id).or_else(|| {
                    stopping.map(|s| SessionRef {
                        session_id: s.id,
                        runner_unit: s.runner_unit.clone(),
                        socket_path: s.socket_path.clone(),
                    })
                }) else {
                    return Ok(Effect::Skipped);
                };
                self.ports.agents.interrupt(&session).await?;
                Ok(Effect::Nothing)
            }
            JobKind::SessionSteer | JobKind::SessionReattach => Ok(Effect::Skipped),
            JobKind::SessionStop => {
                let session: SessionId = field(p, "session_id")?;
                let unit: Option<String> = p
                    .get("runner_unit")
                    .and_then(|u| u.as_str().map(String::from));
                self.ports.agents.stop(session, unit.as_deref()).await?;
                Ok(Effect::Nothing)
            }
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
        let cause = Cause {
            actor: format!("system:worker:{}", job.kind.as_str()),
            causation_id: Some(job.id),
            correlation_id: job.correlation_id,
        };
        let work_item: Option<WorkItemId> = field(&job.payload, "work_item_id").ok();
        let succeeded = || JobOutcome::Succeeded {
            id: job.id,
            owner: owner.clone(),
        };
        let (input, extra, result) = match execution {
            Ok(Effect::Fact(input)) => (
                Some(input),
                Extra {
                    outcome: Some(succeeded()),
                    ..Extra::default()
                },
                JobResult::Succeeded,
            ),
            Ok(Effect::Nothing) => (
                None,
                Extra {
                    outcome: Some(succeeded()),
                    ..Extra::default()
                },
                JobResult::Succeeded,
            ),
            Ok(Effect::Skipped) => (
                None,
                Extra {
                    outcome: Some(succeeded()),
                    ..Extra::default()
                },
                JobResult::Skipped,
            ),
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
                let last_error = Coded {
                    code: e.code.clone(),
                    params: e.params.clone(),
                };
                let exhausted = Event::JobExhausted {
                    job_id: job.id,
                    kind: job.kind,
                    last_error: last_error.clone(),
                };
                (
                    Some(TaskInput::JobExhausted {
                        kind: job.kind,
                        error: last_error,
                    }),
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
        let committed = match work_item {
            Some(id) => self
                .decide(id, input, Mode::Fact, &cause, extra)
                .map(|_| ()),
            None => self.commit_outcome_only(extra),
        };
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

    fn base_branch(&self, task: &Task) -> std::result::Result<String, PortError> {
        let project = self
            .store
            .read(|c| read::load_project(c, task.item.project_id))
            .map_err(|e| internal(e.into()))?
            .ok_or_else(|| PortError::permanent("project_not_found", json!({})))?;
        Ok(project.base_branch)
    }

    fn env_ref(&self, task: &Task, env: &Environment) -> std::result::Result<EnvRef, PortError> {
        Ok(EnvRef {
            environment_id: env.id,
            repo: self.repo_ref(task)?,
            branch: env.branch.clone(),
            root_path: env.root_path.clone(),
            worktree_path: env.worktree_path.clone(),
        })
    }

    fn session_ref(&self, task: &Task, id: SessionId) -> Option<SessionRef> {
        task.live_session()
            .filter(|s| s.id == id)
            .map(|s| SessionRef {
                session_id: s.id,
                runner_unit: s.runner_unit.clone(),
                socket_path: s.socket_path.clone(),
            })
    }

    async fn env_prepare(&self, task: &Task, run: StageRunId) -> Execution {
        let current = task.stage_run.as_ref().is_some_and(|r| r.id == run);
        if !current || task.ready_environment().is_some() {
            return Ok(Effect::Skipped);
        }
        // One environment per stage run: a repeated job finds the one it made.
        let environment_id = EnvironmentId(run.0);
        let prepared = self
            .ports
            .env
            .prepare(&PrepareRequest {
                environment_id,
                work_item_id: task.item.id,
                repo: self.repo_ref(task)?,
                branch: task.item.branch.clone(),
                base_branch: self.base_branch(task)?,
            })
            .await?;
        Ok(Effect::Fact(TaskInput::EnvironmentPrepared {
            environment_id,
            root_path: prepared.root_path,
            worktree_path: prepared.worktree_path,
            base_sha: prepared.base_sha,
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
            Some(previous) if previous.stage_run_id == run => SessionId(job),
            _ => SessionId(run.0),
        };
        let launched = self
            .ports
            .agents
            .start(&SessionLaunch {
                session_id,
                work_item_id: task.item.id,
                environment: self.env_ref(task, env)?,
            })
            .await?;
        Ok(Effect::Fact(TaskInput::SessionLaunched {
            session_id,
            cli: launched.cli,
            cli_version: launched.cli_version,
            model: launched.model,
            account: launched.account,
            runner_unit: launched.runner_unit,
            socket_path: launched.socket_path,
            log_path: launched.log_path,
        }))
    }

    async fn send_turn(
        &self,
        task: &Task,
        session: SessionId,
        turn: u32,
        prompt: Prompt,
    ) -> Execution {
        let Some(session) = self.session_ref(task, session) else {
            return Ok(Effect::Skipped);
        };
        let body = self.turn_text(task, &prompt).map_err(internal)?;
        self.ports.agents.send_turn(&session, turn, &body).await?;
        Ok(Effect::Fact(TaskInput::TurnAccepted { turn }))
    }

    async fn deliver_answer(
        &self,
        task: &Task,
        id: saka_core::ids::PermissionRequestId,
    ) -> Execution {
        let Some(request) = task.permissions.iter().find(|p| {
            p.id == id
                && p.delivered_at.is_none()
                && matches!(p.state, PermissionState::Allowed | PermissionState::Denied)
        }) else {
            return Ok(Effect::Skipped);
        };
        let Some(session) = self.session_ref(task, request.session_id) else {
            return Ok(Effect::Skipped);
        };
        self.ports
            .agents
            .answer_permission(
                &session,
                &request.cli_request_id,
                request.state == PermissionState::Allowed,
                request.message.as_deref(),
            )
            .await?;
        Ok(Effect::Nothing)
    }

    async fn inspect(&self, task: &Task, session: SessionId, turn: u32) -> Execution {
        let (Some(_), Some(env)) = (self.session_ref(task, session), task.ready_environment())
        else {
            return Ok(Effect::Skipped);
        };
        let base = env.base_sha.clone().unwrap_or_default();
        let state = self
            .ports
            .env
            .inspect(&self.env_ref(task, env)?, &base)
            .await?;
        Ok(Effect::Fact(TaskInput::WorktreeInspected {
            turn,
            dirty: state.dirty,
            commits_ahead: state.commits_ahead,
            head_sha: state.head_sha,
        }))
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
        let in_merge = task.is_at(Stage::Merge, StagePhase::AwaitingCi)
            || task.is_at(Stage::Merge, StagePhase::AwaitingMerge);
        let Some(change) = task
            .change_request
            .as_ref()
            .filter(|c| c.id == cr && in_merge)
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
        let found = match &change.merge_commit_sha {
            Some(sha) => {
                self.ports
                    .env
                    .is_merged(&self.repo_ref(task)?, sha, &change.base_branch)
                    .await?
            }
            None => false,
        };
        if found || round >= VERIFY_ROUNDS {
            return Ok(Effect::Fact(TaskInput::MergeVerified { found }));
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
        self.ports.env.teardown(&self.env_ref(task, env)?).await?;
        Ok(Effect::Fact(TaskInput::EnvironmentTornDown))
    }
}
