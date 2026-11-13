use saka_core::model::Task;
use saka_core::task::TaskInput;
use saka_core::vocab::{ChangeState, Stage};
use serde_json::Value;

use super::{Effect, Execution, field};
use crate::Engine;

impl Engine {
    pub(super) async fn rerun_checks(&self, task: &Task, payload: &Value) -> Execution {
        let head_sha: String = field(payload, "head_sha")?;
        let number: u32 = field(payload, "number")?;
        let checks: Vec<String> = field(payload, "checks")?;
        let current = task.ci_rerun_at(&head_sha).is_some_and(|s| s.pending)
            && task
                .change_request
                .as_ref()
                .is_some_and(|c| c.state == ChangeState::Open)
            && task.is_at_stage(Stage::Merge);
        if !current {
            return Ok(Effect::Skipped);
        }
        let repo = self.repo_ref_of(field(payload, "repo_id")?)?;
        let settled = |rerun, unsupported| {
            Ok(Effect::Fact(TaskInput::CiRerunSettled {
                head_sha: head_sha.clone(),
                number,
                rerun,
                unsupported,
            }))
        };
        match self.ports.scm.rerun_failed_checks(&repo, &head_sha).await {
            Ok(outcome) => settled(outcome.rerun, outcome.unsupported),
            Err(e) if !e.retryable => {
                tracing::warn!(code = %e.code, "scm.rerun_checks.refused");
                settled(Vec::new(), checks)
            }
            Err(e) => Err(e),
        }
    }
}
