use async_trait::async_trait;
use saka_core::event::Event;
use saka_core::gate::GatePolicy;
use saka_core::ids::{RegressionCheckId, SessionId};
use saka_core::model::{RegressionRun, Task};
use saka_core::regression;
use saka_core::rejection::Rejection;
use saka_core::scope::MAX_REPORTED_PATHS;
use saka_core::task::TaskInput;
use saka_core::vocab::{RegressionOutcome, Stage, StagePhase, WorkKind};
use saka_ports::{
    PortError, RecipeCommand, RecipePhase, RegressionRefusal, Regressions, Requested,
};

use super::{Effect, Execution, internal};
use crate::{Engine, EngineError, Extra, Mode};

pub const REGRESSION_ACTOR: &str = "agent:regression";

fn refusal(error: &EngineError) -> RegressionRefusal {
    match error {
        EngineError::Rejected(Rejection::RegressionNotAvailable { reason }) => match *reason {
            "already_reproduced" => RegressionRefusal::AlreadyReproduced,
            "pending" => RegressionRefusal::Pending,
            _ => RegressionRefusal::ToolUnavailable,
        },
        EngineError::Rejected(Rejection::RegressionLimitReached) => RegressionRefusal::LimitReached,
        EngineError::Rejected(Rejection::InvalidState { .. } | Rejection::NotFound) => {
            RegressionRefusal::SessionNotRunning
        }
        _ => RegressionRefusal::Unavailable,
    }
}

impl Engine {
    pub(super) async fn reset_branch(&self, task: &Task) -> Execution {
        let Some(env) = task
            .ready_environment()
            .filter(|_| task.needs_branch_reset())
        else {
            return Ok(Effect::Skipped);
        };
        let env_ref = self.env_ref(task, env)?;
        let base = env.base_sha.clone().unwrap_or_default();
        let state = self.ports.env.inspect(&env_ref, &base).await?;
        if state.head_sha != base || state.dirty {
            self.ports.env.reset_branch(&env_ref, &base).await?;
        }
        tracing::info!(
            work_item_id = %task.item.id,
            old_head_sha = %state.head_sha,
            base_sha = %base,
            "regression.branch.reset"
        );
        Ok(Effect::Fact(TaskInput::RegressionBranchReset {
            old_head_sha: state.head_sha,
            base_sha: base,
        }))
    }

    pub(super) async fn reproduce(&self, task: &Task, id: RegressionCheckId) -> Execution {
        let Some(check) = task
            .regressions
            .iter()
            .find(|c| c.id == id && c.awaiting_result())
        else {
            return Ok(Effect::Skipped);
        };
        let Some(env) = task.ready_environment() else {
            return Ok(Effect::Skipped);
        };
        let observed = |outcome: RegressionOutcome, checks: Vec<RegressionRun>, paths| {
            self.counters.regression_check(outcome.as_str());
            tracing::info!(
                work_item_id = %task.item.id,
                attempt = check.attempt,
                outcome = outcome.as_str(),
                "regression.check.observed"
            );
            Ok(Effect::Fact(TaskInput::RegressionObserved {
                regression_check_id: id,
                outcome,
                checks,
                paths,
            }))
        };
        let (project, _) = self.bound(task)?;
        let extra = project
            .workflow
            .kinds
            .get(&WorkKind::Bugfix)
            .map(|o| o.test_paths.clone())
            .unwrap_or_default();
        let repo = self.repo_ref(task)?;
        let changed = self
            .ports
            .env
            .changed_paths(&repo, &check.base_sha, &check.commit_sha, false)
            .await?;
        let outside = regression::non_test_paths(&extra, &changed.paths, changed.truncated);
        if !outside.is_empty() {
            let mut paths = outside;
            paths.truncate(MAX_REPORTED_PATHS);
            return observed(RegressionOutcome::InvalidPaths, Vec::new(), paths);
        }
        let mut shown = changed.paths;
        shown.truncate(MAX_REPORTED_PATHS);
        let env_ref = self
            .env_ref_with_secrets(task, env)
            .map_err(super::Refusal::into_error)?;
        let base = env.base_sha.clone().unwrap_or_default();
        let worktree = self.ports.env.inspect(&env_ref, &base).await?;
        if worktree.dirty || worktree.head_sha != check.commit_sha {
            return observed(RegressionOutcome::Error, Vec::new(), shown);
        }
        let commands = self.pinned(task).map_err(internal)?.regression().to_vec();
        let redactor = self.project_redactor(task.item.project_id)?;
        let policy = GatePolicy::from_recipe(env.recipe.as_ref());
        let per_command_ms = u64::from(policy.timeout_minutes) * 60_000;
        let label: String = check.commit_sha.chars().take(12).collect();
        let started = std::time::Instant::now();
        let mut runs = Vec::new();
        for (index, command) in commands.iter().enumerate() {
            let spent = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let remaining = regression::JOB_BUDGET_MS.saturating_sub(spent);
            if remaining == 0 {
                runs.push(RegressionRun {
                    command: command.clone(),
                    exit_code: None,
                    timed_out: true,
                    duration_ms: 0,
                    output_tail: String::new(),
                });
                break;
            }
            let run = RecipeCommand {
                phase: RecipePhase::Gate,
                index,
                command: command.clone(),
                run: Some(format!("regression-{label}")),
                timeout_ms: Some(per_command_ms.min(remaining)),
            };
            let outcome = self.ports.env.run_recipe_command(&env_ref, &run).await?;
            let timed_out = outcome.timed_out;
            runs.push(RegressionRun {
                command: command.clone(),
                exit_code: outcome.exit_code,
                timed_out,
                duration_ms: outcome.duration_ms,
                output_tail: saka_ports::output_tail(&redactor.text(&outcome.output_tail)),
            });
            if timed_out {
                break;
            }
        }
        observed(regression::outcome_of(&runs), runs, shown)
    }
}

#[async_trait]
impl Regressions for Engine {
    async fn request(&self, session: SessionId) -> Result<Requested, RegressionRefusal> {
        let unavailable = |e: &dyn std::fmt::Display| {
            tracing::warn!(session_id = %session, error = %e, "regression check failed");
            RegressionRefusal::Unavailable
        };
        let owner = self
            .store
            .read(|c| saka_store::read::session_owner(c, session))
            .map_err(|e| unavailable(&e))?
            .ok_or(RegressionRefusal::SessionNotRunning)?;
        let (Some(work_item_id), Some(run)) = (owner.work_item_id(), owner.stage_run_id()) else {
            return Err(RegressionRefusal::ToolUnavailable);
        };
        let task = self.load_task(work_item_id).map_err(|e| unavailable(&e))?;
        if !task.is_bugfix() {
            return Err(RegressionRefusal::ToolUnavailable);
        }
        if task.live_session().map(|s| s.id) != Some(session)
            || task.stage_run.as_ref().map(|r| r.id) != Some(run)
        {
            return Err(RegressionRefusal::SessionNotRunning);
        }
        if !task.is_at(Stage::Dev, StagePhase::RunningSession) {
            return Err(RegressionRefusal::ToolUnavailable);
        }
        let env = task
            .ready_environment()
            .ok_or(RegressionRefusal::Unavailable)?;
        let env_ref = self.env_ref(&task, env).map_err(|e| unavailable(&e.code))?;
        let base = env.base_sha.clone().ok_or(RegressionRefusal::Unavailable)?;
        let state = self
            .ports
            .env
            .inspect(&env_ref, &base)
            .await
            .map_err(|e: PortError| unavailable(&e.code))?;
        if state.dirty {
            return Ok(Requested::DirtyWorktree);
        }
        let commit_sha = state.head_sha;
        let input = TaskInput::RegressionRequested {
            regression_check_id: RegressionCheckId(self.new_id()),
            commit_sha: commit_sha.clone(),
            base_sha: base,
        };
        let cause = self.cause(REGRESSION_ACTOR);
        let events = self
            .decide(
                work_item_id,
                Some(input),
                Mode::Command,
                &cause,
                Extra::default(),
            )
            .map_err(|e| {
                let refused = refusal(&e);
                if refused == RegressionRefusal::Unavailable {
                    tracing::warn!(session_id = %session, error = %e, "regression check failed");
                }
                refused
            })?;
        let attempt = match events.first() {
            Some(Event::RegressionRequested { attempt, .. }) => *attempt,
            _ => self
                .load_task(work_item_id)
                .ok()
                .and_then(|t| {
                    t.run_regressions()
                        .find(|c| c.awaiting_result() && c.commit_sha == commit_sha)
                        .map(|c| c.attempt)
                })
                .ok_or(RegressionRefusal::Unavailable)?,
        };
        Ok(Requested::Recorded {
            attempt,
            commit_sha,
        })
    }
}
