use serde_json::json;

use super::queries::run_view;
use super::{Decider, verdict_params};
use crate::Ctx;
use crate::event::{Event, Prompt};
use crate::ids::{EnvironmentId, ReviewId, SessionId};
use crate::model::{Coded, StageRun};
use crate::review;
use crate::session;
use crate::vocab::{
    BlockerCode, ReviewBasis, ReviewEvent, ReviewOutcome, SessionState, Stage, StagePhase,
    StageRunState, Trigger, WorkItemOrigin,
};

impl Decider<'_> {
    pub(super) fn start_review(&self) -> Vec<Event> {
        let pending = self
            .task
            .stage_run
            .as_ref()
            .is_some_and(|r| r.stage == Stage::Review && r.state == StageRunState::Pending);
        if !pending || !self.task.is_at(Stage::Review, StagePhase::AwaitingReview) {
            return vec![];
        }
        match (
            self.task.external.is_some(),
            self.task.ready_environment().is_some(),
        ) {
            (true, false) => self.phase(StagePhase::PreparingEnvironment, None),
            (false, true) => self.phase(StagePhase::RunningSession, None),
            _ => vec![],
        }
    }

    pub(super) fn review_environment_prepared(
        &self,
        environment_id: EnvironmentId,
        root_path: String,
        worktree_path: String,
        head_sha: String,
    ) -> Vec<Event> {
        let Some(owner) = self.task.run_owner() else {
            return vec![];
        };
        if !self
            .task
            .is_at(Stage::Review, StagePhase::PreparingEnvironment)
        {
            return vec![];
        }
        let mut events = vec![Event::EnvironmentPrepared {
            owner,
            environment_id,
            repo_id: self.item().repo_id,
            root_path,
            worktree_path,
            branch: None,
            base_sha: head_sha,
            port_start: None,
            port_count: None,
            recipe: None,
            recipe_sha256: None,
            setup: Vec::new(),
            toolchains: Vec::new(),
            path_scope: Vec::new(),
            docs: None,
        }];
        events.extend(self.phase(StagePhase::RunningSession, None));
        events
    }

    pub(super) fn review_has_spec(&self) -> bool {
        self.item().spec_id.is_some()
            || self
                .task
                .external
                .as_ref()
                .is_some_and(|e| e.spec.is_some())
    }

    pub(super) fn review_basis(&self) -> ReviewBasis {
        let linked_step = self
            .task
            .external
            .as_ref()
            .and_then(|e| e.spec.as_ref())
            .is_some_and(|s| s.step_id.is_some());
        if self.item().step_id.is_some() || linked_step {
            ReviewBasis::Step
        } else if self.review_has_spec() {
            ReviewBasis::Spec
        } else {
            ReviewBasis::None
        }
    }

    pub(super) fn review_reported(
        &self,
        session_id: SessionId,
        turn: u32,
        text: &str,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Some(session) = self.task.live_session() else {
            return vec![];
        };
        if !self.task.is_at(Stage::Review, StagePhase::RunningSession)
            || session.id != session_id
            || session.turns != turn
            || session.state != SessionState::Idle
            || session.interrupt_requested.is_some()
        {
            return vec![];
        }
        let Some(run) = &self.task.stage_run else {
            return vec![];
        };
        match review::parse(text, self.review_has_spec()) {
            Ok(verdict) => self.review_recorded(run, session.id, verdict, ctx),
            Err(error) if session.followups < review::FORMAT_RETRIES => {
                let Some(view) = run_view(self.task) else {
                    return vec![];
                };
                let prompt = Prompt::VerdictFormat {
                    reason: error.code().to_string(),
                };
                let outcome =
                    session::next_turn(&view, turn + 1, prompt, &self.cfg.session(), self.now);
                self.settle(outcome, ctx)
            }
            Err(error) => {
                let params = json!({ "cause": "verdict_invalid", "code": error.code() });
                let mut events =
                    self.end_session(SessionState::Ended, "verdict_invalid", params.clone());
                events.extend(self.block(BlockerCode::SessionFailed, params, ctx));
                events
            }
        }
    }

    pub(super) fn review_recorded(
        &self,
        run: &StageRun,
        session_id: SessionId,
        verdict: review::Verdict,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Some(head_sha) = run.head_sha.clone() else {
            return vec![];
        };
        let outcome = verdict.outcome;
        let step_id = self.item().step_id.clone().or_else(|| {
            self.task
                .external
                .as_ref()
                .and_then(|e| e.spec.as_ref())
                .and_then(|s| s.step_id.clone())
        });
        let questions = verdict.questions.clone();
        let mut events = vec![Event::ReviewRecorded {
            work_item_id: self.item().id,
            stage_run_id: run.id,
            review_id: ReviewId::new(ctx.ids),
            session_id,
            head_sha: head_sha.clone(),
            outcome,
            basis: self.review_basis(),
            summary: verdict.summary,
            findings: verdict.findings,
            questions: verdict.questions,
        }];
        events.extend(self.end_session(SessionState::Ended, "completed", json!({})));
        match outcome {
            ReviewOutcome::Approved | ReviewOutcome::ChangesRequested => {
                events.extend(self.phase(StagePhase::PostingReview, None));
            }
            ReviewOutcome::SpecUnclear => events.extend(self.block(
                BlockerCode::SpecUnclear,
                json!({ "head_sha": head_sha, "questions": questions, "step_id": step_id }),
                ctx,
            )),
        }
        events
    }

    pub(super) fn own_review_settled(
        &self,
        run: &StageRun,
        review_id: ReviewId,
        mut events: Vec<Event>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Some(review) = self.task.reviews.iter().find(|r| r.id == review_id) else {
            return vec![];
        };
        match review.outcome {
            ReviewOutcome::Approved => {
                events.push(Event::StageRunSucceeded {
                    work_item_id: self.item().id,
                    stage_run_id: run.id,
                    head_sha: run.head_sha.clone(),
                });
                let mut merge = self.new_run(
                    Stage::Merge,
                    Trigger::Auto,
                    StagePhase::AwaitingMerge,
                    None,
                    None,
                    ctx,
                );
                if let Event::StageRunCreated { head_sha, .. } = &mut merge {
                    head_sha.clone_from(&run.head_sha);
                }
                events.push(merge);
            }
            ReviewOutcome::ChangesRequested => {
                let mut params = verdict_params(review);
                let cause = Coded {
                    code: BlockerCode::VerdictFailed.as_str().to_string(),
                    params: params.clone(),
                };
                if self.task.workflow.has_stage(Stage::Fix) && !self.task.fix_rounds_left() {
                    params["rounds"] = json!(self.task.fix_rounds_used());
                    params["max"] = json!(self.task.workflow.fix_rounds);
                    events.extend(self.block(BlockerCode::ReviewRoundsExhausted, params, ctx));
                } else if let Some(fix) = self.start_fix(cause, ctx) {
                    events.push(Event::StageRunSucceeded {
                        work_item_id: self.item().id,
                        stage_run_id: run.id,
                        head_sha: run.head_sha.clone(),
                    });
                    events.push(fix);
                } else {
                    events.extend(self.block(BlockerCode::VerdictFailed, params, ctx));
                }
            }
            ReviewOutcome::SpecUnclear => return vec![],
        }
        events
    }

    pub(super) fn review_posted(
        &self,
        review_id: ReviewId,
        event: ReviewEvent,
        url: Option<String>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let (Some(run), true) = (
            &self.task.stage_run,
            self.task.is_at(Stage::Review, StagePhase::PostingReview),
        ) else {
            return vec![];
        };
        if self
            .task
            .unposted_review()
            .is_none_or(|r| r.id != review_id)
        {
            return vec![];
        }
        let mut events = vec![Event::ReviewPosted {
            work_item_id: self.item().id,
            review_id,
            event,
            url,
        }];
        if self.task.item.origin == WorkItemOrigin::SpecStep {
            return self.own_review_settled(run, review_id, events, ctx);
        }
        events.extend(self.phase(StagePhase::Reviewed, None));
        events.push(Event::StageRunSucceeded {
            work_item_id: self.item().id,
            stage_run_id: run.id,
            head_sha: run.head_sha.clone(),
        });
        let rejected = self
            .task
            .reviews
            .iter()
            .find(|r| r.id == review_id && r.outcome == ReviewOutcome::ChangesRequested);
        if let Some(review) = rejected {
            let cause = Coded {
                code: BlockerCode::VerdictFailed.as_str().to_string(),
                params: verdict_params(review),
            };
            events.extend(self.start_fix(cause, ctx));
        }
        events
    }
}
