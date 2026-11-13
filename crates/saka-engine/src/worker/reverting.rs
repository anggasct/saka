use std::collections::BTreeMap;
use std::time::Instant;

use saka_core::ids::{EnvironmentId, StageRunId};
use saka_core::model::Task;
use saka_core::task::{RevertOutcome, TaskInput};
use saka_core::vocab::{ChangeRole, ChangeState, StagePhase, StageRunState, WorkItemState};
use saka_ports::{Checkout, EnvRef, PortError, PrepareRequest, RevertRequest, Reverted};
use saka_store::read;
use serde_json::{Value, json};

use super::{Effect, Execution, internal};
use crate::Engine;

impl Engine {
    pub(crate) fn origin_of(&self, task: &Task) -> std::result::Result<Option<Task>, PortError> {
        let Some(revert) = &task.revert else {
            return Ok(None);
        };
        self.store
            .read(|c| read::load_task(c, revert.of, &self.cfg.workflow_policy))
            .map_err(|e| internal(e.into()))
    }

    pub(super) async fn revert_job(&self, task: &Task, run: StageRunId) -> Execution {
        let current = task.item.state == WorkItemState::Admitted
            && task.stage_run.as_ref().is_some_and(|r| {
                r.id == run
                    && r.entry_phase == StagePhase::Reverting
                    && r.state == StageRunState::Pending
            });
        if !current {
            return Ok(Effect::Skipped);
        }
        let origin = self.origin_of(task)?.ok_or_else(|| {
            PortError::permanent(
                "revert_origin_missing",
                json!({ "work_item_id": task.item.id }),
            )
        })?;
        let merged = origin
            .change_request
            .as_ref()
            .filter(|cr| cr.role == ChangeRole::Code && cr.state == ChangeState::Merged)
            .and_then(|cr| cr.merge_commit_sha.clone().map(|sha| (cr, sha)))
            .ok_or_else(|| {
                PortError::permanent(
                    "revert_source_missing",
                    json!({ "work_item_id": origin.item.id }),
                )
            })?;
        let (change, merge_commit) = merged;
        let author = match self.session_author(task)? {
            Ok(resolved) => resolved,
            Err(block) => {
                return Ok(Effect::Fact(TaskInput::CredentialBlocked {
                    block,
                    target: ChangeRole::Code,
                }));
            }
        };
        let started = Instant::now();
        tracing::info!(work_item_id = %task.item.id, origin = %origin.item.id, "delivery.revert.started");
        let repo = self.repo_ref(task)?;
        let environment_id = task
            .environment
            .as_ref()
            .map_or(EnvironmentId(run.0), |e| e.id);
        if let Some(stale) = task.ready_environment() {
            let env = self.env_ref(task, stale)?;
            self.ports.env.teardown(&env).await?;
        }
        let prepared = self
            .ports
            .env
            .prepare(&PrepareRequest {
                environment_id,
                repo: repo.clone(),
                checkout: Checkout::Branch(task.item.branch.clone()),
                base_branch: change.base_branch.clone(),
            })
            .await?;
        let env = EnvRef {
            environment_id,
            repo,
            branch: Some(task.item.branch.clone()),
            root_path: prepared.root_path.clone(),
            worktree_path: prepared.worktree_path.clone(),
            vars: BTreeMap::new(),
            path_prefix: Vec::new(),
            secrets: Vec::new(),
            extra_dirs: Vec::new(),
        };
        let reason = task
            .revert
            .as_ref()
            .map(|r| r.reason.as_str())
            .unwrap_or_default();
        let request = RevertRequest {
            base_sha: prepared.base_sha.clone(),
            merge_commit: merge_commit.clone(),
            head_sha: Some(change.head_sha.clone()),
            change_number: change.number,
            message: format!(
                "{}\n\nThis reverts {merge_commit} from {}.\n\nReason: {reason}\n",
                task.item.work, change.url
            ),
            author: author.identity,
            committer: self.cfg.git_identity.clone(),
        };
        let outcome = match self.ports.env.revert(&env, &request).await? {
            Reverted::Committed { head_sha } => RevertOutcome::Committed { head_sha },
            Reverted::NotInBase => blocked("not_in_base", &merge_commit, &change.base_branch),
            Reverted::CommitsUnknown => {
                blocked("commits_unknown", &merge_commit, &change.base_branch)
            }
            Reverted::Empty => blocked("empty", &merge_commit, &change.base_branch),
            Reverted::Conflict { paths, count } => RevertOutcome::Blocked {
                params: json!({
                    "reason": "conflict",
                    "merge_commit_sha": merge_commit,
                    "base_branch": change.base_branch,
                    "paths": paths,
                    "count": count,
                }),
            },
        };
        tracing::info!(
            work_item_id = %task.item.id,
            duration_ms = started.elapsed().as_millis() as u64,
            committed = matches!(outcome, RevertOutcome::Committed { .. }),
            "delivery.revert.finished"
        );
        Ok(Effect::Fact(TaskInput::RevertAttempted {
            environment_id,
            root_path: prepared.root_path,
            worktree_path: prepared.worktree_path,
            base_sha: prepared.base_sha,
            outcome,
        }))
    }
}

fn blocked(reason: &str, merge_commit: &str, base_branch: &str) -> RevertOutcome {
    let params: Value = json!({
        "reason": reason,
        "merge_commit_sha": merge_commit,
        "base_branch": base_branch,
    });
    RevertOutcome::Blocked { params }
}
