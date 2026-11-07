use serde_json::{Value, json};

use super::{Decider, Verdict};
use crate::Ctx;
use crate::event::Event;
use crate::gate::{self, Check, GatePolicy};
use crate::ids::GateResultId;
use crate::session::SessionInput;
use crate::vocab::{BlockerCode, GateOutcome, GateSource, Stage, StagePhase, Trigger};

impl Decider<'_> {
    pub(super) fn tick(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if self.task.live_session().is_some() {
            let events = self.session(SessionInput::Tick, ctx).unwrap_or_default();
            if !events.is_empty() {
                return events;
            }
        }
        let waiting = self.task.is_at(Stage::Merge, StagePhase::AwaitingCi)
            || self.task.is_at(Stage::Sync, StagePhase::Verifying);
        if waiting
            && self.ci_timed_out()
            && let Some(head) = self.gate_head()
        {
            let code = if self.task.is_at(Stage::Sync, StagePhase::Verifying) {
                BlockerCode::DodFailed
            } else {
                BlockerCode::CiRed
            };
            let policy = self.gate_policy();
            let seen = self
                .task
                .gate_for(GateSource::Scm, head)
                .map(|g| g.checks.clone())
                .unwrap_or_default();
            let relevant = policy.scm_checks(&seen);
            let missing = policy.missing_checks(&relevant);
            if policy.uses(GateSource::Scm) && policy.scm_always_required() {
                if !missing.is_empty() && !policy.required_checks.is_empty() {
                    return self.block(
                        code,
                        json!({ "source": "scm", "reason": "required_missing", "missing": missing, "head_sha": head }),
                        ctx,
                    );
                }
                if relevant.is_empty() {
                    return self.block(
                        code,
                        json!({ "source": "scm", "reason": "no_checks", "head_sha": head }),
                        ctx,
                    );
                }
            }
            return self.settle_gate(head, false, None, None, ctx);
        }
        self.settle_hook_gates(ctx)
    }

    pub(super) fn gate_policy(&self) -> GatePolicy {
        GatePolicy::from_recipe(
            self.task
                .environment
                .as_ref()
                .and_then(|e| e.recipe.as_ref()),
        )
    }

    pub(super) fn ci_timed_out(&self) -> bool {
        self.task
            .stage_run
            .as_ref()
            .and_then(|r| r.started_at)
            .map(|started| match &self.task.change_request {
                Some(cr) if self.task.is_at_stage(Stage::Merge) && cr.updated_at > started => {
                    cr.updated_at
                }
                _ => started,
            })
            .is_some_and(|start| self.now.since(start) > self.cfg.ci_appear_timeout_ms)
    }

    pub(super) fn scm_required(
        policy: &GatePolicy,
        scm: Option<(GateOutcome, &[Check])>,
        window_over: bool,
    ) -> bool {
        if !policy.uses(GateSource::Scm) {
            return false;
        }
        if policy.scm_always_required() {
            return true;
        }
        let has_checks = scm.is_some_and(|(_, checks)| !policy.scm_checks(checks).is_empty());
        has_checks || !window_over
    }

    pub(super) fn gate_head(&self) -> Option<&str> {
        if self.task.is_at(Stage::Merge, StagePhase::AwaitingCi)
            || self.task.is_at(Stage::Merge, StagePhase::AwaitingMerge)
        {
            return self
                .task
                .change_request
                .as_ref()
                .map(|c| c.head_sha.as_str());
        }
        if self.task.is_at(Stage::Sync, StagePhase::Verifying) {
            return self.task.stage_run.as_ref()?.head_sha.as_deref();
        }
        None
    }

    pub(super) fn verdict(
        &self,
        head_sha: &str,
        scm: Option<(GateOutcome, &[Check])>,
        saka: Option<(GateOutcome, &[Check], Option<&Value>)>,
        window_over: bool,
    ) -> Verdict {
        let policy = self.gate_policy();
        let scm = scm.or_else(|| {
            self.task
                .gate_for(GateSource::Scm, head_sha)
                .map(|g| (g.outcome, g.checks.as_slice()))
        });
        let saka = saka.or_else(|| {
            self.task
                .gate_for(GateSource::Saka, head_sha)
                .map(|g| (g.outcome, g.checks.as_slice(), None))
        });
        let scm_required = Self::scm_required(&policy, scm, window_over);
        let saka_required = policy.uses(GateSource::Saka);
        let scm_outcome = scm.map(|(_, checks)| policy.scm_outcome(checks));
        if scm_required && scm_outcome == Some(GateOutcome::Failed) {
            let failing: Vec<String> = scm
                .map(|(_, checks)| {
                    policy
                        .scm_checks(checks)
                        .into_iter()
                        .filter(Check::is_failure)
                        .map(|c| c.name)
                        .collect()
                })
                .unwrap_or_default();
            return Verdict::Failed(
                json!({ "source": "scm", "checks": failing, "head_sha": head_sha }),
            );
        }
        if saka_required && let Some((GateOutcome::Failed, checks, failure)) = saka {
            let failing: Vec<String> = checks
                .iter()
                .filter(|c| c.is_failure())
                .map(|c| c.name.clone())
                .collect();
            let mut params = json!({ "source": "saka", "checks": failing, "head_sha": head_sha });
            if let Some(Value::Object(extra)) = failure {
                for (k, v) in extra {
                    params[k.as_str()] = v.clone();
                }
            }
            return Verdict::Failed(params);
        }
        let scm_ok = !scm_required || scm_outcome == Some(GateOutcome::Passed);
        let saka_ok = !saka_required || saka.is_some_and(|(o, _, _)| o == GateOutcome::Passed);
        if scm_ok && saka_ok {
            Verdict::Passed
        } else {
            Verdict::Pending
        }
    }

    pub(super) fn settle_gate(
        &self,
        head_sha: &str,
        head_changed: bool,
        scm: Option<(GateOutcome, &[Check])>,
        saka: Option<(GateOutcome, &[Check], Option<&Value>)>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        if self.task.is_at(Stage::Merge, StagePhase::Merging)
            || self.task.is_at(Stage::Merge, StagePhase::Queued)
            || self.task.is_at(Stage::Merge, StagePhase::Integrating)
        {
            return vec![];
        }
        let verdict = self.verdict(head_sha, scm, saka, self.ci_timed_out());
        if self.task.is_at(Stage::Sync, StagePhase::Verifying) {
            return match verdict {
                Verdict::Failed(mut params) => {
                    if params.get("reason").is_none() {
                        params["reason"] = json!("verify_failed");
                    }
                    self.block(BlockerCode::DodFailed, params, ctx)
                }
                Verdict::Passed => self.phase(StagePhase::TearingDown, None),
                Verdict::Pending => vec![],
            };
        }
        let awaiting_merge = self.task.is_at(Stage::Merge, StagePhase::AwaitingMerge);
        match verdict {
            Verdict::Failed(params) => self.block(BlockerCode::CiRed, params, ctx),
            Verdict::Passed if self.needs_review(head_sha) => self.request_review(head_sha, ctx),
            Verdict::Passed if self.task.is_queued() && !head_changed => {
                self.queued_merge(head_sha, true)
            }
            Verdict::Passed if !awaiting_merge || head_changed => {
                self.phase(StagePhase::AwaitingMerge, Some(head_sha.to_string()))
            }
            Verdict::Passed => vec![],
            Verdict::Pending if awaiting_merge => {
                self.phase(StagePhase::AwaitingCi, Some(head_sha.to_string()))
            }
            Verdict::Pending => vec![],
        }
    }

    pub(super) fn needs_review(&self, head_sha: &str) -> bool {
        self.task.has_review_stage() && !self.task.approved_at(head_sha)
    }

    pub(super) fn request_review(&self, head_sha: &str, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Some(run) = &self.task.stage_run else {
            return vec![];
        };
        let mut request = self.new_run(
            Stage::Review,
            Trigger::Auto,
            StagePhase::AwaitingReview,
            None,
            None,
            ctx,
        );
        if let Event::StageRunCreated {
            head_sha: head @ None,
            ..
        } = &mut request
        {
            *head = Some(head_sha.to_string());
        }
        vec![
            Event::StageRunSucceeded {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                head_sha: Some(head_sha.to_string()),
            },
            request,
        ]
    }

    pub(super) fn proven(&self) -> bool {
        self.task
            .change_request
            .as_ref()
            .is_some_and(|cr| self.verdict(&cr.head_sha, None, None, true) == Verdict::Passed)
    }

    pub(super) fn commit_checks_observed(
        &self,
        head_sha: String,
        checks: Vec<Check>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Some(run) = &self.task.stage_run else {
            return vec![];
        };
        if !self.task.is_at(Stage::Sync, StagePhase::Verifying)
            || self.gate_head() != Some(head_sha.as_str())
        {
            return vec![];
        }
        let outcome = self.gate_policy().scm_outcome(&checks);
        let mut events = Vec::new();
        let unchanged = self
            .task
            .gate_for(GateSource::Scm, &head_sha)
            .is_some_and(|g| g.checks == checks);
        if !unchanged {
            events.push(Event::GateObserved {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                gate_result_id: GateResultId::new(ctx.ids),
                source: GateSource::Scm,
                head_sha: head_sha.clone(),
                outcome,
                checks: checks.clone(),
            });
        }
        events.extend(self.settle_gate(&head_sha, false, Some((outcome, &checks)), None, ctx));
        events
    }

    pub(super) fn saka_gate_started(&self, head_sha: String, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if self.gate_head() != Some(head_sha.as_str())
            || self.task.gate_for(GateSource::Saka, &head_sha).is_some()
        {
            return vec![];
        }
        let Some(run) = &self.task.stage_run else {
            return vec![];
        };
        vec![Event::GateStarted {
            work_item_id: self.item().id,
            stage_run_id: run.id,
            gate_result_id: GateResultId::new(ctx.ids),
            source: GateSource::Saka,
            head_sha,
        }]
    }

    pub(super) fn saka_gate_observed(
        &self,
        head_sha: String,
        checks: Vec<Check>,
        failure: Option<Value>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Some(run) = &self.task.stage_run else {
            return vec![];
        };
        if !matches!(run.stage, Stage::Merge | Stage::Sync) {
            return vec![];
        }
        let outcome = if failure.is_some() || checks.iter().any(Check::is_failure) {
            GateOutcome::Failed
        } else {
            gate::outcome(&checks)
        };
        let mut events = vec![Event::GateObserved {
            work_item_id: self.item().id,
            stage_run_id: run.id,
            gate_result_id: GateResultId::new(ctx.ids),
            source: GateSource::Saka,
            head_sha: head_sha.clone(),
            outcome,
            checks: checks.clone(),
        }];
        if self.gate_head() == Some(head_sha.as_str()) {
            events.extend(self.settle_gate(
                &head_sha,
                false,
                None,
                Some((outcome, &checks, failure.as_ref())),
                ctx,
            ));
        }
        events
    }
}
