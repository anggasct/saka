use saka_core::event::Event;
use saka_core::hook::{self, Facts, Hook, HookAction, HookContext, HookRun, RepoRef};
use saka_core::ids::{HookRunId, ProjectId, RepoId};
use saka_core::job::{Job, JobKind};
use saka_core::model::{Coded, Spec, Task};
use saka_core::secret::SecretScope;
use saka_core::time::Timestamp;
use saka_core::vocab::{BlockerCode, HookMode, HookRunState, Stage, StagePhase};
use saka_ports::{
    IdsSource, Notification, PortError, PortResult, Sealed, WebhookPost, WebhookReceipt,
};
use saka_store::hook_run::HookRunFilter;
use saka_store::list::Page;
use saka_store::{Batch, ClaimedJob, JobOutcome, read};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::action::{Action, Produced};
use crate::commands::reason;
use crate::views::Paged;
use crate::worker::{Effect, Execution, JobReport, JobResult, field};
use crate::{Cause, Engine, EngineError, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct HookReport {
    pub outcome: HookRunState,
    pub duration_ms: i64,
    pub result: Value,
    pub error: Option<Coded>,
}

pub(crate) fn empty_result() -> Value {
    json!({
        "exit_code": null,
        "timed_out": false,
        "http_status": null,
        "log": null,
        "output_tail": null,
        "skipped": null,
    })
}

pub(crate) fn failed(code: &str, params: Value, result: Value, duration_ms: i64) -> HookReport {
    HookReport {
        outcome: HookRunState::Failed,
        duration_ms,
        result,
        error: Some(Coded {
            code: code.to_string(),
            params,
        }),
    }
}

impl Engine {
    pub(crate) fn plan_task_hooks(
        &self,
        batch: &mut Batch,
        task: &Task,
        event: &Event,
        event_id: Uuid,
        cause: &Cause,
        now: Timestamp,
    ) -> Result<()> {
        if let Event::StageRunCreated {
            stage: Stage::Sync,
            entry_phase: StagePhase::TearingDown,
            retry_of: Some(retried),
            ..
        } = event
            && retried.code == BlockerCode::HookFailed.as_str()
        {
            self.rerun_failed_gates(batch, task, cause, now)?;
        }
        if self.cfg.hooks.is_empty() {
            return Ok(());
        }
        let item = &task.item;
        let Some((facts, skip)) =
            self.hook_facts(item.project_id, Some(item.repo_id), item.spec_id)?
        else {
            return Ok(());
        };
        let Some(ctx) = hook::task_context(event, task, &facts, now) else {
            return Ok(());
        };
        self.queue_hooks(batch, &ctx, &skip, event_id, cause, now);
        Ok(())
    }

    fn rerun_failed_gates(
        &self,
        batch: &mut Batch,
        task: &Task,
        cause: &Cause,
        now: Timestamp,
    ) -> Result<()> {
        let Some(project) = self
            .store
            .read(|c| read::load_project(c, task.item.project_id))?
        else {
            return Ok(());
        };
        for gate in task
            .hook_gates
            .iter()
            .filter(|g| g.state == HookRunState::Failed)
        {
            let Some(run) = self
                .store
                .read(|c| saka_store::hook_run::load(c, gate.run_id))?
            else {
                continue;
            };
            let current = self.cfg.hooks.iter().find(|h| {
                h.name == run.hook
                    && h.mode == run.mode
                    && h.project.selects(project.id, &project.name)
            });
            let mut ids = IdsSource(&*self.ports.ids);
            match current.map(|h| hook::rerun(&run, Some(h), &mut ids)) {
                Some(Ok(event)) => self.queue_hook_event(batch, event, cause, now),
                _ => {
                    let mut row = None;
                    for event in hook::skipped(&run, &mut ids) {
                        row = hook::evolve(row, &event, now);
                        self.push_event(batch, &event, Some(run.project_id), cause, now);
                    }
                    batch.hook_runs.extend(row);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn plan_spec_hooks(
        &self,
        batch: &mut Batch,
        spec: &Spec,
        event_id: Uuid,
        cause: &Cause,
        now: Timestamp,
    ) -> Result<()> {
        if self.cfg.hooks.is_empty() {
            return Ok(());
        }
        let Some(project) = self
            .store
            .read(|c| read::load_project(c, spec.project_id))?
        else {
            return Ok(());
        };
        let ctx = hook::spec_context(spec, &project.name, now);
        self.queue_hooks(batch, &ctx, &project.hook_skip, event_id, cause, now);
        Ok(())
    }

    fn hook_facts(
        &self,
        project_id: ProjectId,
        repo_id: Option<RepoId>,
        spec_id: Option<saka_core::ids::SpecId>,
    ) -> Result<Option<(Facts, Vec<String>)>> {
        let loaded = self.store.read(|c| {
            let Some(project) = read::load_project(c, project_id)? else {
                return Ok(None);
            };
            let repo = match repo_id {
                Some(id) => read::load_repo(c, id)?,
                None => None,
            };
            let spec = match spec_id {
                Some(id) => read::load_spec(c, id)?,
                None => None,
            };
            Ok(Some((project, repo, spec)))
        })?;
        Ok(loaded.map(|(project, repo, spec)| {
            let facts = Facts {
                project_id,
                project_name: project.name.clone(),
                repo: repo.and_then(|r| {
                    project
                        .repos
                        .iter()
                        .find(|b| b.repo_id == r.id)
                        .map(|b| RepoRef {
                            id: r.id,
                            name: b.name.clone(),
                            full_name: r.full_name.clone(),
                        })
                }),
                spec_path: spec.map(|s| s.path),
            };
            (facts, project.hook_skip)
        }))
    }

    pub(crate) fn queue_hooks(
        &self,
        batch: &mut Batch,
        ctx: &HookContext,
        skip: &[String],
        source_event_id: Uuid,
        cause: &Cause,
        now: Timestamp,
    ) {
        let mut ids = IdsSource(&*self.ports.ids);
        let hooks: Vec<Hook> = self
            .cfg
            .hooks
            .iter()
            .filter(|h| !(h.mode == HookMode::After && skip.contains(&h.name)))
            .cloned()
            .collect();
        let events = hook::plan(&hooks, ctx, source_event_id, &mut ids);
        for event in events {
            self.queue_hook_event(batch, event, cause, now);
        }
    }

    fn queue_hook_event(&self, batch: &mut Batch, event: Event, cause: &Cause, now: Timestamp) {
        let Event::HookRunQueued {
            hook_run_id,
            project_id,
            hook,
            action,
            trigger,
            ..
        } = &event
        else {
            return;
        };
        tracing::info!(hook = %hook, action = action.as_str(), trigger = trigger.as_str(), hook_run_id = %hook_run_id, "hook.run.queued");
        let job = Job::new(
            JobKind::HookRun,
            hook_run_id,
            json!({ "hook_run_id": hook_run_id }),
            now,
        );
        let project = Some(*project_id);
        batch.hook_runs.extend(hook::evolve(None, &event, now));
        self.push_event(batch, &event, project, cause, now);
        self.push_job(batch, job, project, cause);
    }

    pub(crate) fn checked_hook_skip(
        &self,
        project: Option<ProjectId>,
        config: &crate::project_config::ProjectConfig,
    ) -> std::result::Result<Vec<String>, Vec<crate::project_config::Issue>> {
        let id = project.unwrap_or(ProjectId(Uuid::nil()));
        let issues: Vec<_> = config
            .hook_skip
            .iter()
            .filter(|skip| {
                !self.cfg.hooks.iter().any(|h| {
                    h.name == skip.name
                        && h.mode == saka_core::vocab::HookMode::After
                        && h.project.selects(id, &config.name)
                })
            })
            .map(|skip| crate::project_config::Issue {
                code: "hook_skip_invalid",
                field: Some(format!("hooks.skip[{}]", skip.index)),
                line: Some(skip.line),
            })
            .collect();
        if issues.is_empty() {
            Ok(config.hook_skip.iter().map(|s| s.name.clone()).collect())
        } else {
            Err(issues)
        }
    }

    pub fn project_hooks(&self, project: ProjectId) -> Result<Vec<Hook>> {
        let project =
            self.store
                .read(|c| read::load_project(c, project))?
                .ok_or(EngineError::Rejected(
                    saka_core::rejection::Rejection::NotFound,
                ))?;
        Ok(self
            .cfg
            .hooks
            .iter()
            .filter(|h| h.project.selects(project.id, &project.name))
            .cloned()
            .collect())
    }

    pub fn hook_run(&self, id: HookRunId) -> Result<HookRun> {
        self.store
            .read(|c| saka_store::hook_run::load(c, id))?
            .ok_or(EngineError::Rejected(
                saka_core::rejection::Rejection::NotFound,
            ))
    }

    pub fn hook_runs(&self, filter: &HookRunFilter, page: &Page) -> Result<Paged<HookRun>> {
        let rows = self.store.read(|c| {
            saka_store::hook_run::list(
                c,
                filter,
                &Page {
                    after: page.after,
                    limit: page.limit.saturating_add(1),
                },
            )
        })?;
        let paged = crate::views::paged(rows, page, |(_, key)| *key);
        Ok(Paged {
            items: paged.items.into_iter().map(|(run, _)| run).collect(),
            next: paged.next,
        })
    }

    pub fn rerun_hook(
        &self,
        id: HookRunId,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<HookRun> {
        reason(why)?;
        let _guard = self.locked()?;
        let now = self.now();
        let run = self
            .store
            .read(|c| saka_store::hook_run::load(c, id))?
            .ok_or(EngineError::Rejected(
                saka_core::rejection::Rejection::NotFound,
            ))?;
        let project = self
            .store
            .read(|c| read::load_project(c, run.project_id))?
            .ok_or(EngineError::Rejected(
                saka_core::rejection::Rejection::NotFound,
            ))?;
        let current = self
            .cfg
            .hooks
            .iter()
            .find(|h| h.name == run.hook && h.project.selects(project.id, &project.name));
        if run.mode == HookMode::Gate
            && run.state != HookRunState::Pending
            && let Some(work_item_id) = run.work_item_id
        {
            let holding = self
                .store
                .read(|c| read::load_task(c, work_item_id))?
                .is_some_and(|t| !t.item.state.is_terminal());
            if holding {
                return Err(EngineError::Rejected(
                    saka_core::rejection::Rejection::HookRunGated {
                        work_item_id: work_item_id.to_string(),
                    },
                ));
            }
        }
        let mut ids = IdsSource(&*self.ports.ids);
        let event = hook::rerun(&run, current, &mut ids)?;
        let mut batch = Batch::new(now);
        self.queue_hook_event(&mut batch, event, cause, now);
        let created = batch
            .hook_runs
            .first()
            .cloned()
            .ok_or_else(|| EngineError::Internal("rerun queued nothing".into()))?;
        if let Some(a) = action {
            let reply = (a.respond)(Produced::HookRun(&created));
            batch.action =
                Some(a.record(self, cause, Some(run.project_id), Some(created.id.0), reply));
        }
        self.commit(&batch)?;
        Ok(created)
    }

    pub(crate) async fn hook_job(&self, job: &ClaimedJob) -> Execution {
        let id: HookRunId = field(&job.payload, "hook_run_id")?;
        let Some(run) = self
            .store
            .read(|c| saka_store::hook_run::load(c, id))
            .map_err(|e| crate::worker::internal(e.into()))?
        else {
            return Ok(Effect::Skipped);
        };
        if run.state != HookRunState::Pending {
            return Ok(Effect::Skipped);
        }
        let started = self.now();
        let hook = self.cfg.hooks.iter().find(|h| {
            h.name == run.hook && h.project.selects(run.project_id, &run.context.project.name)
        });
        let report = match hook.map(|h| (h, &h.action)) {
            None => failed(
                "hook_not_configured",
                json!({ "hook": run.hook }),
                empty_result(),
                0,
            ),
            Some((hook, HookAction::Webhook { url, secret })) => {
                self.hook_webhook(&run, hook, url, secret).await?
            }
            Some((hook, HookAction::Notify { channel })) => {
                self.hook_notify(&run, hook, channel).await?
            }
            Some((hook, HookAction::Run { command, secrets })) => {
                self.hook_in_environment(&run, hook, command, secrets)
                    .await?
            }
            Some((hook, HookAction::RunHost { command, cwd })) => {
                self.hook_on_host(&run, hook, command, cwd.as_deref())
                    .await?
            }
        };
        let duration_ms = match report.duration_ms {
            0 => self.now().since(started),
            known => known,
        };
        Ok(Effect::Hook(HookReport {
            duration_ms,
            ..report
        }))
    }

    fn hook_secret(
        &self,
        run: &HookRun,
        name: &str,
    ) -> PortResult<Option<saka_core::secret::SecretText>> {
        let rows = self
            .store
            .read(|c| read::project_secrets(c, run.project_id))
            .map_err(|e| {
                PortError::retryable("store_unavailable", json!({ "detail": e.to_string() }))
            })?;
        let Some(row) = rows.into_iter().find(|r| r.name == name) else {
            return Ok(None);
        };
        let sealed = Sealed {
            nonce: row.nonce,
            ciphertext: row.ciphertext,
        };
        let scope = SecretScope::Project {
            project_id: run.project_id,
            name,
        };
        self.ports.secrets.open(&scope, &sealed).map(Some)
    }

    async fn hook_webhook(
        &self,
        run: &HookRun,
        hook: &Hook,
        url: &str,
        secret_name: &str,
    ) -> std::result::Result<HookReport, PortError> {
        let secret = match self.hook_secret(run, secret_name) {
            Ok(Some(secret)) => secret,
            Ok(None) => {
                return Ok(failed(
                    "hook_secret_missing",
                    json!({ "name": secret_name }),
                    empty_result(),
                    0,
                ));
            }
            Err(e) if e.retryable => return Err(e),
            Err(_) => {
                return Ok(failed(
                    "hook_secret_unreadable",
                    json!({ "name": secret_name }),
                    empty_result(),
                    0,
                ));
            }
        };
        let body = serde_json::to_vec(&run.context.payload(run.id, &run.hook)).map_err(|e| {
            PortError::permanent("hook_payload_invalid", json!({ "detail": e.to_string() }))
        })?;
        let post = WebhookPost {
            url: url.to_string(),
            event: run.trigger.as_str().to_string(),
            delivery_id: run.id.to_string(),
            body,
            secret,
            timeout_ms: hook.timeout_ms,
            timestamp_secs: self.now().0.div_euclid(1000),
        };
        match self.ports.webhooks.post(&post).await {
            Ok(WebhookReceipt { status }) => {
                let mut result = empty_result();
                result["http_status"] = json!(status);
                Ok(HookReport {
                    outcome: HookRunState::Succeeded,
                    duration_ms: 0,
                    result,
                    error: None,
                })
            }
            Err(e) if e.retryable => Err(e),
            Err(e) => {
                let mut result = empty_result();
                result["http_status"] = e.params.get("status").cloned().unwrap_or(Value::Null);
                Ok(failed(&e.code, e.params, result, 0))
            }
        }
    }

    async fn hook_notify(
        &self,
        run: &HookRun,
        hook: &Hook,
        channel: &str,
    ) -> std::result::Result<HookReport, PortError> {
        let notification = Notification {
            channel: channel.to_string(),
            code: "hook.event".to_string(),
            params: run.context.payload(run.id, &hook.name),
            dedupe_key: run.id.to_string(),
        };
        match self.ports.notifier.send(&notification).await {
            Ok(()) => Ok(HookReport {
                outcome: HookRunState::Succeeded,
                duration_ms: 0,
                result: empty_result(),
                error: None,
            }),
            Err(e) if e.retryable => Err(e),
            Err(e) => Ok(failed(&e.code, e.params, empty_result(), 0)),
        }
    }

    pub(crate) fn complete_hook_job(
        &self,
        job: &ClaimedJob,
        execution: Execution,
    ) -> Result<JobReport> {
        let _guard = self.locked()?;
        let now = self.now();
        let owner = self.cfg.worker_id.clone();
        let cause = Cause {
            actor: format!("system:worker:{}", job.kind.as_str()),
            causation_id: Some(job.id),
            correlation_id: job.correlation_id,
        };
        let mut batch = Batch::new(now);
        let succeeded = JobOutcome::Succeeded {
            id: job.id,
            owner: owner.clone(),
        };
        let id: HookRunId = field(&job.payload, "hook_run_id")
            .map_err(|e| EngineError::Internal(format!("hook job without a run: {}", e.code)))?;
        let (report, outcome, result) = match execution {
            Ok(Effect::Hook(report)) => (Some(report), succeeded, JobResult::Succeeded),
            Ok(_) => (None, succeeded, JobResult::Skipped),
            Err(e) if e.retryable && job.attempts < job.max_attempts => {
                let run_at = e.retry_at.unwrap_or_else(|| job.backoff(now));
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
                let params = json!({ "last": { "code": e.code, "params": e.params } });
                let report = failed("retries_exhausted", params, empty_result(), 0);
                (
                    Some(report),
                    JobOutcome::Dead {
                        id: job.id,
                        owner,
                        error: e.to_json(),
                    },
                    JobResult::Exhausted { code: e.code },
                )
            }
        };
        batch.job_outcome = Some(outcome);
        if let Some(report) = report {
            let run = self
                .store
                .read(|c| saka_store::hook_run::load(c, id))?
                .ok_or(EngineError::Rejected(
                    saka_core::rejection::Rejection::NotFound,
                ))?;
            if let Some(event) = hook::finished(
                &run,
                report.outcome,
                job.attempts,
                report.duration_ms,
                report.result,
                report.error,
            ) {
                batch
                    .hook_runs
                    .extend(hook::evolve(Some(run.clone()), &event, now));
                self.push_event(&mut batch, &event, Some(run.project_id), &cause, now);
                self.log_hook_finished(&run, &event);
            }
        }
        match self.commit(&batch) {
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

    fn log_hook_finished(&self, run: &HookRun, event: &Event) {
        let Event::HookRunFinished {
            outcome,
            attempts,
            duration_ms,
            error,
            ..
        } = event
        else {
            return;
        };
        self.counters
            .hook_run(run.action.as_str(), run.mode.as_str(), outcome.as_str());
        let code = error.as_ref().map_or("", |e| e.code.as_str());
        if *outcome == HookRunState::Failed {
            tracing::warn!(hook = %run.hook, action = run.action.as_str(), outcome = outcome.as_str(), attempts, duration_ms, code, "hook.run.finished");
        } else {
            tracing::info!(hook = %run.hook, action = run.action.as_str(), outcome = outcome.as_str(), attempts, duration_ms, code, "hook.run.finished");
        }
    }
}
