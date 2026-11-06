use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::Ctx;
use crate::authorship::CredentialBlock;
use crate::blocker::primary_action;
use crate::event::{Event, Prompt};
use crate::external;
use crate::gate::{self, Check, GatePolicy};
use crate::ids::{
    BlockerId, ChangeRequestId, EnvironmentId, GateResultId, ReviewId, SessionId, StageRunId,
};
use crate::job::{JobKind, exhausted_code};
use crate::model::{
    Blocker, Budget, ChangeRequest, Coded, EnvironmentSetup, ExternalChange, GateResult, Retries,
    Review, ReviewPosted, RunOwner, StageRun, Task, Usage, WorkItem,
};
use crate::rejection::Rejection;
use crate::review;
use crate::scope::ScopeViolation;
use crate::session::{
    self, RunRetry, RunView, SessionConfig, SessionInput, SessionOutcome, SessionSignal,
};
use crate::time::Timestamp;
use crate::vocab::{
    BlockerAction, BlockerCode, BlockerResolution, ChangeCredentialKind, ChangeRole, ChangeState,
    EnvironmentState, ExternalCause, GateOutcome, GateSource, HookRunState, MergeMethod,
    ResumeCause, ResumeTrigger, ReviewBasis, ReviewEvent, ReviewOutcome, ScopeStage, SessionState,
    Stage, StagePhase, StageRunState, Trigger, WorkItemOrigin, WorkItemState,
};
use crate::workflow::Workflow;

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
pub struct DocsInspection {
    pub dirty: bool,
    pub commits_ahead: u32,
    pub head_sha: String,
    pub outside: Option<ScopeViolation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservedChange {
    pub state: ChangeState,
    pub head_sha: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskInput {
    Admit,
    EnvironmentPrepared {
        environment_id: EnvironmentId,
        root_path: String,
        worktree_path: String,
        base_sha: String,
        setup: Box<EnvironmentSetup>,
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
        outside: Option<ScopeViolation>,
        docs: Option<DocsInspection>,
    },
    PathScopeViolated {
        role: ChangeRole,
        head_sha: String,
        violation: ScopeViolation,
    },
    Tick,
    BranchPushed {
        target: ChangeRole,
        sha: String,
    },
    PushRejected {
        target: ChangeRole,
        remote_sha: String,
        local_sha: String,
    },
    ChangeOpened {
        role: ChangeRole,
        change_request_id: ChangeRequestId,
        number: u64,
        url: String,
        head_sha: String,
        base_branch: String,
        opened_with: ChangeCredentialKind,
    },
    CredentialBlocked {
        block: CredentialBlock,
        target: ChangeRole,
    },
    SakaGateStarted {
        head_sha: String,
    },
    AutoRetry,
    Resume {
        trigger: ResumeTrigger,
    },
    SakaGateObserved {
        head_sha: String,
        checks: Vec<Check>,
        failure: Option<Value>,
    },
    ChangeObserved {
        role: ChangeRole,
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
        role: ChangeRole,
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
    },
    MergeRejected {
        role: ChangeRole,
        reason: String,
    },
    MergeVerified {
        found: bool,
        base_tip: Option<String>,
        docs_found: Option<bool>,
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
    ResolveBlocker {
        blocker_id: BlockerId,
        expected: Option<BlockerAction>,
        reason: String,
    },
    JobExhausted {
        kind: JobKind,
        error: Coded,
    },
    ReviewRequested {
        head_sha: String,
        cause: ExternalCause,
        actor: Option<String>,
    },
    Takeover {
        reason: String,
    },
    Return {
        stage: Option<Stage>,
        reason: String,
        change: Option<ObservedChange>,
    },
    StartReview,
    ReviewEnvironmentPrepared {
        environment_id: EnvironmentId,
        root_path: String,
        worktree_path: String,
        head_sha: String,
    },
    ReviewReported {
        session_id: SessionId,
        turn: u32,
        text: String,
    },
    ReviewPosted {
        review_id: ReviewId,
        event: ReviewEvent,
        url: Option<String>,
    },
}

pub fn decide(
    task: &Task,
    input: TaskInput,
    cfg: &LoopConfig,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<Event>, Rejection> {
    if task.item.origin == WorkItemOrigin::ExternalChange {
        match input {
            TaskInput::ChangeObserved { .. } | TaskInput::ReviewRequested { .. } => {
                return external::decide(task, input, ctx);
            }
            TaskInput::Session(_)
            | TaskInput::StartReview
            | TaskInput::ReviewEnvironmentPrepared { .. }
            | TaskInput::EnvironmentFailed { .. }
            | TaskInput::ReviewReported { .. }
            | TaskInput::ReviewPosted { .. }
            | TaskInput::Tick
            | TaskInput::Retry { .. }
            | TaskInput::AutoRetry
            | TaskInput::Resume { .. }
            | TaskInput::ResolveBlocker { .. }
            | TaskInput::JobExhausted { .. }
            | TaskInput::EnvironmentTornDown { .. }
            | TaskInput::Cancel { .. } => {}
            _ => return Err(Rejection::Unsupported),
        }
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
    if task.item.state == WorkItemState::TakenOver {
        return d.paused(input, ctx);
    }
    Ok(match input {
        I::Admit => d.admit(ctx),
        I::EnvironmentPrepared {
            environment_id,
            root_path,
            worktree_path,
            base_sha,
            setup,
        } => d.environment_prepared(environment_id, root_path, worktree_path, base_sha, *setup),
        I::EnvironmentFailed { code, params } => d.environment_failed(code, params, ctx),
        I::Session(input) => return d.session(input, ctx),
        I::WorktreeInspected {
            turn,
            dirty,
            commits_ahead,
            head_sha,
            outside,
            docs,
        } => d.worktree_inspected(
            turn,
            Inspected {
                dirty,
                commits_ahead,
                head_sha,
                outside,
            },
            docs,
            ctx,
        ),
        I::PathScopeViolated {
            role: ChangeRole::Docs,
            head_sha,
            violation,
        } => {
            if task.is_at(Stage::Sync, StagePhase::MergingDocs) {
                d.block(
                    BlockerCode::PathScopeViolation,
                    with_target(violation.params(ScopeStage::Sync, &head_sha)),
                    ctx,
                )
            } else {
                vec![]
            }
        }
        I::PathScopeViolated {
            role: ChangeRole::Code,
            head_sha,
            violation,
        } => {
            if task.is_at(Stage::Merge, StagePhase::Merging) {
                d.block(
                    BlockerCode::PathScopeViolation,
                    violation.params(ScopeStage::Merge, &head_sha),
                    ctx,
                )
            } else {
                vec![]
            }
        }
        I::Tick => d.tick(ctx),
        I::BranchPushed {
            target: ChangeRole::Code,
            sha,
        } => d.branch_pushed(sha, ctx),
        I::BranchPushed {
            target: ChangeRole::Docs,
            sha,
        } => d.docs_pushed(sha),
        I::PushRejected {
            target,
            remote_sha,
            local_sha,
        } => {
            if task.is_at(Stage::Dev, StagePhase::Pushing) {
                d.block(
                    BlockerCode::PushRejected,
                    json!({ "target": target, "remote_sha": remote_sha, "local_sha": local_sha }),
                    ctx,
                )
            } else {
                vec![]
            }
        }
        I::ChangeOpened {
            role,
            change_request_id,
            number,
            url,
            head_sha,
            base_branch,
            opened_with,
        } => d.change_opened(
            Opened {
                role,
                id: change_request_id,
                number,
                url,
                head_sha,
                base_branch,
                opened_with,
            },
            ctx,
        ),
        I::CredentialBlocked { block, target } => d.credential_blocked(&block, target, ctx),
        I::ChangeObserved {
            role: ChangeRole::Code,
            state,
            head_sha,
            merge_commit_sha,
            merged_at,
            checks,
        } => d.change_observed(state, head_sha, merge_commit_sha, merged_at, checks, ctx),
        I::ChangeObserved {
            role: ChangeRole::Docs,
            state,
            head_sha,
            merge_commit_sha,
            merged_at,
            ..
        } => d.docs_observed(state, head_sha, merge_commit_sha, merged_at, false),
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
            role: ChangeRole::Docs,
            merge_commit_sha,
            merged_at,
        } => d.docs_observed(
            ChangeState::Merged,
            String::new(),
            Some(merge_commit_sha),
            merged_at,
            true,
        ),
        I::MergeRejected {
            role: ChangeRole::Docs,
            reason,
        } => d.docs_merge_rejected(reason, ctx),
        I::MergeSucceeded {
            role: ChangeRole::Code,
            merge_commit_sha,
            merged_at,
        } => {
            if task.is_at(Stage::Merge, StagePhase::Merging) {
                d.merged(merge_commit_sha, merged_at, true, ctx)
            } else {
                vec![]
            }
        }
        I::MergeRejected {
            role: ChangeRole::Code,
            reason,
        } => {
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
        I::MergeVerified {
            found,
            base_tip,
            docs_found,
        } => d.merge_verified(found, base_tip, docs_found, ctx),
        I::CommitChecksObserved { head_sha, checks } => {
            d.commit_checks_observed(head_sha, checks, ctx)
        }
        I::EnvironmentTornDown { teardown_failures } => d.environment_torn_down(teardown_failures),
        I::Retry { reason } => return d.retry(reason, ctx),
        I::AutoRetry => d.auto_retry(ctx),
        I::Resume { trigger } => d.resume(trigger, ctx),
        I::Cancel { reason } => return d.cancel(reason),
        I::ResolveBlocker {
            blocker_id,
            expected,
            reason,
        } => {
            return d.resolve_blocker(blocker_id, expected, reason, ctx);
        }
        I::JobExhausted { kind, error } => d.job_exhausted(kind, error, ctx),
        I::ReviewRequested { .. } => return Err(Rejection::Unsupported),
        I::Takeover { reason } => return d.takeover(reason),
        I::Return { .. } => {
            return Err(Rejection::InvalidState {
                state: d.state_name(),
            });
        }
        I::StartReview => d.start_review(),
        I::ReviewEnvironmentPrepared {
            environment_id,
            root_path,
            worktree_path,
            head_sha,
        } => d.review_environment_prepared(environment_id, root_path, worktree_path, head_sha),
        I::ReviewReported {
            session_id,
            turn,
            text,
        } => d.review_reported(session_id, turn, &text, ctx),
        I::ReviewPosted {
            review_id,
            event,
            url,
        } => d.review_posted(review_id, event, url),
    })
}

const RETURN_STAGES: [Stage; 2] = [Stage::Dev, Stage::Merge];

pub fn takeable(stage: Stage, phase: Option<StagePhase>) -> bool {
    stage != Stage::Sync && phase != Some(StagePhase::Merging)
}

fn takeover_place(task: &Task) -> Result<(Option<Stage>, Option<StagePhase>), Rejection> {
    if task.item.origin != WorkItemOrigin::SpecStep {
        return Err(Rejection::Unsupported);
    }
    let place = match task.item.state {
        WorkItemState::InStage => (task.item.stage, task.item.stage_phase),
        WorkItemState::Blocked => (task.stage_run.as_ref().map(|r| r.stage), None),
        other => {
            return Err(Rejection::InvalidState {
                state: other.as_str().to_string(),
            });
        }
    };
    match place {
        (Some(stage), phase) if !takeable(stage, phase) => Err(Rejection::StageNotTakeable {
            stage: stage.as_str(),
            phase: phase.map(StagePhase::as_str),
        }),
        place => Ok(place),
    }
}

pub fn can_takeover(task: &Task) -> bool {
    takeover_place(task).is_ok()
}

pub fn return_entry(stage: Stage, environment_ready: bool) -> Option<StagePhase> {
    match stage {
        Stage::Dev if environment_ready => Some(StagePhase::RunningSession),
        Stage::Dev => Some(StagePhase::PreparingEnvironment),
        Stage::Merge => Some(StagePhase::AwaitingCi),
        Stage::Review | Stage::Fix | Stage::Sync => None,
    }
}

struct Inspected {
    dirty: bool,
    commits_ahead: u32,
    head_sha: String,
    outside: Option<ScopeViolation>,
}

fn with_target(mut params: Value) -> Value {
    params["target"] = json!(ChangeRole::Docs);
    params
}

struct Opened {
    role: ChangeRole,
    id: ChangeRequestId,
    number: u64,
    url: String,
    head_sha: String,
    base_branch: String,
    opened_with: ChangeCredentialKind,
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
            head_sha: None,
        }
    }

    fn phase(&self, phase: StagePhase, head_sha: Option<String>) -> Vec<Event> {
        self.phase_docs(phase, head_sha, None)
    }

    fn phase_docs(
        &self,
        phase: StagePhase,
        head_sha: Option<String>,
        docs_head_sha: Option<String>,
    ) -> Vec<Event> {
        match self.run_id() {
            Some(stage_run_id) => {
                vec![Event::StagePhaseChanged {
                    work_item_id: self.item().id,
                    stage_run_id,
                    phase,
                    head_sha,
                    docs_head_sha,
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
        if matches!(input, SessionInput::Launched { .. }) && !self.running_agent() {
            return Ok(vec![]);
        }
        let outcome = session::decide(&view, input, &self.cfg.session(), ctx)?;
        Ok(self.settle(outcome, ctx))
    }

    fn running_agent(&self) -> bool {
        self.task.is_at(Stage::Dev, StagePhase::RunningSession)
            || self.task.is_at(Stage::Review, StagePhase::RunningSession)
    }

    fn settle(&self, outcome: SessionOutcome, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let mut events = outcome.events;
        match outcome.signal {
            // The finished turn's worktree is inspected by the job `react` schedules for it.
            None | Some(SessionSignal::TurnCompleted { .. }) => {}
            Some(SessionSignal::Stopped {
                code: BlockerCode::SessionLost,
                ..
            }) if !self.running_agent() => {}
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
            path_scope: setup.path_scope,
            docs: setup.docs.map(Box::new),
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
        if !self.preparing()
            && !self
                .task
                .is_at(Stage::Review, StagePhase::PreparingEnvironment)
        {
            return vec![];
        }
        self.block(code, params, ctx)
    }

    fn worktree_inspected(
        &self,
        turn: u32,
        code: Inspected,
        docs: Option<DocsInspection>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Inspected {
            dirty,
            commits_ahead,
            head_sha,
            outside,
        } = code;
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
        let committed = !dirty && commits_ahead > 0;
        let docs_dirty = docs.as_ref().is_some_and(|d| d.dirty);
        let docs_violation = docs
            .as_ref()
            .filter(|d| !d.dirty && d.commits_ahead > 0)
            .and_then(|d| d.outside.clone().map(|v| (v, d.head_sha.clone())));
        if committed && outside.is_none() && !docs_dirty && docs_violation.is_none() {
            let docs_head = docs.filter(|d| d.commits_ahead > 0).map(|d| d.head_sha);
            let mut events = self.end_session(SessionState::Ended, "completed", json!({}));
            events.extend(self.phase_docs(StagePhase::Pushing, Some(head_sha), docs_head));
            return events;
        }
        let violation = outside.filter(|_| committed);
        let code_ok = committed && violation.is_none();
        let docs_path = self
            .task
            .environment
            .as_ref()
            .and_then(|e| e.docs.as_ref())
            .map(|d| d.path.clone())
            .unwrap_or_default();
        if session.followups < self.cfg.max_followup_turns {
            let Some(view) = run_view(self.task) else {
                return vec![];
            };
            let prompt = match (violation, &docs_violation) {
                (Some(v), _) => Prompt::OutOfScope {
                    paths: v.paths,
                    count: v.count,
                    scope: v.scope,
                },
                (None, _) if !code_ok => Prompt::Uncommitted {
                    dirty,
                    commits_ahead,
                },
                (None, Some((v, _))) if !docs_dirty => Prompt::DocsOutOfScope {
                    path: docs_path,
                    paths: v.paths.clone(),
                    count: v.count,
                    scope: v.scope.clone(),
                },
                (None, _) => Prompt::DocsUncommitted { path: docs_path },
            };
            let outcome =
                session::next_turn(&view, turn + 1, prompt, &self.cfg.session(), self.now);
            return self.settle(outcome, ctx);
        }
        if let Some(v) = violation {
            let params = v.params(ScopeStage::Dev, &head_sha);
            let mut events =
                self.end_session(SessionState::Ended, "path_scope_violation", params.clone());
            events.extend(self.block(BlockerCode::PathScopeViolation, params, ctx));
            return events;
        }
        if let Some((v, docs_head)) = docs_violation.filter(|_| code_ok && !docs_dirty) {
            let params = with_target(v.params(ScopeStage::Dev, &docs_head));
            let mut events =
                self.end_session(SessionState::Ended, "path_scope_violation", params.clone());
            events.extend(self.block(BlockerCode::PathScopeViolation, params, ctx));
            return events;
        }
        let params = json!({ "dirty": dirty, "commits": commits_ahead, "docs_dirty": docs_dirty });
        let mut events = self.end_session(SessionState::Ended, "no_changes", params.clone());
        events.extend(self.block(BlockerCode::NoChanges, params, ctx));
        events
    }

    fn start_review(&self) -> Vec<Event> {
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

    fn review_environment_prepared(
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

    fn review_has_spec(&self) -> bool {
        self.item().spec_id.is_some()
            || self
                .task
                .external
                .as_ref()
                .is_some_and(|e| e.spec.is_some())
    }

    fn review_basis(&self) -> ReviewBasis {
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

    fn review_reported(
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

    fn review_recorded(
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

    fn review_posted(
        &self,
        review_id: ReviewId,
        event: ReviewEvent,
        url: Option<String>,
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
        events.extend(self.phase(StagePhase::Reviewed, None));
        events.push(Event::StageRunSucceeded {
            work_item_id: self.item().id,
            stage_run_id: run.id,
            head_sha: run.head_sha.clone(),
        });
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
        self.settle_hook_gates(ctx)
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
        if !self.task.is_at(Stage::Dev, StagePhase::Pushing) || docs_pending(self.task).is_some() {
            return vec![];
        }
        let mut events = vec![Event::BranchPushed {
            work_item_id: self.item().id,
            target: ChangeRole::Code,
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
                        role: ChangeRole::Code,
                        head_sha: sha.clone(),
                    });
                }
                if needs_docs_change(self.task) {
                    events.extend(self.phase(StagePhase::OpeningChange, Some(sha)));
                } else {
                    events.extend(self.dev_done(sha, ctx));
                }
            }
            _ => events.extend(self.phase(StagePhase::OpeningChange, Some(sha))),
        }
        events
    }

    // Docs go first: the code push that follows is what moves the stage on.
    fn docs_pushed(&self, sha: String) -> Vec<Event> {
        let branch = self
            .task
            .environment
            .as_ref()
            .and_then(|e| e.docs.as_ref())
            .and_then(|d| d.branch.clone());
        let (true, Some(branch)) = (self.task.is_at(Stage::Dev, StagePhase::Pushing), branch)
        else {
            return vec![];
        };
        let mut events = vec![Event::BranchPushed {
            work_item_id: self.item().id,
            target: ChangeRole::Docs,
            branch,
            sha: sha.clone(),
        }];
        if let Some(cr) = self
            .task
            .docs_change
            .as_ref()
            .filter(|c| c.state == ChangeState::Open && c.head_sha != sha)
        {
            events.push(Event::ChangeRequestUpdated {
                work_item_id: self.item().id,
                change_request_id: cr.id,
                role: ChangeRole::Docs,
                head_sha: sha,
            });
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

    fn change_opened(&self, opened: Opened, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Opened {
            role,
            id,
            number,
            url,
            head_sha,
            base_branch,
            opened_with,
        } = opened;
        if !self.task.is_at(Stage::Dev, StagePhase::OpeningChange) {
            return vec![];
        }
        if role == ChangeRole::Docs {
            let docs = self.task.environment.as_ref().and_then(|e| e.docs.as_ref());
            let (Some(docs), Some(code), None) = (
                docs,
                self.task.change_request.as_ref(),
                &self.task.docs_change,
            ) else {
                return vec![];
            };
            let mut events = vec![Event::ChangeRequestOpened {
                work_item_id: self.item().id,
                change_request_id: id,
                role,
                repo_id: docs.repo_id,
                number,
                url,
                head_branch: docs.branch.clone().unwrap_or_default(),
                base_branch,
                head_sha,
                opened_with,
            }];
            let code_head = self
                .task
                .stage_run
                .as_ref()
                .and_then(|r| r.head_sha.clone())
                .unwrap_or_else(|| code.head_sha.clone());
            events.extend(self.dev_done(code_head, ctx));
            return events;
        }
        if self.task.change_request.is_some() {
            return vec![];
        }
        let mut events = vec![Event::ChangeRequestOpened {
            work_item_id: self.item().id,
            change_request_id: id,
            role,
            repo_id: self.item().repo_id,
            number,
            url,
            head_branch: self.item().branch.clone(),
            base_branch,
            head_sha: head_sha.clone(),
            opened_with,
        }];
        if needs_docs_change(self.task) {
            events.extend(self.phase(StagePhase::OpeningChange, Some(head_sha)));
        } else {
            events.extend(self.dev_done(head_sha, ctx));
        }
        events
    }

    fn credential_blocked(
        &self,
        block: &CredentialBlock,
        target: ChangeRole,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let phase = block.phase();
        let waiting = self.task.is_at(Stage::Dev, phase)
            && (phase != StagePhase::RunningSession || self.task.live_session().is_none());
        if !waiting {
            return vec![];
        }
        let params = match target {
            ChangeRole::Docs => with_target(block.params()),
            ChangeRole::Code => block.params(),
        };
        self.block(block.code(), params, ctx)
    }

    fn docs_observed(
        &self,
        state: ChangeState,
        head_sha: String,
        merge_commit_sha: Option<String>,
        merged_at: Option<Timestamp>,
        by_saka: bool,
    ) -> Vec<Event> {
        let Some(cr) = self
            .task
            .docs_change
            .as_ref()
            .filter(|c| c.state == ChangeState::Open)
        else {
            return vec![];
        };
        if self.item().state.is_terminal() {
            return vec![];
        }
        let id = self.item().id;
        let mut events = match state {
            ChangeState::Open if !head_sha.is_empty() && head_sha != cr.head_sha => {
                return vec![Event::ChangeRequestUpdated {
                    work_item_id: id,
                    change_request_id: cr.id,
                    role: ChangeRole::Docs,
                    head_sha,
                }];
            }
            ChangeState::Open => return vec![],
            ChangeState::Closed => vec![
                Event::ChangeRequestClosed {
                    work_item_id: id,
                    change_request_id: cr.id,
                    role: ChangeRole::Docs,
                },
                Event::DocsChangeSkipped {
                    work_item_id: id,
                    change_request_id: cr.id,
                    number: cr.number,
                    reason: "closed".into(),
                },
            ],
            ChangeState::Merged => {
                let Some(merge_commit_sha) = merge_commit_sha else {
                    return vec![];
                };
                vec![Event::ChangeRequestMerged {
                    work_item_id: id,
                    change_request_id: cr.id,
                    role: ChangeRole::Docs,
                    merge_commit_sha,
                    merged_at,
                    merged_by_saka: by_saka,
                }]
            }
        };
        if self.task.is_at(Stage::Sync, StagePhase::MergingDocs) {
            events.extend(self.phase(StagePhase::Verifying, None));
        }
        events
    }

    fn docs_merge_rejected(&self, reason: String, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Some(cr) = self
            .task
            .docs_change
            .as_ref()
            .filter(|_| self.task.is_at(Stage::Sync, StagePhase::MergingDocs))
        else {
            return vec![];
        };
        let status = merge_status(&reason);
        self.block(
            BlockerCode::DocsMergeFailed,
            json!({ "reason": reason, "number": cr.number, "head_sha": cr.head_sha, "status": status }),
            ctx,
        )
    }

    fn sync_entry(&self) -> StagePhase {
        if docs_open(self.task) {
            StagePhase::MergingDocs
        } else {
            StagePhase::Verifying
        }
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
                    role: ChangeRole::Code,
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
                role: ChangeRole::Code,
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
                role: ChangeRole::Code,
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
                self.sync_entry(),
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
            role: ChangeRole::Code,
            merge_commit_sha,
            merged_at,
            merged_by_saka: false,
        });
        events.push(self.new_run(
            Stage::Sync,
            Trigger::Auto,
            self.sync_entry(),
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
        docs_found: Option<bool>,
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
        if docs_found == Some(false) {
            let docs = self.task.docs_change.as_ref();
            return self.block(
                BlockerCode::DodFailed,
                json!({
                    "reason": "docs_not_in_base",
                    "merge_commit_sha": docs.and_then(|c| c.merge_commit_sha.clone()),
                    "base_branch": docs.map(|c| c.base_branch.clone()),
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

    fn gates_pass(&self) -> bool {
        self.task
            .hook_gates
            .iter()
            .all(|g| g.state == HookRunState::Succeeded)
    }

    fn settle_hook_gates(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let gates = &self.task.hook_gates;
        if !self.task.is_at(Stage::Sync, StagePhase::TearingDown)
            || gates.iter().any(|g| g.state == HookRunState::Pending)
        {
            return vec![];
        }
        if let Some(failed) = gates.iter().find(|g| g.state == HookRunState::Failed) {
            return self.block(
                BlockerCode::HookFailed,
                json!({
                    "hook": failed.hook,
                    "hook_run_id": failed.run_id,
                    "action": failed.action,
                    "error": failed.error,
                }),
                ctx,
            );
        }
        let torn_down = self.task.environment.as_ref().is_none_or(|e| {
            matches!(
                e.state,
                EnvironmentState::TornDown | EnvironmentState::TeardownFailed
            )
        });
        if torn_down {
            self.finish_sync()
        } else {
            vec![]
        }
    }

    fn finish_sync(&self) -> Vec<Event> {
        if !self.task.is_at(Stage::Sync, StagePhase::TearingDown) || !self.gates_pass() {
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

    // Resuming after the operator renewed a credential or changed the mode is not a blind
    // retry, so it never uses up the stage's automatic retries.
    fn resume(&self, trigger: ResumeTrigger, ctx: &mut Ctx<'_>) -> Vec<Event> {
        self.retry_as(Trigger::Auto, trigger.as_str().to_string(), ctx)
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
            // A review restarts from its request; only a failed post re-enters at the post.
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
        // A new run starts without a head; pushing again needs the commit the failed run produced.
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

    fn resolve_blocker(
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
            Some(other) => Err(Rejection::ActionNeedsInput {
                action: other.as_str(),
            }),
            None => Err(Rejection::BlockerNotOpen {
                blocker_id: blocker_id.0.to_string(),
            }),
        }
    }

    fn cancel(&self, reason: String) -> Result<Vec<Event>, Rejection> {
        if !matches!(
            self.item().state,
            WorkItemState::Queued
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
        let stale = match kind {
            JobKind::EnvPrepareReview => !self
                .task
                .is_at(Stage::Review, StagePhase::PreparingEnvironment),
            JobKind::ScmPostReview => !self.task.is_at(Stage::Review, StagePhase::PostingReview),
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

    // Late facts for the run the operator left are dropped; only returning or cancelling changes it.
    fn paused(&self, input: TaskInput, ctx: &mut Ctx<'_>) -> Result<Vec<Event>, Rejection> {
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

    fn takeover(&self, reason: String) -> Result<Vec<Event>, Rejection> {
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

    fn give_back(
        &self,
        stage: Option<Stage>,
        reason: String,
        change: Option<ObservedChange>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Vec<Event>, Rejection> {
        let last = self.task.stage_run.as_ref().map(|r| r.stage);
        let stage = stage.unwrap_or(match last {
            Some(stage) if RETURN_STAGES.contains(&stage) => stage,
            _ => Stage::Dev,
        });
        let env_ready = self.task.ready_environment().is_some();
        let (true, Some(entry)) = (
            RETURN_STAGES.contains(&stage),
            return_entry(stage, env_ready),
        ) else {
            return Err(Rejection::ReturnTargetInvalid {
                stage: stage.as_str(),
                reason: "stage_unsupported",
            });
        };
        let mut events = Vec::new();
        match &self.task.change_request {
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
                        head_sha: head,
                    });
                }
            }
            None if stage == Stage::Merge => {
                return Err(Rejection::ReturnTargetInvalid {
                    stage: stage.as_str(),
                    reason: "no_change_request",
                });
            }
            None => {}
        }
        events.push(Event::WorkItemReturned {
            work_item_id: self.item().id,
            reason: reason.clone(),
            stage,
        });
        events.push(self.new_run(stage, Trigger::Auto, entry, Some(reason), None, ctx));
        Ok(events)
    }

    fn state_name(&self) -> String {
        self.item().state.as_str().to_string()
    }
}

pub fn retry_target(task: &Task, code: BlockerCode, params: &Value) -> Option<(Stage, StagePhase)> {
    let reviewing = task
        .stage_run
        .as_ref()
        .is_some_and(|r| r.stage == Stage::Review);
    if !reviewing {
        return retry_entry(code, params);
    }
    use BlockerCode as B;
    match code {
        B::EnvSetupFailed
        | B::SessionFailed
        | B::SessionLost
        | B::SessionTimeout
        | B::PermissionExpired
        | B::BudgetExhausted
        | B::QuotaExhausted
        | B::SpecUnclear => Some((Stage::Review, StagePhase::AwaitingReview)),
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
        B::CiRed if params.get("reason").and_then(Value::as_str) == Some("runner_failed") => {
            (Stage::Merge, StagePhase::AwaitingCi)
        }
        B::CiRed => (Stage::Dev, StagePhase::RunningSession),
        B::PushRejected => (Stage::Dev, StagePhase::Pushing),
        B::MergeConflict => (Stage::Merge, StagePhase::AwaitingCi),
        B::DodFailed => (Stage::Sync, StagePhase::Verifying),
        B::HookFailed => (Stage::Sync, StagePhase::TearingDown),
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
                StagePhase::MergingDocs | StagePhase::Verifying | StagePhase::TearingDown => {
                    Stage::Sync
                }
                StagePhase::AwaitingTrigger
                | StagePhase::AwaitingReview
                | StagePhase::PostingReview
                | StagePhase::Reviewed => Stage::Review,
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
    if let Event::ExternalChangeAccepted {
        work_item_id,
        project_id,
        repo_id,
        task_key,
        title,
        branch,
        change_request_id,
        number,
        url,
        base_branch,
        head_sha,
        author,
        from_fork,
        draft,
        spec,
        cause,
        actor,
    } = event
    {
        let mut t = Task::new(WorkItem {
            id: *work_item_id,
            project_id: *project_id,
            origin: WorkItemOrigin::ExternalChange,
            spec_id: None,
            spec_revision_id: None,
            step_id: None,
            step_index: None,
            task_key: task_key.clone(),
            work: title.clone(),
            repo_id: *repo_id,
            branch: branch.clone(),
            state: WorkItemState::InStage,
            stage: Some(Stage::Review),
            stage_phase: Some(StagePhase::AwaitingTrigger),
            priority: 0,
            version: 1,
            admitted_at: None,
            done_at: None,
            created_at: now,
            updated_at: now,
        });
        t.change_request = Some(ChangeRequest {
            id: *change_request_id,
            work_item_id: *work_item_id,
            role: ChangeRole::Code,
            repo_id: *repo_id,
            number: *number,
            url: url.clone(),
            head_branch: branch.clone(),
            base_branch: base_branch.clone(),
            head_sha: head_sha.clone(),
            state: ChangeState::Open,
            merge_commit_sha: None,
            merged_at: None,
            skipped_at: None,
            last_polled_at: None,
            created_at: now,
            updated_at: now,
        });
        t.external = Some(ExternalChange {
            title: title.clone(),
            author: author.clone(),
            from_fork: *from_fork,
            draft: *draft,
            spec: spec.clone(),
            cause: *cause,
            actor: actor.clone(),
            created_at: now,
            spec_slug: None,
            workflow: Workflow::default(),
        });
        return Some(t);
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
        E::StageRunCancelled { stage_run_id, .. } => {
            if let Some(run) = t.stage_run.as_mut().filter(|r| r.id == *stage_run_id)
                && matches!(run.state, StageRunState::Pending | StageRunState::Running)
            {
                run.state = StageRunState::Cancelled;
                run.ended_at = Some(now);
                run.updated_at = now;
            }
            if t.item.state == WorkItemState::InStage && t.item.stage == Some(Stage::Review) {
                t.item.stage_phase = Some(StagePhase::AwaitingTrigger);
            }
        }
        E::WorkItemRepinned {
            spec_revision_id,
            step_index,
            work,
            repo_id,
            priority,
            restored,
            ..
        } => {
            t.item.spec_revision_id = Some(*spec_revision_id);
            t.item.step_index = Some(*step_index);
            t.item.work.clone_from(work);
            t.item.repo_id = *repo_id;
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
            head_sha,
            ..
        } => {
            count_retry(&mut t.retries, *stage, *trigger, reason.as_deref());
            let retired = t.stage_run.as_ref().map(|r| r.id);
            if let Some(mut old) = t.stage_run.take() {
                if old.stage == Stage::Review && old.state == StageRunState::Pending {
                    old.state = StageRunState::Cancelled;
                    old.ended_at = Some(now);
                    old.updated_at = now;
                }
                t.retired_runs.push(old);
            }
            t.prior_usage = Usage::default();
            let queued = *entry_phase == StagePhase::AwaitingReview;
            let waiting = *entry_phase == StagePhase::PreparingEnvironment || queued;
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
                head_sha: head_sha.clone(),
                docs_head_sha: None,
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
            if queued {
                set_place(
                    &mut t.item,
                    WorkItemState::InStage,
                    Some((*stage, *entry_phase)),
                );
            } else if waiting {
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
            phase,
            head_sha,
            docs_head_sha,
            ..
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
                if docs_head_sha.is_some() {
                    run.docs_head_sha.clone_from(docs_head_sha);
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
        E::ReviewRecorded {
            review_id,
            stage_run_id,
            session_id,
            head_sha,
            outcome,
            basis,
            summary,
            findings,
            questions,
            ..
        } => t.reviews.push(Review {
            id: *review_id,
            work_item_id: t.item.id,
            stage_run_id: *stage_run_id,
            session_id: *session_id,
            head_sha: head_sha.clone(),
            outcome: *outcome,
            basis: *basis,
            summary: summary.clone(),
            findings: findings.clone(),
            questions: questions.clone(),
            posted: None,
            created_at: now,
        }),
        E::ReviewPosted {
            review_id,
            event,
            url,
            ..
        } => {
            if let Some(review) = t.reviews.iter_mut().find(|r| r.id == *review_id) {
                review.posted = Some(ReviewPosted {
                    event: *event,
                    url: url.clone(),
                    at: now,
                });
            }
        }
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
        E::WorkItemTakenOver { .. } => {
            if let Some(run) = t
                .stage_run
                .as_mut()
                .filter(|r| matches!(r.state, StageRunState::Pending | StageRunState::Running))
            {
                run.state = StageRunState::Cancelled;
                run.ended_at = Some(now);
                run.updated_at = now;
            }
            set_place(&mut t.item, WorkItemState::TakenOver, None);
        }
        E::WorkItemReturned { .. } => {}
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
        E::BranchPushed {
            target: ChangeRole::Docs,
            sha,
            ..
        } => {
            if let Some(docs) = t.environment.as_mut().and_then(|e| e.docs.as_mut()) {
                docs.pushed_sha = Some(sha.clone());
            }
        }
        E::BranchPushed { .. } => {}
        E::ChangeRequestOpened {
            change_request_id,
            role,
            repo_id,
            number,
            url,
            head_branch,
            base_branch,
            head_sha,
            ..
        } => {
            let cr = Some(ChangeRequest {
                id: *change_request_id,
                work_item_id: t.item.id,
                role: *role,
                repo_id: *repo_id,
                number: *number,
                url: url.clone(),
                head_branch: head_branch.clone(),
                base_branch: base_branch.clone(),
                head_sha: head_sha.clone(),
                state: ChangeState::Open,
                merge_commit_sha: None,
                merged_at: None,
                skipped_at: None,
                last_polled_at: None,
                created_at: now,
                updated_at: now,
            });
            match role {
                ChangeRole::Code => t.change_request = cr,
                ChangeRole::Docs => t.docs_change = cr,
            }
        }
        E::ChangeRequestUpdated { role, head_sha, .. } => {
            if let Some(cr) = t.change_of(*role) {
                cr.head_sha.clone_from(head_sha);
                cr.updated_at = now;
            }
        }
        E::ChangeRequestMerged {
            role,
            merge_commit_sha,
            merged_at,
            ..
        } => {
            if let Some(cr) = t.change_of(*role) {
                cr.state = ChangeState::Merged;
                cr.merge_commit_sha = Some(merge_commit_sha.clone());
                cr.merged_at = merged_at.or(Some(now));
                cr.updated_at = now;
            }
            if let Some(run) = t.stage_run.as_mut().filter(|r| {
                *role == ChangeRole::Code
                    && r.stage == Stage::Dev
                    && matches!(r.state, StageRunState::Pending | StageRunState::Running)
            }) {
                run.state = StageRunState::Cancelled;
                run.ended_at = Some(now);
                run.updated_at = now;
            }
        }
        E::ChangeRequestClosed { role, .. } => {
            if let Some(cr) = t.change_of(*role) {
                cr.state = ChangeState::Closed;
                cr.updated_at = now;
            }
        }
        E::DocsChangeSkipped { .. } => {
            if let Some(cr) = &mut t.docs_change {
                cr.skipped_at = Some(now);
                cr.updated_at = now;
            }
        }
        E::WorkItemCreated { .. }
        | E::ExternalChangeAccepted { .. }
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
        | E::SpecDraftCancelled { .. }
        | E::UserLinkAuthorizationStarted { .. }
        | E::UserLinkAuthorizationSlowed { .. }
        | E::UserLinkAuthorizationEnded { .. }
        | E::UserLinked { .. }
        | E::UserLinkRefreshed { .. }
        | E::UserLinkRelinkRequired { .. }
        | E::UserLinkRemoved { .. }
        | E::HookRunQueued { .. }
        | E::HookRunFinished { .. }
        | E::ProjectAuthorshipSet { .. }
        | E::ProjectUserTokenSet { .. }
        | E::ProjectUserTokenRemoved { .. }
        | E::ProjectUserTokenInvalidated { .. } => {}
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

pub fn can_retry(task: &Task) -> bool {
    task.item.state == WorkItemState::Blocked
        && task.open_blocker().is_some_and(|b| {
            retry_target(task, b.code, &b.params).is_some_and(|(stage, _)| {
                task.retries.get(&stage).map_or(0, |r| r.manual) < MANUAL_RETRY_LIMIT
            })
        })
}

pub fn available_actions(task: &Task) -> Vec<&'static str> {
    let mut actions = Vec::new();
    let merge_ready = task.is_at(Stage::Merge, StagePhase::AwaitingMerge) && gates_clear(task);
    if merge_ready {
        actions.push("merge");
    }
    if can_retry(task) {
        actions.push("retry");
    }
    if matches!(
        task.item.state,
        WorkItemState::Queued
            | WorkItemState::Admitted
            | WorkItemState::InStage
            | WorkItemState::Blocked
            | WorkItemState::TakenOver
    ) {
        actions.push("cancel");
    }
    if can_takeover(task) {
        actions.push("takeover");
    }
    if task.item.state == WorkItemState::TakenOver {
        actions.push("return");
    }
    if external::awaiting_request(task) {
        actions.push("review");
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
        BlockerCode::EnvSetupFailed => !matches!(
            params.pointer("/error/code").and_then(Value::as_str),
            Some("setup_command_failed" | "docs_commit_missing" | "docs_checkout_too_large")
        ),
        BlockerCode::MergeConflict | BlockerCode::DocsMergeFailed => {
            text("reason") == Some("head_changed")
        }
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
    let Some((stage, _)) = retry_target(task, blocker.code, &blocker.params) else {
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

pub fn docs_pending(task: &Task) -> Option<String> {
    let pushed = task
        .environment
        .as_ref()
        .and_then(|e| e.docs.as_ref())
        .and_then(|d| d.pushed_sha.as_ref());
    task.stage_run
        .as_ref()
        .and_then(|r| r.docs_head_sha.clone())
        .filter(|h| Some(h) != pushed)
}

pub fn needs_docs_change(task: &Task) -> bool {
    task.docs_change.is_none()
        && task
            .environment
            .as_ref()
            .and_then(|e| e.docs.as_ref())
            .is_some_and(|d| d.pushed_sha.is_some())
}

pub fn docs_open(task: &Task) -> bool {
    task.docs_change
        .as_ref()
        .is_some_and(|c| c.state == ChangeState::Open && c.skipped_at.is_none())
}

pub fn watches_docs_change(task: &Task) -> bool {
    docs_open(task)
        && matches!(
            task.item.state,
            WorkItemState::Blocked | WorkItemState::InStage
        )
}

pub fn watches_change(task: &Task) -> bool {
    let open = task
        .change_request
        .as_ref()
        .is_some_and(|c| c.state == ChangeState::Open);
    open && match task.item.state {
        WorkItemState::InStage if task.item.origin == WorkItemOrigin::ExternalChange => true,
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
    let owner = RunOwner::StageRun {
        work_item_id: task.item.id,
        stage_run_id: run.id,
    };
    let reviewing = run.stage == Stage::Review;
    // A reviewer never inherits another run's conversation: only its own session is visible.
    let session = task
        .session
        .as_ref()
        .filter(|s| !reviewing || s.owner == owner);
    Some(RunView {
        owner,
        project_id: task.item.project_id,
        session,
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
        packet: if reviewing {
            Prompt::ReviewPacket
        } else {
            Prompt::WorkPacket {
                retry_of: blocker
                    .filter(|_| run.trigger == Trigger::Manual)
                    .map(coded),
            }
        },
    })
}

pub fn merge_status(reason: &str) -> u16 {
    match reason {
        "head_changed" => 409,
        "rejected" => 422,
        _ => 405,
    }
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
            (BlockerCode::PathScopeViolation, json!({ "stage": "dev" })),
            (BlockerCode::PathScopeViolation, json!({ "stage": "merge" })),
            (BlockerCode::BudgetExhausted, json!({})),
            (
                BlockerCode::MergeConflict,
                json!({ "reason": "not_mergeable" }),
            ),
            (
                BlockerCode::EnvSetupFailed,
                json!({ "error": { "code": "setup_command_failed" } }),
            ),
            (
                BlockerCode::EnvSetupFailed,
                json!({ "error": { "code": "docs_commit_missing" } }),
            ),
            (
                BlockerCode::EnvSetupFailed,
                json!({ "error": { "code": "docs_checkout_too_large" } }),
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
    fn credential_blockers_resume_where_they_stopped_and_never_count_as_auto_retries() {
        let link =
            |phase: &str| json!({ "integration_id": "github", "cause": "revoked", "phase": phase });
        assert_eq!(
            retry_entry(BlockerCode::UserLinkRequired, &link("running_session")),
            Some((Stage::Dev, StagePhase::RunningSession))
        );
        assert_eq!(
            retry_entry(BlockerCode::UserLinkRequired, &link("opening_change")),
            Some((Stage::Dev, StagePhase::OpeningChange))
        );
        assert_eq!(
            retry_entry(BlockerCode::UserAccessDenied, &json!({ "status": 403 })),
            Some((Stage::Dev, StagePhase::OpeningChange))
        );
        assert!(!auto_retryable(
            BlockerCode::UserLinkRequired,
            &link("opening_change")
        ));
        assert!(!auto_retryable(BlockerCode::UserAccessDenied, &json!({})));
        for phase in [StagePhase::RunningSession, StagePhase::OpeningChange] {
            let token = json!({ "cause": "rejected", "phase": phase });
            assert_eq!(
                retry_entry(BlockerCode::UserTokenInvalid, &token),
                Some((Stage::Dev, phase))
            );
            assert!(!auto_retryable(BlockerCode::UserTokenInvalid, &token));
        }
        let mut r = BTreeMap::new();
        count_retry(
            &mut r,
            Stage::Dev,
            Trigger::Auto,
            Some("auto retry: session_lost"),
        );
        for trigger in ResumeTrigger::ALL {
            count_retry(&mut r, Stage::Dev, Trigger::Auto, Some(trigger.as_str()));
        }
        assert_eq!(r[&Stage::Dev], Retries { auto: 1, manual: 0 });
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
