use saka_core::gate::{Check, GatePolicy};
use saka_core::ids::StageRunId;
use saka_core::model::Task;
use saka_core::task::TaskInput;
use saka_core::vocab::{CheckConclusion, CheckKind, CheckStatus, Stage, StagePhase};
use saka_ports::{PortError, RecipeCommand, RecipePhase, StatusReport, StatusState};
use serde_json::{Value, json};

use super::{Effect, Execution, Refusal, check_name, field, internal};
use crate::{Engine, Extra, Mode};

impl Engine {
    pub(super) async fn gate_verify(
        &self,
        task: &Task,
        run: StageRunId,
        head: String,
    ) -> Execution {
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
        let mut regression_commands = Vec::new();
        if task.is_bugfix() {
            regression_commands = pinned
                .regression()
                .iter()
                .filter(|c| !step_commands.contains(c))
                .cloned()
                .collect();
            if let (true, Some(reproduced)) = (in_gate, task.reproduced_regression()) {
                let repo = self.repo_ref(task)?;
                let present = self
                    .ports
                    .env
                    .is_ancestor(&repo, &reproduced.commit_sha, &head)
                    .await?
                    || self
                        .ports
                        .env
                        .carries_patch(&repo, &reproduced.commit_sha, &head)
                        .await?;
                checks.push(Check {
                    name: saka_core::regression::COMMIT_CHECK.to_string(),
                    kind: CheckKind::Command,
                    status: CheckStatus::Completed,
                    conclusion: Some(if present {
                        CheckConclusion::Success
                    } else {
                        CheckConclusion::Failure
                    }),
                    url: None,
                });
                if !present {
                    failure = Some(json!({
                        "reason": "regression_commit_missing",
                        "commit": reproduced.commit_sha,
                    }));
                }
            }
        }
        let commands: Vec<&String> = if failure.is_some() {
            Vec::new()
        } else {
            step_commands
                .iter()
                .chain(&regression_commands)
                .chain(&policy.commands)
                .collect()
        };
        for (index, command) in commands.into_iter().enumerate() {
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
        let after = self.ports.env.inspect(&env_ref, &base).await?;
        if after.head_sha != head {
            let current = self.load_task(task.item.id).map_err(internal)?;
            if saka_core::task::gate_head(&current) == Some(head.as_str()) {
                return Err(PortError::retryable(
                    "worktree_moved",
                    json!({ "head_sha": head, "found_sha": after.head_sha }),
                ));
            }
            return Ok(Effect::Skipped);
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

    pub(super) async fn report_status(&self, task: &Task, p: &Value) -> Execution {
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
}
