use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::Ctx;
use crate::event::{Event, Prompt};
use crate::gate::{self, Check, GatePolicy};
use crate::ids::{BlockerId, ChangeRequestId, EnvironmentId, GateResultId, StageRunId};
use crate::job::{JobKind, exhausted_code};
use crate::model::{
    Blocker, Budget, ChangeRequest, Coded, EnvironmentSetup, GateResult, Retries, RunOwner,
    StageRun, Task, Usage, WorkItem,
};
use crate::rejection::Rejection;
use crate::session::{
    self, RunRetry, RunView, SessionConfig, SessionInput, SessionOutcome, SessionSignal,
};
use crate::time::Timestamp;
use crate::vocab::{
    BlockerCode, BlockerResolution, ChangeState, EnvironmentState, GateOutcome, GateSource,
    MergeMethod, ResumeCause, SessionState, Stage, StagePhase, StageRunState, Trigger,
    WorkItemOrigin, WorkItemState,
};

#[derive(Debug, Clone, PartialEq)]
pub struct LoopConfig {
    pub max_followup_turns: u32,
    pub ci_appear_timeout_ms: i64,
    pub poll_interval_ms: i64,
    pub merge_method: MergeMethod,
    pub delete_branch_after_merge: bool,
    pub commit_status: bool,
    pub permission_wait_ms: i64,
    pub idle_ms: i64,
    pub budgets: BTreeMap<Stage, Budget>,
    pub budget_warn_percent: u32,
    pub rate_limit_pause: f64,
    pub account: String,
}

pub const AUTO_RETRY_PREFIX: &str = "auto retry: ";
pub const MANUAL_RETRY_LIMIT: u32 = 3;
pub const AUTO_RETRY_DELAYS_MS: [i64; 2] = [60_000, 300_000];
pub const MERGED_ON_HOST: &str = "change merged on the host";

impl LoopConfig {
    pub fn session(&self) -> SessionConfig {
        SessionConfig {
            permission_wait_ms: self.permission_wait_ms,
            idle_ms: self.idle_ms,
            budget_warn_percent: self.budget_warn_percent,
            rate_limit_pause: self.rate_limit_pause,
            account: self.account.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Verdict {
    Failed(Value),
    Passed,
    Pending,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskInput {
    Admit,
    EnvironmentPrepared {
        environment_id: EnvironmentId,
        root_path: String,
        worktree_path: String,
        base_sha: String,
        setup: EnvironmentSetup,
    },
    EnvironmentFailed {
        code: BlockerCode,
        params: Value,
    },
    Session(SessionInput),
    WorktreeInspected {
        turn: u32,
        dirty: bool,
        commits_ahead: u32,
        head_sha: String,
    },
    Tick,
    BranchPushed {
        sha: String,
    },
    PushRejected {
        remote_sha: String,
        local_sha: String,
    },
    ChangeOpened {
        change_request_id: ChangeRequestId,
        number: u64,
        url: String,
        head_sha: String,
        base_branch: String,
    },
    SakaGateStarted {
        head_sha: String,
    },
    AutoRetry,
    SakaGateObserved {
        head_sha: String,
        checks: Vec<Check>,
        failure: Option<Value>,
    },
    ChangeObserved {
        state: ChangeState,
        head_sha: String,
        merge_commit_sha: Option<String>,
        merged_at: Option<Timestamp>,
        checks: Vec<Check>,
    },
    RequestMerge {
        expected_version: u64,
        reason: Option<String>,
    },
    MergeSucceeded {
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
    },
    MergeRejected {
        reason: String,
    },
    MergeVerified {
        found: bool,
        base_tip: Option<String>,
    },
    CommitChecksObserved {
        head_sha: String,
        checks: Vec<Check>,
    },
    EnvironmentTornDown {
        teardown_failures: Vec<Value>,
    },
    Retry {
        reason: String,
    },
    Cancel {
        reason: String,
    },
    JobExhausted {
        kind: JobKind,
        error: Coded,
    },
}

pub fn decide(
    task: &Task,
    input: TaskInput,
    cfg: &LoopConfig,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<Event>, Rejection> {
    if task.item.origin != WorkItemOrigin::SpecStep {
        return Err(Rejection::Unsupported);
    }
    if task.item.state == WorkItemState::Superseded {
        return Err(Rejection::InvalidState {
            state: task.item.state.as_str().to_string(),
        });
    }
    let d = Decider {
        task,
        cfg,
        now: ctx.now,
    };
    use TaskInput as I;
    Ok(match input {
        I::Admit => d.admit(ctx),
        I::EnvironmentPrepared {
            environment_id,
            root_path,
            worktree_path,
            base_sha,
            setup,
        } => d.environment_prepared(environment_id, root_path, worktree_path, base_sha, setup),
        I::EnvironmentFailed { code, params } => d.environment_failed(code, params, ctx),
        I::Session(input) => return d.session(input, ctx),
        I::WorktreeInspected {
            turn,
            dirty,
            commits_ahead,
            head_sha,
        } => d.worktree_inspected(turn, dirty, commits_ahead, head_sha, ctx),
        I::Tick => d.tick(ctx),
        I::BranchPushed { sha } => d.branch_pushed(sha, ctx),
        I::PushRejected {
            remote_sha,
            local_sha,
        } => {
            if task.is_at(Stage::Dev, StagePhase::Pushing) {
                d.block(
                    BlockerCode::PushRejected,
                    json!({ "remote_sha": remote_sha, "local_sha": local_sha }),
                    ctx,
                )
            } else {
                vec![]
            }
        }
        I::ChangeOpened {
            change_request_id,
            number,
            url,
            head_sha,
            base_branch,
        } => d.change_opened(change_request_id, number, url, head_sha, base_branch, ctx),
        I::ChangeObserved {
            state,
            head_sha,
            merge_commit_sha,
            merged_at,
            checks,
        } => d.change_observed(state, head_sha, merge_commit_sha, merged_at, checks, ctx),
        I::SakaGateStarted { head_sha } => d.saka_gate_started(head_sha, ctx),
        I::SakaGateObserved {
            head_sha,
            checks,
            failure,
        } => d.saka_gate_observed(head_sha, checks, failure, ctx),
        I::RequestMerge {
            expected_version,
            reason,
        } => return d.request_merge(expected_version, reason),
        I::MergeSucceeded {
            merge_commit_sha,
            merged_at,
        } => {
            if task.is_at(Stage::Merge, StagePhase::Merging) {
                d.merged(merge_commit_sha, merged_at, true, ctx)
            } else {
                vec![]
            }
        }
        I::MergeRejected { reason } => {
            if task.is_at(Stage::Merge, StagePhase::Merging) {
                let head = task.change_request.as_ref().map(|c| c.head_sha.clone());
                d.block(
                    BlockerCode::MergeConflict,
                    json!({ "reason": reason, "head_sha": head }),
                    ctx,
                )
            } else {
                vec![]
            }
        }
        I::MergeVerified { found, base_tip } => d.merge_verified(found, base_tip, ctx),
        I::CommitChecksObserved { head_sha, checks } => {
            d.commit_checks_observed(head_sha, checks, ctx)
        }
        I::EnvironmentTornDown { teardown_failures } => d.environment_torn_down(teardown_failures),
        I::Retry { reason } => return d.retry(reason, ctx),
        I::AutoRetry => d.auto_retry(ctx),
        I::Cancel { reason } => return d.cancel(reason),
        I::JobExhausted { kind, error } => d.job_exhausted(kind, error, ctx),
    })
}

struct Decider<'a> {
    task: &'a Task,
    cfg: &'a LoopConfig,
    now: Timestamp,
}

impl Decider<'_> {
    fn item(&self) -> &WorkItem {
        &self.task.item
    }

    fn run_id(&self) -> Option<StageRunId> {
        self.task.stage_run.as_ref().map(|r| r.id)
    }

    fn next_attempt(&self, stage: Stage) -> u32 {
        self.task.attempts.get(&stage).copied().unwrap_or(0) + 1
    }

    fn new_run(
        &self,
        stage: Stage,
        trigger: Trigger,
        entry: StagePhase,
        reason: Option<String>,
        retry_of: Option<Coded>,
        ctx: &mut Ctx<'_>,
    ) -> Event {
        Event::StageRunCreated {
            work_item_id: self.item().id,
            stage_run_id: StageRunId::new(ctx.ids),
            stage,
            attempt: self.next_attempt(stage),
            trigger,
            entry_phase: entry,
            reason,
            retry_of,
            budget: self.cfg.budgets.get(&stage).copied(),
        }
    }

    fn phase(&self, phase: StagePhase, head_sha: Option<String>) -> Vec<Event> {
        match self.run_id() {
            Some(stage_run_id) => {
                vec![Event::StagePhaseChanged {
                    work_item_id: self.item().id,
                    stage_run_id,
                    phase,
                    head_sha,
                }]
            }
            None => vec![],
        }
    }

    fn session(&self, input: SessionInput, ctx: &mut Ctx<'_>) -> Result<Vec<Event>, Rejection> {
        let Some(view) = run_view(self.task) else {
            return match input {
                SessionInput::Steer { .. }
                | SessionInput::Interrupt { .. }
                | SessionInput::Stop { .. }
                | SessionInput::AnswerPermission { .. } => Err(Rejection::NotFound),
                _ => Ok(vec![]),
            };
        };
        if matches!(input, SessionInput::Launched { .. })
            && !self.task.is_at(Stage::Dev, StagePhase::RunningSession)
        {
            return Ok(vec![]);
        }
        let outcome = session::decide(&view, input, &self.cfg.session(), ctx)?;
        Ok(self.settle(outcome, ctx))
    }

    fn settle(&self, outcome: SessionOutcome, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let mut events = outcome.events;
        match outcome.signal {
            // The finished turn's worktree is inspected by the job `react` schedules for it.
            None | Some(SessionSignal::TurnCompleted { .. }) => {}
            Some(SessionSignal::Stopped {
                code: BlockerCode::SessionLost,
                ..
            }) if !self.task.is_at(Stage::Dev, StagePhase::RunningSession) => {}
            Some(SessionSignal::Stopped { code, params }) => {
                events.extend(self.block(code, params, ctx));
            }
        }
        events
    }

    fn end_session(&self, state: SessionState, code: &str, params: Value) -> Vec<Event> {
        run_view(self.task)
            .map(|view| session::end(&view, state, code, params, self.now))
            .unwrap_or_default()
    }

    fn block(&self, code: BlockerCode, params: Value, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if self.task.open_blocker().is_some() || self.item().state.is_terminal() {
            return vec![];
        }
        vec![Event::WorkItemBlocked {
            work_item_id: self.item().id,
            stage_run_id: self.run_id(),
            blocker_id: BlockerId::new(ctx.ids),
            code,
            params,
            stage: self
                .item()
                .stage
                .or(self.task.stage_run.as_ref().map(|r| r.stage)),
            phase: self
                .item()
                .stage_phase
                .or(self.task.stage_run.as_ref().map(|r| r.entry_phase)),
        }]
    }

    fn admit(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if self.item().state != WorkItemState::Queued {
            return vec![];
        }
        vec![
            Event::WorkItemAdmitted {
                work_item_id: self.item().id,
            },
            self.new_run(
                Stage::Dev,
                Trigger::Auto,
                StagePhase::PreparingEnvironment,
                None,
                None,
                ctx,
            ),
        ]
    }

    fn environment_prepared(
        &self,
        environment_id: EnvironmentId,
        root_path: String,
        worktree_path: String,
        base_sha: String,
        setup: EnvironmentSetup,
    ) -> Vec<Event> {
        if !self.preparing() {
            return vec![];
        }
        let Some(owner) = self.task.run_owner() else {
            return vec![];
        };
        let mut events = vec![Event::EnvironmentPrepared {
            owner,
            environment_id,
            repo_id: self.item().repo_id,
            root_path,
            worktree_path,
            branch: Some(self.item().branch.clone()),
            base_sha,
            port_start: setup.port_start,
            port_count: setup.port_count,
            recipe: setup.recipe,
            recipe_sha256: setup.recipe_sha256,
            setup: setup.setup,
            toolchains: setup.toolchains,
        }];
        events.extend(self.phase(StagePhase::RunningSession, None));
        events
    }

    fn preparing(&self) -> bool {
        self.item().state == WorkItemState::Admitted
            && self
                .task
                .stage_run
                .as_ref()
                .is_some_and(|r| r.state == StageRunState::Pending)
    }

    fn environment_failed(
        &self,
        code: BlockerCode,
        params: Value,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        if !self.preparing() {
            return vec![];
        }
        self.block(code, params, ctx)
    }

    fn worktree_inspected(
        &self,
        turn: u32,
        dirty: bool,
        commits_ahead: u32,
        head_sha: String,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Some(session) = self.task.live_session() else {
            return vec![];
        };
        if !self.task.is_at(Stage::Dev, StagePhase::RunningSession)
            || session.turns != turn
            || session.state != SessionState::Idle
            || session.interrupt_requested.is_some()
        {
            return vec![];
        }
        if !dirty && commits_ahead > 0 {
            let mut events = self.end_session(SessionState::Ended, "completed", json!({}));
            events.extend(self.phase(StagePhase::Pushing, Some(head_sha)));
            return events;
        }
        if session.followups < self.cfg.max_followup_turns {
            let Some(view) = run_view(self.task) else {
                return vec![];
            };
            let prompt = Prompt::Uncommitted {
                dirty,
                commits_ahead,
            };
            let outcome =
                session::next_turn(&view, turn + 1, prompt, &self.cfg.session(), self.now);
            return self.settle(outcome, ctx);
        }
        let params = json!({ "dirty": dirty, "commits": commits_ahead });
        let mut events = self.end_session(SessionState::Ended, "no_changes", params.clone());
        events.extend(self.block(BlockerCode::NoChanges, params, ctx));
        events
    }

    fn tick(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
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
        vec![]
    }

    fn gate_policy(&self) -> GatePolicy {
        GatePolicy::from_recipe(
            self.task
                .environment
                .as_ref()
                .and_then(|e| e.recipe.as_ref()),
        )
    }

    fn ci_timed_out(&self) -> bool {
        self.task
            .stage_run
            .as_ref()
            .and_then(|r| r.started_at)
            .is_some_and(|started| self.now.since(started) > self.cfg.ci_appear_timeout_ms)
    }

    fn scm_required(
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

    fn gate_head(&self) -> Option<&str> {
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

    fn verdict(
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

    fn settle_gate(
        &self,
        head_sha: &str,
        head_changed: bool,
        scm: Option<(GateOutcome, &[Check])>,
        saka: Option<(GateOutcome, &[Check], Option<&Value>)>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        if self.task.is_at(Stage::Merge, StagePhase::Merging) {
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

    fn proven(&self) -> bool {
        self.task
            .change_request
            .as_ref()
            .is_some_and(|cr| self.verdict(&cr.head_sha, None, None, true) == Verdict::Passed)
    }

    fn commit_checks_observed(
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

    fn saka_gate_started(&self, head_sha: String, ctx: &mut Ctx<'_>) -> Vec<Event> {
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

    fn saka_gate_observed(
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

    fn branch_pushed(&self, sha: String, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if !self.task.is_at(Stage::Dev, StagePhase::Pushing) {
            return vec![];
        }
        let mut events = vec![Event::BranchPushed {
            work_item_id: self.item().id,
            branch: self.item().branch.clone(),
            sha: sha.clone(),
        }];
        match &self.task.change_request {
            // The change already exists (a retried dev stage): it now shows the new head.
            Some(cr) if cr.state == ChangeState::Open => {
                if cr.head_sha != sha {
                    events.push(Event::ChangeRequestUpdated {
                        work_item_id: self.item().id,
                        change_request_id: cr.id,
                        head_sha: sha.clone(),
                    });
                }
                events.extend(self.dev_done(sha, ctx));
            }
            _ => events.extend(self.phase(StagePhase::OpeningChange, Some(sha))),
        }
        events
    }

    fn dev_done(&self, head_sha: String, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Some(run) = &self.task.stage_run else {
            return vec![];
        };
        vec![
            Event::StageRunSucceeded {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                head_sha: Some(head_sha),
            },
            self.new_run(
                Stage::Merge,
                Trigger::Auto,
                StagePhase::AwaitingCi,
                None,
                None,
                ctx,
            ),
        ]
    }

    fn change_opened(
        &self,
        id: ChangeRequestId,
        number: u64,
        url: String,
        head_sha: String,
        base_branch: String,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        if !self.task.is_at(Stage::Dev, StagePhase::OpeningChange) {
            return vec![];
        }
        let mut events = vec![Event::ChangeRequestOpened {
            work_item_id: self.item().id,
            change_request_id: id,
            repo_id: self.item().repo_id,
            number,
            url,
            head_branch: self.item().branch.clone(),
            base_branch,
            head_sha: head_sha.clone(),
        }];
        events.extend(self.dev_done(head_sha, ctx));
        events
    }

    fn change_observed(
        &self,
        state: ChangeState,
        head_sha: String,
        merge_commit_sha: Option<String>,
        merged_at: Option<Timestamp>,
        checks: Vec<Check>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let in_merge = [
            StagePhase::AwaitingCi,
            StagePhase::AwaitingMerge,
            StagePhase::Merging,
        ]
        .iter()
        .any(|p| self.task.is_at(Stage::Merge, *p));
        let (Some(cr), Some(run)) = (&self.task.change_request, &self.task.stage_run) else {
            return vec![];
        };
        if cr.state != ChangeState::Open {
            return vec![];
        }
        if !in_merge {
            return match merge_commit_sha {
                Some(sha) if state == ChangeState::Merged && watches_change(self.task) => {
                    self.merged_elsewhere(sha, merged_at, ctx)
                }
                _ => vec![],
            };
        }
        match state {
            ChangeState::Merged => {
                let Some(sha) = merge_commit_sha else {
                    return vec![];
                };
                return self.merged(sha, merged_at, false, ctx);
            }
            ChangeState::Closed => {
                let mut events = vec![Event::ChangeRequestClosed {
                    work_item_id: self.item().id,
                    change_request_id: cr.id,
                }];
                events.extend(self.block(
                    BlockerCode::ChangeClosed,
                    json!({ "number": cr.number }),
                    ctx,
                ));
                return events;
            }
            ChangeState::Open => {}
        }
        let mut events = Vec::new();
        let head_changed = cr.head_sha != head_sha;
        if head_changed {
            events.push(Event::ChangeRequestUpdated {
                work_item_id: self.item().id,
                change_request_id: cr.id,
                head_sha: head_sha.clone(),
            });
        }
        let unchanged = self
            .task
            .gate_for(GateSource::Scm, &head_sha)
            .is_some_and(|g| g.checks == checks);
        let policy = self.gate_policy();
        let outcome = policy.scm_outcome(&checks);
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
        events.extend(self.settle_gate(
            &head_sha,
            head_changed,
            Some((outcome, &checks)),
            None,
            ctx,
        ));
        events
    }

    fn request_merge(
        &self,
        expected_version: u64,
        reason: Option<String>,
    ) -> Result<Vec<Event>, Rejection> {
        if expected_version != self.item().version {
            return Err(Rejection::VersionStale);
        }
        // A task stopped in the merge stage (red CI, a conflict) is not ready to merge,
        // rather than in the wrong state.
        let in_merge = match self.item().state {
            WorkItemState::InStage => self.item().stage == Some(Stage::Merge),
            WorkItemState::Blocked => self
                .task
                .stage_run
                .as_ref()
                .is_some_and(|r| r.stage == Stage::Merge),
            _ => false,
        };
        if !in_merge || self.task.is_at(Stage::Merge, StagePhase::Merging) {
            return Err(Rejection::InvalidState {
                state: self.state_name(),
            });
        }
        let cr = self.task.change_request.as_ref();
        let ready =
            self.task.is_at(Stage::Merge, StagePhase::AwaitingMerge) && gates_clear(self.task);
        let (Some(cr), true) = (cr, ready) else {
            let gate_outcome = cr
                .and_then(|c| {
                    let mut at_head = self.task.gates.iter().filter(|g| g.head_sha == c.head_sha);
                    at_head
                        .clone()
                        .find(|g| g.outcome == GateOutcome::Failed)
                        .or_else(|| at_head.find(|g| g.outcome == GateOutcome::Pending))
                        .or_else(|| self.task.gate_for(GateSource::Scm, &c.head_sha))
                })
                .map(|g| g.outcome.as_str().to_string());
            return Err(Rejection::MergeNotReady {
                gate_outcome,
                head_sha: cr.map(|c| c.head_sha.clone()),
            });
        };
        let mut events = vec![Event::MergeRequested {
            work_item_id: self.item().id,
            head_sha: cr.head_sha.clone(),
            merge_method: self.cfg.merge_method,
            reason,
        }];
        events.extend(self.phase(StagePhase::Merging, Some(cr.head_sha.clone())));
        Ok(events)
    }

    fn merged(
        &self,
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
        by_saka: bool,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let (Some(cr), Some(run)) = (&self.task.change_request, &self.task.stage_run) else {
            return vec![];
        };
        vec![
            Event::ChangeRequestMerged {
                work_item_id: self.item().id,
                change_request_id: cr.id,
                merge_commit_sha: merge_commit_sha.clone(),
                merged_at,
                merged_by_saka: by_saka,
            },
            Event::StageRunSucceeded {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                head_sha: Some(cr.head_sha.clone()),
            },
            self.new_run(
                Stage::Sync,
                Trigger::Auto,
                StagePhase::Verifying,
                None,
                None,
                ctx,
            ),
        ]
    }

    fn merged_elsewhere(
        &self,
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Some(cr) = &self.task.change_request else {
            return vec![];
        };
        let mut events = Vec::new();
        if let Some(b) = self.task.open_blocker() {
            events.push(Event::BlockerResolved {
                work_item_id: self.item().id,
                blocker_id: b.id,
                resolution: BlockerResolution::Auto,
                reason: Some(MERGED_ON_HOST.to_string()),
            });
        }
        events.extend(self.end_session(SessionState::Ended, "change_merged", json!({})));
        events.push(Event::ChangeRequestMerged {
            work_item_id: self.item().id,
            change_request_id: cr.id,
            merge_commit_sha,
            merged_at,
            merged_by_saka: false,
        });
        events.push(self.new_run(
            Stage::Sync,
            Trigger::Auto,
            StagePhase::Verifying,
            Some(MERGED_ON_HOST.to_string()),
            None,
            ctx,
        ));
        events
    }

    fn merge_verified(
        &self,
        found: bool,
        base_tip: Option<String>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let targeted = self
            .task
            .stage_run
            .as_ref()
            .is_some_and(|r| r.head_sha.is_some());
        if !self.task.is_at(Stage::Sync, StagePhase::Verifying) || targeted {
            return vec![];
        }
        let cr = self.task.change_request.as_ref();
        if !found {
            return self.block(
                BlockerCode::DodFailed,
                json!({
                    "reason": "not_in_base",
                    "merge_commit_sha": cr.and_then(|c| c.merge_commit_sha.clone()),
                    "base_branch": cr.map(|c| c.base_branch.clone()),
                }),
                ctx,
            );
        }
        if self.proven() {
            return self.phase(StagePhase::TearingDown, None);
        }
        let first = self.task.attempts.get(&Stage::Sync).copied().unwrap_or(1) <= 1;
        let target = if first {
            cr.and_then(|c| c.merge_commit_sha.clone())
        } else {
            base_tip.or_else(|| cr.and_then(|c| c.merge_commit_sha.clone()))
        };
        match target {
            Some(t) => self.phase(StagePhase::Verifying, Some(t)),
            None => vec![],
        }
    }

    fn environment_torn_down(&self, teardown_failures: Vec<Value>) -> Vec<Event> {
        let Some(env) = self
            .task
            .environment
            .as_ref()
            .filter(|e| e.state != EnvironmentState::TornDown)
        else {
            return vec![];
        };
        let mut events = vec![Event::EnvironmentTornDown {
            owner: env.owner,
            environment_id: env.id,
            teardown_failures,
        }];
        events.extend(self.finish_sync());
        events
    }

    fn teardown_failed(&self, error: Coded) -> Vec<Event> {
        let Some(env) = self.task.environment.as_ref().filter(|e| {
            !matches!(
                e.state,
                EnvironmentState::TeardownFailed | EnvironmentState::TornDown
            )
        }) else {
            return vec![];
        };
        let mut events = vec![Event::EnvironmentTeardownFailed {
            owner: env.owner,
            environment_id: env.id,
            error,
        }];
        events.extend(self.finish_sync());
        events
    }

    fn finish_sync(&self) -> Vec<Event> {
        if !self.task.is_at(Stage::Sync, StagePhase::TearingDown) {
            return vec![];
        }
        let (Some(run), Some(sha)) = (
            &self.task.stage_run,
            self.task
                .change_request
                .as_ref()
                .and_then(|c| c.merge_commit_sha.clone()),
        ) else {
            return vec![];
        };
        vec![
            Event::StageRunSucceeded {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                head_sha: None,
            },
            Event::WorkItemDone {
                work_item_id: self.item().id,
                merge_commit_sha: sha,
            },
        ]
    }

    fn retry(&self, reason: String, ctx: &mut Ctx<'_>) -> Result<Vec<Event>, Rejection> {
        self.retry_as(Trigger::Manual, reason, ctx)
    }

    fn auto_retry(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
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

    fn retry_as(
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
        let Some((stage, mut entry)) = retry_entry(blocker.code, &blocker.params) else {
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
        if entry == StagePhase::RunningSession && !env_ready {
            entry = StagePhase::PreparingEnvironment;
        } else if entry == StagePhase::PreparingEnvironment && env_ready {
            entry = StagePhase::RunningSession;
        }
        let retry_of = Coded {
            code: blocker.code.as_str().to_string(),
            params: blocker.params.clone(),
        };
        let pushed_head = self
            .task
            .stage_run
            .as_ref()
            .filter(|r| r.stage == stage && entry == StagePhase::Pushing)
            .and_then(|r| r.head_sha.clone());
        let run = self.new_run(
            stage,
            trigger,
            entry,
            Some(reason.clone()),
            Some(retry_of),
            ctx,
        );
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
        // A new run starts without a head; pushing again needs the commit the failed run produced.
        let repush = match (&run, pushed_head) {
            (Event::StageRunCreated { stage_run_id, .. }, Some(head_sha)) => {
                Some(Event::StagePhaseChanged {
                    work_item_id: self.item().id,
                    stage_run_id: *stage_run_id,
                    phase: StagePhase::Pushing,
                    head_sha: Some(head_sha),
                })
            }
            _ => None,
        };
        events.push(run);
        events.extend(repush);
        Ok(events)
    }

    fn cancel(&self, reason: String) -> Result<Vec<Event>, Rejection> {
        if !matches!(
            self.item().state,
            WorkItemState::Queued
                | WorkItemState::Admitted
                | WorkItemState::InStage
                | WorkItemState::Blocked
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
        events.extend(self.end_session(SessionState::Ended, "cancelled", json!({})));
        events.push(Event::WorkItemCancelled {
            work_item_id: self.item().id,
            reason,
        });
        Ok(events)
    }

    fn job_exhausted(&self, kind: JobKind, error: Coded, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if kind == JobKind::EnvTeardown {
            return self.teardown_failed(error);
        }
        if kind == JobKind::SessionReattach {
            let gone = SessionInput::Gone {
                cause: "runner_silent".to_string(),
            };
            return self.session(gone, ctx).unwrap_or_default();
        }
        // A toolchain download that kept failing names the tool, not the job.
        if kind == JobKind::EnvPrepare && error.code == "toolchain_unavailable" {
            return self.environment_failed(BlockerCode::ToolchainUnavailable, error.params, ctx);
        }
        if kind == JobKind::ScmPollChangeRequest && self.task.item.stage != Some(Stage::Merge) {
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

    fn state_name(&self) -> String {
        self.item().state.as_str().to_string()
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
        | B::BudgetExhausted
        | B::QuotaExhausted
        | B::NoChanges => (Stage::Dev, StagePhase::RunningSession),
        B::CiRed if params.get("reason").and_then(Value::as_str) == Some("runner_failed") => {
            (Stage::Merge, StagePhase::AwaitingCi)
        }
        B::CiRed => (Stage::Dev, StagePhase::RunningSession),
        B::PushRejected => (Stage::Dev, StagePhase::Pushing),
        B::MergeConflict => (Stage::Merge, StagePhase::AwaitingCi),
        B::DodFailed => (Stage::Sync, StagePhase::Verifying),
        B::ScmUnavailable => {
            let phase: StagePhase = params.get("phase").and_then(Value::as_str)?.parse().ok()?;
            let stage = match phase {
                StagePhase::PreparingEnvironment
                | StagePhase::RunningSession
                | StagePhase::Pushing
                | StagePhase::OpeningChange => Stage::Dev,
                StagePhase::AwaitingCi | StagePhase::AwaitingMerge | StagePhase::Merging => {
                    Stage::Merge
                }
                StagePhase::Verifying | StagePhase::TearingDown => Stage::Sync,
            };
            // Merging again needs a fresh look at the change and its checks.
            let phase = if phase == StagePhase::Merging {
                StagePhase::AwaitingCi
            } else {
                phase
            };
            (stage, phase)
        }
        _ => return None,
    })
}

pub fn evolve(task: Option<Task>, event: &Event, now: Timestamp) -> Option<Task> {
    if let Event::WorkItemCreated {
        work_item_id,
        project_id,
        spec_id,
        spec_revision_id,
        step_id,
        step_index,
        task_key,
        work,
        branch,
        repo_id,
        priority,
    } = event
    {
        return Some(Task::new(WorkItem {
            id: *work_item_id,
            project_id: *project_id,
            origin: WorkItemOrigin::SpecStep,
            spec_id: Some(*spec_id),
            spec_revision_id: Some(*spec_revision_id),
            step_id: Some(step_id.clone()),
            step_index: Some(*step_index),
            task_key: task_key.clone(),
            work: work.clone(),
            repo_id: *repo_id,
            branch: branch.clone(),
            state: WorkItemState::Queued,
            stage: None,
            stage_phase: None,
            priority: *priority,
            version: 1,
            admitted_at: None,
            done_at: None,
            created_at: now,
            updated_at: now,
        }));
    }
    let mut t = task?;
    if event.work_item_id() != Some(t.item.id) {
        return Some(t);
    }
    t.item.version += 1;
    t.item.updated_at = now;
    apply(&mut t, event, now);
    Some(t)
}

fn set_place(item: &mut WorkItem, state: WorkItemState, place: Option<(Stage, StagePhase)>) {
    item.state = state;
    item.stage = place.map(|p| p.0);
    item.stage_phase = place.map(|p| p.1);
}

fn apply(t: &mut Task, event: &Event, now: Timestamp) {
    use Event as E;
    if let E::SessionStarted {
        owner,
        resume_cause: Some(ResumeCause::Lost),
        ..
    } = event
        && let Some(run) = t
            .stage_run
            .as_mut()
            .filter(|r| owner.stage_run_id() == Some(r.id))
    {
        run.resumed_lost = true;
    }
    if session::apply(t.run_parts(), event, now) {
        return;
    }
    match event {
        E::WorkItemRepinned {
            spec_revision_id,
            step_index,
            work,
            priority,
            restored,
            ..
        } => {
            t.item.spec_revision_id = Some(*spec_revision_id);
            t.item.step_index = Some(*step_index);
            t.item.work.clone_from(work);
            t.item.priority = *priority;
            if *restored {
                set_place(&mut t.item, WorkItemState::Queued, None);
            }
        }
        E::WorkItemSuperseded { .. } => {
            set_place(&mut t.item, WorkItemState::Superseded, None);
        }
        E::WorkItemAdmitted { .. } => {
            t.item.state = WorkItemState::Admitted;
            t.item.admitted_at = Some(now);
        }
        E::StageRunCreated {
            stage_run_id,
            stage,
            attempt,
            trigger,
            entry_phase,
            reason,
            budget,
            ..
        } => {
            count_retry(&mut t.retries, *stage, *trigger, reason.as_deref());
            let retired = t.stage_run.as_ref().map(|r| r.id);
            if let Some(old) = t.stage_run.take() {
                t.retired_runs.push(old);
            }
            t.prior_usage = Usage::default();
            let waiting = *entry_phase == StagePhase::PreparingEnvironment;
            t.stage_run = Some(StageRun {
                id: *stage_run_id,
                work_item_id: t.item.id,
                stage: *stage,
                attempt: *attempt,
                state: if waiting {
                    StageRunState::Pending
                } else {
                    StageRunState::Running
                },
                trigger: *trigger,
                reason: reason.clone(),
                entry_phase: *entry_phase,
                head_sha: None,
                failure: None,
                budget: *budget,
                budget_warnings: Vec::new(),
                resumed_lost: false,
                started_at: if waiting { None } else { Some(now) },
                ended_at: None,
                created_at: now,
                updated_at: now,
            });
            t.attempts.insert(*stage, *attempt);
            let merged_head = t.change_request.as_ref().map(|c| c.head_sha.clone());
            if *stage == Stage::Sync {
                t.gates.retain(|g| {
                    Some(g.stage_run_id) == retired && Some(&g.head_sha) == merged_head.as_ref()
                });
            } else {
                t.gates.clear();
            }
            if waiting {
                set_place(&mut t.item, WorkItemState::Admitted, None);
            } else {
                set_place(
                    &mut t.item,
                    WorkItemState::InStage,
                    Some((*stage, *entry_phase)),
                );
            }
        }
        E::StagePhaseChanged {
            phase, head_sha, ..
        } => {
            if let Some(run) = &mut t.stage_run {
                if run.stage == Stage::Sync && head_sha.is_some() && run.head_sha != *head_sha {
                    t.gates.clear();
                }
                run.state = StageRunState::Running;
                run.started_at.get_or_insert(now);
                if head_sha.is_some() {
                    run.head_sha.clone_from(head_sha);
                }
                run.updated_at = now;
                let stage = run.stage;
                set_place(&mut t.item, WorkItemState::InStage, Some((stage, *phase)));
            }
        }
        E::StageRunSucceeded {
            stage_run_id,
            head_sha,
            ..
        } => {
            if let Some(run) = t.stage_run.as_mut().filter(|r| r.id == *stage_run_id) {
                run.state = StageRunState::Succeeded;
                if head_sha.is_some() {
                    run.head_sha.clone_from(head_sha);
                }
                run.ended_at = Some(now);
                run.updated_at = now;
            }
        }
        E::GateObserved {
            stage_run_id,
            gate_result_id,
            source,
            head_sha,
            outcome,
            checks,
            ..
        } => {
            t.gates.retain(|g| g.source != *source);
            t.gates.insert(
                t.gates.partition_point(|g| g.source < *source),
                GateResult {
                    id: *gate_result_id,
                    stage_run_id: *stage_run_id,
                    work_item_id: t.item.id,
                    source: *source,
                    head_sha: head_sha.clone(),
                    outcome: *outcome,
                    checks: checks.clone(),
                    observed_at: now,
                },
            );
        }
        E::GateStarted {
            stage_run_id,
            gate_result_id,
            source,
            head_sha,
            ..
        } => {
            t.gates.retain(|g| g.source != *source);
            t.gates.insert(
                t.gates.partition_point(|g| g.source < *source),
                GateResult {
                    id: *gate_result_id,
                    stage_run_id: *stage_run_id,
                    work_item_id: t.item.id,
                    source: *source,
                    head_sha: head_sha.clone(),
                    outcome: GateOutcome::Pending,
                    checks: Vec::new(),
                    observed_at: now,
                },
            );
        }
        E::MergeRequested { .. } => {}
        E::WorkItemBlocked {
            stage_run_id,
            blocker_id,
            code,
            params,
            ..
        } => {
            t.blocker = Some(Blocker {
                id: *blocker_id,
                work_item_id: t.item.id,
                stage_run_id: *stage_run_id,
                code: *code,
                params: params.clone(),
                resolution: None,
                opened_at: now,
                resolved_at: None,
            });
            if let Some(run) = &mut t.stage_run {
                run.state = StageRunState::Blocked;
                run.failure = Some(Coded {
                    code: code.as_str().to_string(),
                    params: params.clone(),
                });
                run.ended_at = Some(now);
                run.updated_at = now;
            }
            set_place(&mut t.item, WorkItemState::Blocked, None);
        }
        E::BlockerResolved {
            blocker_id,
            resolution,
            ..
        } => {
            if let Some(b) = t.blocker.as_mut().filter(|b| b.id == *blocker_id) {
                b.resolution = Some(*resolution);
                b.resolved_at = Some(now);
            }
        }
        E::WorkItemCancelled { .. } => {
            if let Some(run) = t
                .stage_run
                .as_mut()
                .filter(|r| matches!(r.state, StageRunState::Pending | StageRunState::Running))
            {
                run.state = StageRunState::Cancelled;
                run.ended_at = Some(now);
                run.updated_at = now;
            }
            set_place(&mut t.item, WorkItemState::Cancelled, None);
        }
        E::WorkItemDone { .. } => {
            set_place(&mut t.item, WorkItemState::Done, None);
            t.item.done_at = Some(now);
        }
        E::BudgetWarned {
            owner, dimension, ..
        } => {
            if let Some(run) = t
                .stage_run
                .as_mut()
                .filter(|r| owner.stage_run_id() == Some(r.id))
                && !run.budget_warnings.contains(dimension)
            {
                run.budget_warnings.push(*dimension);
                run.updated_at = now;
            }
        }
        E::PermissionRuleAdded { .. } => {
            t.added_rules.extend(crate::rule::evolve(None, event, now));
        }
        E::PermissionRuleRevoked { .. } => {}
        E::BranchPushed { .. } => {}
        E::ChangeRequestOpened {
            change_request_id,
            repo_id,
            number,
            url,
            head_branch,
            base_branch,
            head_sha,
            ..
        } => {
            t.change_request = Some(ChangeRequest {
                id: *change_request_id,
                work_item_id: t.item.id,
                repo_id: *repo_id,
                number: *number,
                url: url.clone(),
                head_branch: head_branch.clone(),
                base_branch: base_branch.clone(),
                head_sha: head_sha.clone(),
                state: ChangeState::Open,
                merge_commit_sha: None,
                merged_at: None,
                last_polled_at: None,
                created_at: now,
                updated_at: now,
            });
        }
        E::ChangeRequestUpdated { head_sha, .. } => {
            if let Some(cr) = &mut t.change_request {
                cr.head_sha.clone_from(head_sha);
                cr.updated_at = now;
            }
        }
        E::ChangeRequestMerged {
            merge_commit_sha,
            merged_at,
            ..
        } => {
            if let Some(cr) = &mut t.change_request {
                cr.state = ChangeState::Merged;
                cr.merge_commit_sha = Some(merge_commit_sha.clone());
                cr.merged_at = merged_at.or(Some(now));
                cr.updated_at = now;
            }
            if let Some(run) = t.stage_run.as_mut().filter(|r| {
                r.stage == Stage::Dev
                    && matches!(r.state, StageRunState::Pending | StageRunState::Running)
            }) {
                run.state = StageRunState::Cancelled;
                run.ended_at = Some(now);
                run.updated_at = now;
            }
        }
        E::ChangeRequestClosed { .. } => {
            if let Some(cr) = &mut t.change_request {
                cr.state = ChangeState::Closed;
                cr.updated_at = now;
            }
        }
        E::WorkItemCreated { .. }
        | E::RepoRegistered { .. }
        | E::ProjectRegistered { .. }
        | E::ProjectConfigChanged { .. }
        | E::ProjectConfigRejected { .. }
        | E::ProjectConfigErrorCleared { .. }
        | E::SecretSet { .. }
        | E::SecretRemoved { .. }
        | E::DocCommitted { .. }
        | E::SpecRegistered { .. }
        | E::SpecRevisionRecorded { .. }
        | E::SpecApproved { .. }
        | E::SpecWithdrawn { .. }
        | E::SpecShipped { .. }
        | E::JobExhausted { .. }
        | E::SpecDraftRequested { .. }
        | E::SpecDraftAttemptStarted { .. }
        | E::SpecDraftPacketRendered { .. }
        | E::SpecDraftLinted { .. }
        | E::SpecDraftCommitting { .. }
        | E::SpecDraftSucceeded { .. }
        | E::SpecDraftFailed { .. }
        | E::SpecDraftRetried { .. }
        | E::SpecDraftCancelled { .. } => {}
        E::EnvironmentPrepared { .. }
        | E::EnvironmentTornDown { .. }
        | E::EnvironmentTeardownFailed { .. }
        | E::SessionStarted { .. }
        | E::SessionInitialized { .. }
        | E::SessionStateChanged { .. }
        | E::SessionTurnRequested { .. }
        | E::SessionTurnEnded { .. }
        | E::SessionSteered { .. }
        | E::SessionInterruptRequested { .. }
        | E::QuotaObserved { .. }
        | E::SessionReattached { .. }
        | E::SessionStopRequested { .. }
        | E::SessionEnded { .. }
        | E::SessionLost { .. }
        | E::PermissionRequested { .. }
        | E::PermissionAnswered { .. }
        | E::PermissionDelivered { .. } => {}
    }
}

pub fn gates_clear(task: &Task) -> bool {
    let Some(cr) = &task.change_request else {
        return false;
    };
    let at_head: Vec<&GateResult> = task
        .gates
        .iter()
        .filter(|g| g.head_sha == cr.head_sha)
        .collect();
    !at_head.is_empty()
        && at_head.iter().all(|g| g.outcome != GateOutcome::Failed)
        && at_head.iter().any(|g| g.outcome == GateOutcome::Passed)
}

pub fn available_actions(task: &Task) -> Vec<&'static str> {
    let mut actions = Vec::new();
    let merge_ready = task.is_at(Stage::Merge, StagePhase::AwaitingMerge) && gates_clear(task);
    if merge_ready {
        actions.push("merge");
    }
    let retryable = task.item.state == WorkItemState::Blocked
        && task.open_blocker().is_some_and(|b| {
            retry_entry(b.code, &b.params).is_some_and(|(stage, _)| {
                task.retries.get(&stage).map_or(0, |r| r.manual) < MANUAL_RETRY_LIMIT
            })
        });
    if retryable {
        actions.push("retry");
    }
    if matches!(
        task.item.state,
        WorkItemState::Queued
            | WorkItemState::Admitted
            | WorkItemState::InStage
            | WorkItemState::Blocked
    ) {
        actions.push("cancel");
    }
    actions
}

pub fn count_retry(
    retries: &mut BTreeMap<Stage, Retries>,
    stage: Stage,
    trigger: Trigger,
    reason: Option<&str>,
) {
    let entry = retries.entry(stage).or_default();
    match trigger {
        Trigger::Manual => {
            entry.manual += 1;
            entry.auto = 0;
        }
        Trigger::Auto if reason.is_some_and(|r| r.starts_with(AUTO_RETRY_PREFIX)) => {
            entry.auto += 1;
        }
        Trigger::Auto => {}
    }
}

pub fn auto_retryable(code: BlockerCode, params: &Value) -> bool {
    let text = |key: &str| params.get(key).and_then(Value::as_str);
    match code {
        BlockerCode::SessionLost | BlockerCode::ScmUnavailable => true,
        BlockerCode::EnvSetupFailed => {
            params.pointer("/error/code").and_then(Value::as_str) != Some("setup_command_failed")
        }
        BlockerCode::MergeConflict => text("reason") == Some("head_changed"),
        BlockerCode::CiRed => {
            text("source") == Some("saka") && text("reason") == Some("runner_failed")
        }
        BlockerCode::DodFailed => text("reason") == Some("runner_failed"),
        _ => false,
    }
}

pub fn auto_retry_due(task: &Task, now: Timestamp) -> bool {
    let Some(blocker) = task.open_blocker() else {
        return false;
    };
    if task.item.state != WorkItemState::Blocked || !auto_retryable(blocker.code, &blocker.params) {
        return false;
    }
    let Some((stage, _)) = retry_entry(blocker.code, &blocker.params) else {
        return false;
    };
    let done = task.retries.get(&stage).map_or(0, |r| r.auto);
    let Some(delay) = usize::try_from(done)
        .ok()
        .and_then(|i| AUTO_RETRY_DELAYS_MS.get(i))
    else {
        return false;
    };
    now.since(blocker.opened_at) >= *delay
}

pub fn watches_change(task: &Task) -> bool {
    let open = task
        .change_request
        .as_ref()
        .is_some_and(|c| c.state == ChangeState::Open);
    open && match task.item.state {
        WorkItemState::Blocked => true,
        WorkItemState::InStage => {
            task.item.stage == Some(Stage::Dev)
                || task.is_at(Stage::Merge, StagePhase::AwaitingCi)
                || task.is_at(Stage::Merge, StagePhase::AwaitingMerge)
        }
        _ => false,
    }
}

pub fn needs_tick(task: &Task, cfg: &LoopConfig, now: Timestamp) -> bool {
    struct Probe;
    impl crate::ids::IdSource for Probe {
        fn next_id(&mut self) -> uuid::Uuid {
            uuid::Uuid::nil()
        }
    }
    let mut ids = Probe;
    let mut ctx = Ctx { now, ids: &mut ids };
    decide(task, TaskInput::Tick, cfg, &mut ctx).is_ok_and(|events| !events.is_empty())
}

pub fn run_view(task: &Task) -> Option<RunView<'_>> {
    let run = task.stage_run.as_ref()?;
    let blocker = task.blocker.as_ref();
    let coded = |b: &Blocker| Coded {
        code: b.code.as_str().to_string(),
        params: b.params.clone(),
    };
    Some(RunView {
        owner: RunOwner::StageRun {
            work_item_id: task.item.id,
            stage_run_id: run.id,
        },
        project_id: task.item.project_id,
        session: task.session.as_ref(),
        permissions: &task.permissions,
        prior_usage: task.prior_usage,
        budget: run.budget,
        budget_warnings: &run.budget_warnings,
        resumed_lost: run.resumed_lost,
        ready_environment: task.ready_environment().map(|e| e.id),
        retry: RunRetry {
            trigger: run.trigger,
            retried: blocker
                .filter(|b| b.resolution.is_some())
                .map(|b| (b.code, b.params.clone())),
        },
        packet: Prompt::WorkPacket {
            retry_of: blocker
                .filter(|_| run.trigger == Trigger::Manual)
                .map(coded),
        },
    })
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[test]
    fn only_transient_blockers_are_retried_automatically() {
        let yes = [
            (BlockerCode::SessionLost, json!({})),
            (BlockerCode::ScmUnavailable, json!({ "phase": "pushing" })),
            (
                BlockerCode::EnvSetupFailed,
                json!({ "error": { "code": "worktree_failed" } }),
            ),
            (
                BlockerCode::MergeConflict,
                json!({ "reason": "head_changed" }),
            ),
            (
                BlockerCode::CiRed,
                json!({ "source": "saka", "reason": "runner_failed" }),
            ),
        ];
        for (code, params) in yes {
            assert!(auto_retryable(code, &params), "{code}");
        }
        let no = [
            (
                BlockerCode::CiRed,
                json!({ "source": "scm", "checks": ["test"] }),
            ),
            (
                BlockerCode::CiRed,
                json!({ "source": "saka", "command": "make test" }),
            ),
            (BlockerCode::PushRejected, json!({})),
            (BlockerCode::NoChanges, json!({})),
            (BlockerCode::BudgetExhausted, json!({})),
            (
                BlockerCode::MergeConflict,
                json!({ "reason": "not_mergeable" }),
            ),
            (
                BlockerCode::EnvSetupFailed,
                json!({ "error": { "code": "setup_command_failed" } }),
            ),
        ];
        for (code, params) in no {
            assert!(!auto_retryable(code, &params), "{code} {params}");
        }
    }

    #[test]
    fn a_runner_failure_re_runs_the_gate_instead_of_the_session() {
        let params = json!({ "source": "saka", "reason": "runner_failed" });
        assert_eq!(
            retry_entry(BlockerCode::CiRed, &params),
            Some((Stage::Merge, StagePhase::AwaitingCi))
        );
        assert_eq!(
            retry_entry(
                BlockerCode::CiRed,
                &json!({ "source": "saka", "command": "x" })
            ),
            Some((Stage::Dev, StagePhase::RunningSession))
        );
    }

    #[test]
    fn manual_retries_reset_the_automatic_count() {
        let mut r = BTreeMap::new();
        count_retry(&mut r, Stage::Merge, Trigger::Auto, None);
        assert_eq!(r[&Stage::Merge], Retries::default());
        count_retry(
            &mut r,
            Stage::Merge,
            Trigger::Auto,
            Some("auto retry: ci_red"),
        );
        count_retry(
            &mut r,
            Stage::Merge,
            Trigger::Auto,
            Some("auto retry: ci_red"),
        );
        assert_eq!(r[&Stage::Merge], Retries { auto: 2, manual: 0 });
        count_retry(&mut r, Stage::Merge, Trigger::Manual, Some("try again"));
        assert_eq!(r[&Stage::Merge], Retries { auto: 0, manual: 1 });
    }
}
