use saka_core::job::{Job, JobKind};
use saka_core::model::Task;
use saka_core::task::{Integration, TaskInput};
use saka_core::vocab::{ChangeState, ScopeStage, Stage, StagePhase};
use saka_ports::{
    EnqueueOutcome, MergeOutcome, MergeRequest, PortError, PushOutcome, PushRequest, Rebase,
    RepoRef,
};
use saka_store::ClaimedJob;
use serde_json::{Value, json};

use super::{Effect, Execution, VERIFY_PAUSE_MS, VERIFY_ROUNDS};
use crate::{Engine, text};

impl Engine {
    pub(super) async fn integrate(&self, task: &Task) -> Execution {
        let Some(change) = task
            .change_request
            .as_ref()
            .filter(|_| task.is_at(Stage::Merge, StagePhase::Integrating))
        else {
            return Ok(Effect::Skipped);
        };
        let repo = self.repo_ref(task)?;
        let capabilities = self.ports.scm.capabilities(&repo.integration_id);
        let native = capabilities.as_ref().is_some_and(|c| c.merge_queue)
            && self
                .ports
                .scm
                .merge_queue(&repo, &change.base_branch)
                .await?;
        if native {
            return self.enqueue(task, repo).await;
        }
        if task.external.is_some() {
            let updatable = capabilities.is_some_and(|c| c.update_branch);
            return self.integrate_outside(task, repo, updatable).await;
        }
        let Some(env) = task.ready_environment() else {
            return Err(PortError::permanent(
                "environment_missing",
                json!({ "work_item_id": task.item.id }),
            ));
        };
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

    async fn integrate_outside(&self, task: &Task, repo: RepoRef, updatable: bool) -> Execution {
        let Some(change) = task.change_request.as_ref() else {
            return Ok(Effect::Skipped);
        };
        let head = change.head_sha.clone();
        let Some(probe) = self
            .ports
            .env
            .base_probe(&repo, &change.base_branch, &head)
            .await?
        else {
            return Err(PortError::permanent(
                "base_branch_missing",
                json!({ "base_branch": change.base_branch }),
            ));
        };
        let (outcome, label) = if probe.in_head {
            (
                Integration::Rebased {
                    from_sha: head.clone(),
                    head_sha: head,
                    base_sha: probe.base_sha,
                },
                "current",
            )
        } else {
            (
                Integration::Behind {
                    head_sha: head,
                    base_sha: probe.base_sha,
                    updatable,
                },
                "behind",
            )
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

    pub(super) async fn enqueue(&self, task: &Task, repo: RepoRef) -> Execution {
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
        if task.external.is_none()
            && let Some(violation) = self
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

    pub(super) async fn merge(&self, task: &Task, head_sha: String) -> Execution {
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
        if task.external.is_none()
            && let Some(violation) = self
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

    pub(super) async fn verify(&self, task: &Task, job: &ClaimedJob) -> Execution {
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
}
