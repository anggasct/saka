use serde_json::{Value, json};

use super::Decider;
use crate::Ctx;
use crate::event::Event;
use crate::ids::RegressionCheckId;
use crate::model::RegressionRun;
use crate::regression::{self, MAX_ATTEMPTS};
use crate::rejection::Rejection;
use crate::vocab::{BlockerCode, RegressionOutcome, SessionState, Stage, StagePhase};

impl Decider<'_> {
    pub(super) fn regression_requested(
        &self,
        id: RegressionCheckId,
        commit_sha: String,
        base_sha: String,
    ) -> Result<Vec<Event>, Rejection> {
        let unavailable = |reason| Err(Rejection::RegressionNotAvailable { reason });
        let task = self.task;
        if !task.is_bugfix() {
            return unavailable("not_bugfix");
        }
        if !task.is_at(Stage::Dev, StagePhase::RunningSession) {
            return unavailable("not_dev");
        }
        let Some(run) = task.stage_run.as_ref() else {
            return unavailable("not_dev");
        };
        let mid_turn = task.live_session().is_some_and(|s| {
            s.turns > s.recorded_turn && !matches!(s.state, SessionState::Interrupted)
        });
        if !mid_turn {
            return unavailable("not_in_turn");
        }
        if task.reproduced_regression().is_some() {
            return unavailable("already_reproduced");
        }
        let attempts: Vec<_> = task.run_regressions().collect();
        if let Some(open) = attempts.iter().find(|c| c.awaiting_result()) {
            return if open.commit_sha == commit_sha {
                Ok(Vec::new())
            } else {
                unavailable("pending")
            };
        }
        if attempts.len() >= MAX_ATTEMPTS as usize {
            return Err(Rejection::RegressionLimitReached);
        }
        Ok(vec![Event::RegressionRequested {
            work_item_id: self.item().id,
            stage_run_id: run.id,
            regression_check_id: id,
            commit_sha,
            base_sha,
            attempt: u32::try_from(attempts.len() + 1).unwrap_or(u32::MAX),
        }])
    }

    pub(super) fn regression_observed(
        &self,
        id: RegressionCheckId,
        outcome: RegressionOutcome,
        checks: Vec<RegressionRun>,
        paths: Vec<String>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Some(check) = self
            .task
            .regressions
            .iter()
            .find(|c| c.id == id && c.awaiting_result())
        else {
            return Vec::new();
        };
        let mut events = vec![Event::RegressionObserved {
            work_item_id: self.item().id,
            stage_run_id: check.stage_run_id,
            regression_check_id: id,
            outcome,
            checks,
            paths,
        }];
        let current = self.run_id() == Some(check.stage_run_id)
            && self.task.is_at(Stage::Dev, StagePhase::RunningSession);
        if outcome != RegressionOutcome::Reproduced && check.attempt >= MAX_ATTEMPTS && current {
            let params = json!({ "attempts": check.attempt, "last_outcome": outcome });
            events.extend(self.end_session(
                SessionState::Ended,
                "regression_not_reproduced",
                params.clone(),
            ));
            events.extend(self.block(BlockerCode::RegressionNotReproduced, params, ctx));
        }
        events
    }

    pub(super) fn regression_branch_reset(
        &self,
        old_head_sha: String,
        base_sha: String,
    ) -> Vec<Event> {
        match (self.task.needs_branch_reset(), self.run_id()) {
            (true, Some(stage_run_id)) => vec![Event::RegressionBranchReset {
                work_item_id: self.item().id,
                stage_run_id,
                old_head_sha,
                base_sha,
            }],
            _ => Vec::new(),
        }
    }

    pub(super) fn regression_unrunnable(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let pending = self
            .task
            .run_regressions()
            .find(|c| c.awaiting_result())
            .map(|c| c.id);
        match pending {
            Some(id) => {
                self.regression_observed(id, RegressionOutcome::Error, Vec::new(), Vec::new(), ctx)
            }
            None => Vec::new(),
        }
    }

    pub(super) fn regression_unproven(&self) -> Option<Value> {
        if !self.task.is_bugfix()
            || !self.task.is_at(Stage::Dev, StagePhase::RunningSession)
            || self.task.reproduced_regression().is_some()
        {
            return None;
        }
        let attempts: Vec<_> = self.task.run_regressions().collect();
        Some(json!({
            "attempts": attempts.len(),
            "last_outcome": regression::last_outcome(&attempts),
        }))
    }
}
