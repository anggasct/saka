use saka_core::job::{Job, JobKind};
use saka_core::model::Task;
use saka_core::task::{BranchUpdate, TaskInput};
use saka_core::vocab::{ChangeState, Stage, StagePhase};
use saka_ports::{RepoRef, UpdateOutcome};
use saka_store::ClaimedJob;
use serde_json::{Value, json};

use super::{Effect, Execution, field};
use crate::{Engine, text};

const UPDATE_ROUNDS: u32 = 10;
const UPDATE_PAUSE_MS: i64 = 3_000;

impl Engine {
    pub(super) async fn update_branch(&self, task: &Task, job: &ClaimedJob) -> Execution {
        let expected: String = field(&job.payload, "expected_head_sha")?;
        let Some(change) = task
            .change_request
            .as_ref()
            .filter(|_| task.is_at(Stage::Merge, StagePhase::AwaitingCi))
        else {
            return Ok(Effect::Skipped);
        };
        let round: u32 = job
            .payload
            .get("round")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(1);
        let repo = self.repo_ref(task)?;
        let settled = |outcome| {
            Ok(Effect::Fact(TaskInput::BranchUpdated {
                expected_head_sha: expected.clone(),
                outcome,
            }))
        };
        let snapshot = self.ports.scm.change_request(&repo, change.number).await?;
        if snapshot.state != ChangeState::Open {
            return Ok(Effect::Skipped);
        }
        if snapshot.head_sha != expected {
            return settled(
                self.updated_head(&repo, &change.base_branch, &expected, snapshot.head_sha)
                    .await?,
            );
        }
        if round == 1 {
            match self
                .ports
                .scm
                .update_branch(&repo, change.number, &expected)
                .await?
            {
                UpdateOutcome::HeadChanged => return settled(BranchUpdate::HeadChanged),
                UpdateOutcome::Conflict => {
                    let base_sha = self
                        .ports
                        .env
                        .branch_tip(&repo, &change.base_branch)
                        .await?
                        .unwrap_or_default();
                    return settled(BranchUpdate::Conflict {
                        head_sha: expected.clone(),
                        base_sha,
                    });
                }
                UpdateOutcome::Accepted => {
                    let after = self.ports.scm.change_request(&repo, change.number).await?;
                    if after.head_sha != expected {
                        return settled(
                            self.updated_head(
                                &repo,
                                &change.base_branch,
                                &expected,
                                after.head_sha,
                            )
                            .await?,
                        );
                    }
                }
            }
        }
        if round >= UPDATE_ROUNDS {
            return settled(BranchUpdate::Unchanged);
        }
        let mut payload = job.payload.clone();
        if let Value::Object(map) = &mut payload {
            map.insert("round".into(), json!(round + 1));
        }
        Ok(Effect::Later(Job::new(
            JobKind::ScmUpdateBranch,
            format!(
                "{}:{expected}:{}",
                task.stage_run
                    .as_ref()
                    .map_or_else(String::new, |r| r.id.to_string()),
                round + 1
            ),
            payload,
            self.now().plus_ms(UPDATE_PAUSE_MS),
        )))
    }

    async fn updated_head(
        &self,
        repo: &RepoRef,
        base_branch: &str,
        expected: &str,
        head_sha: String,
    ) -> Result<BranchUpdate, saka_ports::PortError> {
        let base = self
            .ports
            .env
            .base_probe(repo, base_branch, &head_sha)
            .await?;
        let parents = self.ports.env.commit_parents(repo, &head_sha).await?;
        let proven = match (base, parents) {
            (Some(base), Some(parents)) => {
                parents.len() == 2
                    && parents.iter().any(|p| p == expected)
                    && parents.contains(&base.base_sha)
            }
            _ => false,
        };
        Ok(if proven {
            BranchUpdate::Updated { head_sha }
        } else {
            BranchUpdate::Unexpected { head_sha }
        })
    }

    pub(super) async fn post_comment(&self, task: &Task, payload: &Value) -> Execution {
        let head_sha: String = field(payload, "head_sha")?;
        let code: String = field(payload, "code")?;
        let number: u64 = field(payload, "number")?;
        let params = payload.get("params").cloned().unwrap_or(Value::Null);
        let current = task
            .change_request
            .as_ref()
            .is_some_and(|c| c.state == ChangeState::Open && c.head_sha == head_sha);
        if !current {
            return Ok(Effect::Skipped);
        }
        let repo = self.repo_ref_of(field(payload, "repo_id")?)?;
        self.ports
            .scm
            .post_comment(&repo, number, &text::comment_text(&code, &params))
            .await?;
        Ok(Effect::Nothing)
    }
}
