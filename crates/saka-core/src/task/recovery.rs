use serde_json::{Value, json};

use super::queries::{auto_retry_due, run_view};
use super::{
    AUTO_RETRY_PREFIX, Decider, MANUAL_RETRY_LIMIT, ObservedChange, RETURN_STAGES, TaskInput,
    return_entry, takeover_place, verdict_params,
};
use crate::Ctx;
use crate::blocker::primary_action;
use crate::event::Event;
use crate::external;
use crate::ids::BlockerId;
use crate::job::{JobKind, exhausted_code};
use crate::model::{Blocker, Coded, FixRound, Task};
use crate::rejection::Rejection;
use crate::session::{self, SessionInput};
use crate::vocab::{
    BlockerAction, BlockerCode, BlockerResolution, ChangeRole, ChangeState, ResumeTrigger,
    ReviewOutcome, SessionState, Stage, StagePhase, Trigger, WorkItemOrigin, WorkItemState,
};

impl Decider<'_> {
    pub(super) fn retry(&self, reason: String, ctx: &mut Ctx<'_>) -> Result<Vec<Event>, Rejection> {
        self.retry_as(Trigger::Manual, reason, ctx)
    }

    pub(super) fn auto_retry(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Some(blocker) = self.task.open_blocker() else {
            return vec![];
        };
        if !auto_retry_due(self.task, self.now) {
            return vec![];
        }
        let reason = format!("{AUTO_RETRY_PREFIX}{}", blocker.code.as_str());
        self.retry_as(Trigger::Auto, reason, ctx)
            .unwrap_or_default()
    }

    pub(super) fn resume(&self, trigger: ResumeTrigger, ctx: &mut Ctx<'_>) -> Vec<Event> {
        self.retry_as(Trigger::Auto, trigger.as_str().to_string(), ctx)
            .unwrap_or_default()
    }

    pub(super) fn retry_as(
        &self,
        trigger: Trigger,
        reason: String,
        ctx: &mut Ctx<'_>,
    ) -> Result<Vec<Event>, Rejection> {
        let blocked = self.item().state == WorkItemState::Blocked;
        let Some(blocker) = self.task.open_blocker().filter(|_| blocked) else {
            return Err(Rejection::InvalidState {
                state: self.state_name(),
            });
        };
        let Some((stage, mut entry)) = retry_target(self.task, blocker.code, &blocker.params)
        else {
            return Err(Rejection::InvalidState {
                state: self.state_name(),
            });
        };
        if trigger == Trigger::Manual
            && self.task.retries.get(&stage).map_or(0, |r| r.manual) >= MANUAL_RETRY_LIMIT
        {
            return Err(Rejection::RetryLimitReached {
                stage: Some(stage.as_str().to_string()),
                limit: MANUAL_RETRY_LIMIT,
            });
        }
        let env_ready = self.task.ready_environment().is_some();
        if stage == Stage::Review {
        } else if entry == StagePhase::RunningSession && !env_ready {
            entry = StagePhase::PreparingEnvironment;
        } else if entry == StagePhase::PreparingEnvironment && env_ready {
            entry = StagePhase::RunningSession;
        }
        let retry_of = Coded {
            code: blocker.code.as_str().to_string(),
            params: blocker.params.clone(),
        };
        let pushing = self
            .task
            .stage_run
            .as_ref()
            .filter(|r| r.stage == stage && entry == StagePhase::Pushing);
        let pushed_head = pushing.and_then(|r| r.head_sha.clone());
        let pushed_docs = pushing.and_then(|r| r.docs_head_sha.clone());
        let mut run = self.new_run(
            stage,
            trigger,
            entry,
            Some(reason.clone()),
            Some(retry_of),
            ctx,
        );
        if stage == Stage::Fix
            && let Event::StageRunCreated { fix, .. } = &mut run
        {
            *fix = Some(self.retry_round(blocker));
        }
        if stage == Stage::Review
            && let Event::StageRunCreated { head_sha, .. } = &mut run
        {
            *head_sha = if entry == StagePhase::PostingReview {
                self.task
                    .stage_run
                    .as_ref()
                    .and_then(|r| r.head_sha.clone())
            } else {
                self.task
                    .change_request
                    .as_ref()
                    .map(|c| c.head_sha.clone())
            };
        }
        let mut events = vec![Event::BlockerResolved {
            work_item_id: self.item().id,
            blocker_id: blocker.id,
            resolution: if trigger == Trigger::Auto {
                BlockerResolution::Auto
            } else {
                BlockerResolution::Retried
            },
            reason: Some(reason),
        }];
        let repush = match (&run, pushed_head) {
            (Event::StageRunCreated { stage_run_id, .. }, Some(head_sha)) => {
                Some(Event::StagePhaseChanged {
                    work_item_id: self.item().id,
                    stage_run_id: *stage_run_id,
                    phase: StagePhase::Pushing,
                    head_sha: Some(head_sha),
                    docs_head_sha: pushed_docs,
                })
            }
            _ => None,
        };
        events.push(run);
        events.extend(repush);
        Ok(events)
    }

    pub(super) fn update_branch(
        &self,
        reason: String,
        ctx: &mut Ctx<'_>,
    ) -> Result<Vec<Event>, Rejection> {
        let invalid = || Rejection::InvalidState {
            state: self.state_name(),
        };
        let (Some(blocker), Some(cr)) = (self.task.open_blocker(), &self.task.change_request)
        else {
            return Err(invalid());
        };
        if blocker.code != BlockerCode::ChangeBehindBase || cr.state != ChangeState::Open {
            return Err(invalid());
        }
        let retry_of = Coded {
            code: blocker.code.as_str().to_string(),
            params: blocker.params.clone(),
        };
        Ok(vec![
            Event::BlockerResolved {
                work_item_id: self.item().id,
                blocker_id: blocker.id,
                resolution: BlockerResolution::Retried,
                reason: Some(reason.clone()),
            },
            self.new_run(
                Stage::Merge,
                Trigger::Auto,
                StagePhase::AwaitingCi,
                Some(reason),
                Some(retry_of),
                ctx,
            ),
            Event::ChangeRequestUpdateRequested {
                work_item_id: self.item().id,
                change_request_id: cr.id,
                expected_head_sha: cr.head_sha.clone(),
            },
        ])
    }

    pub(super) fn retry_round(&self, blocker: &Blocker) -> FixRound {
        if let Some(round) = self
            .task
            .stage_run
            .as_ref()
            .filter(|r| r.stage == Stage::Fix)
            .and_then(|r| r.fix.clone())
        {
            return round;
        }
        let code = match blocker.code {
            BlockerCode::ReviewRoundsExhausted => BlockerCode::VerdictFailed,
            other => other,
        };
        FixRound {
            number: self.task.fix_rounds_used() + 1,
            cause: Coded {
                code: code.as_str().to_string(),
                params: blocker.params.clone(),
            },
        }
    }

    pub(super) fn resolve_blocker(
        &self,
        blocker_id: BlockerId,
        expected: Option<BlockerAction>,
        reason: String,
        ctx: &mut Ctx<'_>,
    ) -> Result<Vec<Event>, Rejection> {
        let open = self
            .task
            .open_blocker()
            .filter(|b| b.id == blocker_id && self.item().state == WorkItemState::Blocked);
        if open.is_none() {
            return Err(Rejection::BlockerNotOpen {
                blocker_id: blocker_id.0.to_string(),
            });
        }
        let designated = primary_action(self.task).map(|a| a.kind);
        if let Some((expected, actual)) = expected.zip(designated)
            && expected != actual
        {
            return Err(Rejection::BlockerActionChanged {
                blocker_id: blocker_id.0.to_string(),
                expected: expected.as_str(),
                actual: actual.as_str(),
            });
        }
        match designated {
            Some(BlockerAction::Retry) => self.retry(reason, ctx),
            Some(BlockerAction::Cancel) => self.cancel(reason),
            Some(BlockerAction::Takeover) => self.takeover(reason),
            Some(BlockerAction::UpdateBranch) => self.update_branch(reason, ctx),
            Some(other) => Err(Rejection::ActionNeedsInput {
                action: other.as_str(),
            }),
            None => Err(Rejection::BlockerNotOpen {
                blocker_id: blocker_id.0.to_string(),
            }),
        }
    }

    pub(super) fn cancel(&self, reason: String) -> Result<Vec<Event>, Rejection> {
        if !matches!(
            self.item().state,
            WorkItemState::Queued
                | WorkItemState::AwaitingStart
                | WorkItemState::Admitted
                | WorkItemState::InStage
                | WorkItemState::Blocked
                | WorkItemState::TakenOver
        ) {
            return Err(Rejection::InvalidState {
                state: self.state_name(),
            });
        }
        let mut events = Vec::new();
        if let Some(b) = self.task.open_blocker() {
            events.push(Event::BlockerResolved {
                work_item_id: self.item().id,
                blocker_id: b.id,
                resolution: BlockerResolution::Cancelled,
                reason: Some(reason.clone()),
            });
        }
        events.extend(external::cancel_run(self.task, "cancelled"));
        events.extend(self.end_session(SessionState::Ended, "cancelled", json!({})));
        events.push(Event::WorkItemCancelled {
            work_item_id: self.item().id,
            reason,
        });
        Ok(events)
    }

    pub(super) fn job_exhausted(
        &self,
        kind: JobKind,
        error: Coded,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        if kind == JobKind::EnvTeardown {
            return self.teardown_failed(error);
        }
        if kind == JobKind::SessionReattach {
            let gone = SessionInput::Gone {
                cause: "runner_silent".to_string(),
            };
            return self.session(gone, ctx).unwrap_or_default();
        }
        if kind == JobKind::EnvPrepare && error.code == "toolchain_unavailable" {
            return self.environment_failed(BlockerCode::ToolchainUnavailable, error.params, ctx);
        }
        if kind == JobKind::ScmPollChangeRequest && self.task.item.stage != Some(Stage::Merge) {
            return vec![];
        }
        let stale = match kind {
            JobKind::EnvPrepareReview => !self
                .task
                .is_at(Stage::Review, StagePhase::PreparingEnvironment),
            JobKind::ScmPostReview => !self.task.is_at(Stage::Review, StagePhase::PostingReview),
            JobKind::ScmIntegrate => !self.task.is_at(Stage::Merge, StagePhase::Integrating),
            _ => false,
        };
        if stale {
            return vec![];
        }
        if kind == JobKind::GateVerify && self.task.is_at(Stage::Sync, StagePhase::Verifying) {
            return self.block(
                BlockerCode::DodFailed,
                json!({ "source": "saka", "reason": "runner_failed", "head_sha": self.gate_head() }),
                ctx,
            );
        }
        let Some((code, mut params)) = exhausted_code(kind) else {
            return vec![];
        };
        if let Value::Object(map) = &mut params {
            map.insert(
                "error".to_string(),
                json!({ "code": error.code, "params": error.params }),
            );
        }
        let mut events = Vec::new();
        if matches!(
            kind,
            JobKind::SessionSendTurn
                | JobKind::SessionDeliverPermissionAnswer
                | JobKind::SessionInterrupt
                | JobKind::SessionSteer
        ) && let Some(view) = run_view(self.task)
        {
            events.extend(session::lose(&view, "runner_unreachable", self.now));
        }
        events.extend(self.block(code, params, ctx));
        events
    }

    pub(super) fn paused(
        &self,
        input: TaskInput,
        ctx: &mut Ctx<'_>,
    ) -> Result<Vec<Event>, Rejection> {
        use TaskInput as I;
        match input {
            I::Return {
                stage,
                reason,
                change,
            } => self.give_back(stage, reason, change, ctx),
            I::Cancel { reason } => self.cancel(reason),
            I::Takeover { .. }
            | I::Retry { .. }
            | I::ResolveBlocker { .. }
            | I::RequestMerge { .. } => Err(Rejection::InvalidState {
                state: self.state_name(),
            }),
            I::Session(
                SessionInput::Steer { .. }
                | SessionInput::Interrupt { .. }
                | SessionInput::Stop { .. }
                | SessionInput::AnswerPermission { .. },
            ) => Err(Rejection::NotFound),
            _ => Ok(vec![]),
        }
    }

    pub(super) fn takeover(&self, reason: String) -> Result<Vec<Event>, Rejection> {
        let (stage, phase) = takeover_place(self.task)?;
        let mut events = Vec::new();
        if let Some(b) = self.task.open_blocker() {
            events.push(Event::BlockerResolved {
                work_item_id: self.item().id,
                blocker_id: b.id,
                resolution: BlockerResolution::TakenOver,
                reason: Some(reason.clone()),
            });
        }
        events.extend(self.end_session(SessionState::Ended, "taken_over", json!({})));
        events.push(Event::WorkItemTakenOver {
            work_item_id: self.item().id,
            reason,
            stage,
            phase,
        });
        Ok(events)
    }

    pub(super) fn give_back(
        &self,
        stage: Option<Stage>,
        reason: String,
        change: Option<ObservedChange>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Vec<Event>, Rejection> {
        let stage = stage.unwrap_or(match self.task.stage_run.as_ref().map(|r| r.stage) {
            Some(stage @ (Stage::Dev | Stage::Merge)) => stage,
            Some(Stage::Review) => Stage::Merge,
            _ => Stage::Dev,
        });
        let env_ready = self.task.ready_environment().is_some();
        let invalid = |reason: &'static str| Rejection::ReturnTargetInvalid {
            stage: stage.as_str(),
            reason,
        };
        let (true, Some(entry)) = (
            RETURN_STAGES.contains(&stage),
            return_entry(stage, env_ready),
        ) else {
            return Err(invalid("stage_unsupported"));
        };
        if matches!(stage, Stage::Review | Stage::Fix) && !self.task.workflow.has_stage(stage) {
            return Err(invalid("stage_not_in_workflow"));
        }
        if stage == Stage::Review && !env_ready {
            return Err(invalid("environment_missing"));
        }
        let mut events = Vec::new();
        let head = match &self.task.change_request {
            Some(cr) => {
                let (state, head) = match change {
                    Some(c) => (c.state, c.head_sha),
                    None => (cr.state, cr.head_sha.clone()),
                };
                if state != ChangeState::Open {
                    return Err(Rejection::ChangeRequestNotOpen {
                        number: cr.number,
                        state: state.as_str(),
                    });
                }
                if head != cr.head_sha {
                    events.push(Event::ChangeRequestUpdated {
                        work_item_id: self.item().id,
                        change_request_id: cr.id,
                        role: ChangeRole::Code,
                        head_sha: head.clone(),
                    });
                }
                Some(head)
            }
            None if stage != Stage::Dev => return Err(invalid("no_change_request")),
            None => None,
        };
        let mut run = self.new_run(stage, Trigger::Auto, entry, Some(reason.clone()), None, ctx);
        if let Event::StageRunCreated { head_sha, fix, .. } = &mut run {
            match stage {
                Stage::Review => head_sha.clone_from(&head),
                Stage::Fix => {
                    let verdict = self.task.reviews.iter().rev().find(|r| {
                        r.outcome == ReviewOutcome::ChangesRequested
                            && Some(&r.head_sha) == head.as_ref()
                    });
                    let Some(verdict) = verdict else {
                        return Err(invalid("no_findings"));
                    };
                    *fix = Some(FixRound {
                        number: self.task.fix_rounds_used().max(1),
                        cause: Coded {
                            code: BlockerCode::VerdictFailed.as_str().to_string(),
                            params: verdict_params(verdict),
                        },
                    });
                }
                _ => {}
            }
        }
        events.push(Event::WorkItemReturned {
            work_item_id: self.item().id,
            reason,
            stage,
        });
        events.push(run);
        Ok(events)
    }
}

pub fn retry_target(task: &Task, code: BlockerCode, params: &Value) -> Option<(Stage, StagePhase)> {
    let run_stage = task.stage_run.as_ref().map(|r| r.stage);
    let fixes = task.item.origin == WorkItemOrigin::SpecStep && task.workflow.has_stage(Stage::Fix);
    if run_stage == Some(Stage::Fix) {
        return retry_entry(code, params).map(|(stage, phase)| match stage {
            Stage::Dev => (Stage::Fix, phase),
            other => (other, phase),
        });
    }
    use BlockerCode as B;
    if run_stage != Some(Stage::Review) {
        return match code {
            B::MergeConflict
                if fixes
                    && matches!(
                        params.get("reason").and_then(Value::as_str),
                        Some("rebase_conflict" | "worktree_unclean")
                    ) =>
            {
                Some((Stage::Fix, StagePhase::RunningSession))
            }
            B::CiRed if task.external.is_some() => Some((Stage::Merge, StagePhase::AwaitingCi)),
            _ => retry_entry(code, params),
        };
    }
    match code {
        B::EnvSetupFailed
        | B::SessionFailed
        | B::SessionLost
        | B::SessionTimeout
        | B::PermissionExpired
        | B::DecisionExpired
        | B::BudgetExhausted
        | B::QuotaExhausted
        | B::SpecUnclear => Some((Stage::Review, StagePhase::AwaitingReview)),
        B::VerdictFailed if fixes => Some((Stage::Fix, StagePhase::RunningSession)),
        B::VerdictFailed => Some((Stage::Dev, StagePhase::RunningSession)),
        B::ReviewRoundsExhausted if fixes => Some((Stage::Fix, StagePhase::RunningSession)),
        B::ScmUnavailable
            if params.get("phase").and_then(Value::as_str) == Some("posting_review") =>
        {
            Some((Stage::Review, StagePhase::PostingReview))
        }
        _ => retry_entry(code, params).filter(|(stage, _)| *stage == Stage::Review),
    }
}

pub fn retry_entry(code: BlockerCode, params: &Value) -> Option<(Stage, StagePhase)> {
    use BlockerCode as B;
    Some(match code {
        B::EnvSetupFailed
        | B::EnvRecipeInvalid
        | B::ToolchainUnpinned
        | B::ToolchainUnavailable
        | B::SecretMissing
        | B::SecretUnreadable => (Stage::Dev, StagePhase::PreparingEnvironment),
        B::SessionFailed
        | B::SessionLost
        | B::SessionTimeout
        | B::PermissionExpired
        | B::DecisionExpired
        | B::BudgetExhausted
        | B::QuotaExhausted
        | B::NoChanges => (Stage::Dev, StagePhase::RunningSession),
        B::PathScopeViolation if params.get("stage").and_then(Value::as_str) == Some("sync") => {
            (Stage::Sync, StagePhase::MergingDocs)
        }
        B::PathScopeViolation => (Stage::Dev, StagePhase::RunningSession),
        B::DocsMergeFailed => (Stage::Sync, StagePhase::MergingDocs),
        B::UserLinkRequired | B::UserTokenInvalid => {
            match params.get("phase").and_then(Value::as_str)?.parse().ok()? {
                StagePhase::OpeningChange => (Stage::Dev, StagePhase::OpeningChange),
                _ => (Stage::Dev, StagePhase::RunningSession),
            }
        }
        B::UserAccessDenied => (Stage::Dev, StagePhase::OpeningChange),
        B::SpecUnclear => (Stage::Review, StagePhase::AwaitingReview),
        B::VerdictFailed => (Stage::Dev, StagePhase::RunningSession),
        B::CiRed if params.get("reason").and_then(Value::as_str) == Some("runner_failed") => {
            (Stage::Merge, StagePhase::AwaitingCi)
        }
        B::CiRed => (Stage::Dev, StagePhase::RunningSession),
        B::PushRejected => (Stage::Dev, StagePhase::Pushing),
        B::BranchMoved => (Stage::Dev, StagePhase::RunningSession),
        B::MergeConflict
            if matches!(
                params.get("reason").and_then(Value::as_str),
                Some("rebase_conflict" | "worktree_unclean")
            ) =>
        {
            (Stage::Dev, StagePhase::RunningSession)
        }
        B::MergeConflict | B::ChangeBehindBase => (Stage::Merge, StagePhase::AwaitingCi),
        B::DodFailed => (Stage::Sync, StagePhase::Verifying),
        B::HookFailed => (Stage::Sync, StagePhase::TearingDown),
        B::ScmUnavailable => {
            let phase: StagePhase = params.get("phase").and_then(Value::as_str)?.parse().ok()?;
            let stage = match phase {
                StagePhase::PreparingEnvironment
                | StagePhase::RunningSession
                | StagePhase::Pushing
                | StagePhase::OpeningChange => Stage::Dev,
                StagePhase::AwaitingCi
                | StagePhase::AwaitingMerge
                | StagePhase::Queued
                | StagePhase::Integrating
                | StagePhase::Merging => Stage::Merge,
                StagePhase::MergingDocs | StagePhase::Verifying | StagePhase::TearingDown => {
                    Stage::Sync
                }
                StagePhase::AwaitingTrigger
                | StagePhase::AwaitingReview
                | StagePhase::PostingReview
                | StagePhase::Reviewed => Stage::Review,
            };
            let phase = if matches!(
                phase,
                StagePhase::Merging | StagePhase::Queued | StagePhase::Integrating
            ) {
                StagePhase::AwaitingCi
            } else {
                phase
            };
            (stage, phase)
        }
        _ => return None,
    })
}
