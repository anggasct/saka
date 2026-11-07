mod change;
mod evolve;
mod gates;
mod merge;
mod queries;
mod recovery;
mod reviewing;

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::Ctx;
use crate::authorship::CredentialBlock;
use crate::autonomy::Ceilings;
use crate::event::{Event, Prompt};
use crate::external;
use crate::gate::Check;
use crate::ids::{BlockerId, ChangeRequestId, EnvironmentId, ReviewId, SessionId, StageRunId};
use crate::job::JobKind;
use crate::model::{Budget, Coded, EnvironmentSetup, FixRound, Review, Task, WorkItem};
use crate::rejection::Rejection;
use crate::review;
use crate::scope::ScopeViolation;
use crate::session::{self, SessionConfig, SessionInput, SessionOutcome, SessionSignal};
use crate::time::Timestamp;
use crate::vocab::{
    BlockerAction, BlockerCode, ChangeCredentialKind, ChangeRole, ChangeState, ExternalCause,
    MergeMethod, ResumeTrigger, ReviewEvent, ScopeStage, SessionState, Severity, Stage, StagePhase,
    StageRunState, Trigger, WorkItemOrigin, WorkItemState,
};

pub use evolve::evolve;
pub use queries::{
    auto_merge, auto_merge_enabled, auto_retry_due, auto_retryable, available_actions, can_retry,
    count_retry, docs_open, docs_pending, gates_clear, merge_allowed, merge_ready, merge_status,
    needs_docs_change, needs_tick, run_view, watches_change, watches_docs_change,
};
pub use recovery::{retry_entry, retry_target};

#[derive(Debug, Clone, PartialEq)]
pub struct LoopConfig {
    pub max_followup_turns: u32,
    pub ci_appear_timeout_ms: i64,
    pub poll_interval_ms: i64,
    pub merge_method: MergeMethod,
    pub delete_branch_after_merge: bool,
    pub commit_status: bool,
    pub permission_wait_ms: i64,
    pub decision_wait_ms: i64,
    pub idle_ms: i64,
    pub budgets: BTreeMap<Stage, Budget>,
    pub budget_warn_percent: u32,
    pub rate_limit_pause: f64,
    pub account: String,
    pub autonomy: Ceilings,
}

pub const AUTO_RETRY_PREFIX: &str = "auto retry: ";
pub const MANUAL_RETRY_LIMIT: u32 = 3;
pub const AUTO_RETRY_DELAYS_MS: [i64; 2] = [60_000, 300_000];
pub const VERDICT_FAILED_FINDINGS: usize = 20;
pub const MERGED_ON_HOST: &str = "change merged on the host";
pub const QUEUE_REASON: &str = "merge queue";
pub const AUTONOMY_REASON: &str = "autonomy";
const MAX_CONFLICT_PATHS: usize = 20;

impl LoopConfig {
    pub fn session(&self) -> SessionConfig {
        SessionConfig {
            permission_wait_ms: self.permission_wait_ms,
            decision_wait_ms: self.decision_wait_ms,
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
pub enum Integration {
    Rebased {
        from_sha: String,
        head_sha: String,
        base_sha: String,
    },
    Moved {
        remote_sha: String,
    },
    Conflict {
        head_sha: String,
        base_sha: String,
        paths: Vec<String>,
    },
    Unclean {
        head_sha: String,
    },
    Enqueued {
        head_sha: String,
    },
    Merged {
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
    },
    Rejected {
        reason: String,
    },
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
    FixRequested {
        cause: ExternalCause,
        actor: Option<String>,
    },
    StartReview,
    MergeNotReady,
    AwaitStart,
    ApproveStart {
        expected_version: u64,
    },
    MergeTurn,
    Integrated(Integration),
    QueueLeft,
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
            | TaskInput::FixRequested { .. }
            | TaskInput::EnvironmentPrepared { .. }
            | TaskInput::WorktreeInspected { .. }
            | TaskInput::BranchPushed {
                target: ChangeRole::Code,
                ..
            }
            | TaskInput::PushRejected { .. }
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
        I::AwaitStart => d.await_start(),
        I::MergeNotReady => d.merge_not_ready(),
        I::ApproveStart { expected_version } => return d.approve_start(expected_version),
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
            if task.is_at(Stage::Merge, StagePhase::Merging)
                || task.is_at(Stage::Merge, StagePhase::Integrating)
            {
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
            if task.is_writing_at(StagePhase::Pushing) {
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
        I::FixRequested { cause, actor } => return d.fix_requested(cause, actor.as_deref(), ctx),
        I::Takeover { reason } => return d.takeover(reason),
        I::Return { .. } => {
            return Err(Rejection::InvalidState {
                state: d.state_name(),
            });
        }
        I::StartReview => d.start_review(),
        I::MergeTurn => d.merge_turn(),
        I::Integrated(outcome) => d.integrated(outcome, ctx),
        I::QueueLeft => d.queue_left(ctx),
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
        } => d.review_posted(review_id, event, url, ctx),
    })
}

const RETURN_STAGES: [Stage; 4] = [Stage::Dev, Stage::Review, Stage::Fix, Stage::Merge];

fn verdict_params(review: &Review) -> Value {
    let blocking: Vec<&review::Finding> = review
        .findings
        .iter()
        .filter(|f| matches!(f.severity, Severity::Blocker | Severity::Major))
        .take(VERDICT_FAILED_FINDINGS)
        .collect();
    json!({
        "head_sha": review.head_sha,
        "findings": review.findings.len(),
        "summary": review.summary,
        "blocking": blocking,
    })
}

pub fn takeable(stage: Stage, phase: Option<StagePhase>) -> bool {
    stage != Stage::Sync
        && !matches!(
            phase,
            Some(StagePhase::Merging | StagePhase::Integrating | StagePhase::PostingReview)
        )
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
        Stage::Dev | Stage::Fix if environment_ready => Some(StagePhase::RunningSession),
        Stage::Dev | Stage::Fix => Some(StagePhase::PreparingEnvironment),
        Stage::Review => Some(StagePhase::AwaitingReview),
        Stage::Merge => Some(StagePhase::AwaitingCi),
        Stage::Sync => None,
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
            fix: None,
        }
    }

    fn fix_run(
        &self,
        round: FixRound,
        trigger: Trigger,
        reason: Option<String>,
        ctx: &mut Ctx<'_>,
    ) -> Event {
        let entry = if self.task.ready_environment().is_some() {
            StagePhase::RunningSession
        } else {
            StagePhase::PreparingEnvironment
        };
        let mut run = self.new_run(Stage::Fix, trigger, entry, reason, None, ctx);
        if let Event::StageRunCreated { fix, .. } = &mut run {
            *fix = Some(round);
        }
        run
    }

    fn start_fix(&self, cause: Coded, ctx: &mut Ctx<'_>) -> Option<Event> {
        let auto = self.task.fixable()
            && self.task.workflow.stage_trigger(Stage::Fix) == Some(Trigger::Auto)
            && self.task.fix_rounds_left();
        auto.then(|| {
            let round = FixRound {
                number: self.task.fix_rounds_used() + 1,
                cause,
            };
            self.fix_run(round, Trigger::Auto, None, ctx)
        })
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
                | SessionInput::AnswerPermission { .. }
                | SessionInput::AskDecision { .. } => Err(Rejection::NotFound),
                SessionInput::AnswerDecision { .. } => Err(Rejection::DecisionNotFound),
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
        self.task.is_writing_at(StagePhase::RunningSession)
            || self.task.is_at(Stage::Review, StagePhase::RunningSession)
    }

    fn settle(&self, outcome: SessionOutcome, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let mut events = outcome.events;
        match outcome.signal {
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

    fn await_start(&self) -> Vec<Event> {
        if self.item().state != WorkItemState::Queued
            || self.item().origin != WorkItemOrigin::SpecStep
            || self.item().start_approved_at.is_some()
        {
            return vec![];
        }
        vec![Event::WorkItemAwaitingStart {
            work_item_id: self.item().id,
        }]
    }

    fn approve_start(&self, expected_version: u64) -> Result<Vec<Event>, Rejection> {
        if expected_version != self.item().version {
            return Err(Rejection::VersionStale);
        }
        if self.item().state != WorkItemState::AwaitingStart {
            return Err(Rejection::InvalidState {
                state: self.state_name(),
            });
        }
        Ok(vec![Event::WorkItemStartApproved {
            work_item_id: self.item().id,
        }])
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
        if !self.task.is_writing_at(StagePhase::RunningSession)
            || session.turns != turn
            || session.state != SessionState::Idle
            || session.interrupt_requested.is_some()
        {
            return vec![];
        }
        let unchanged = self.task.is_at(Stage::Fix, StagePhase::RunningSession)
            && self
                .task
                .change_request
                .as_ref()
                .is_some_and(|cr| cr.head_sha == head_sha);
        let committed = !dirty && commits_ahead > 0 && !unchanged;
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

    fn fix_requested(
        &self,
        cause: ExternalCause,
        actor: Option<&str>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Vec<Event>, Rejection> {
        let (Some(cr), Some(_)) = (&self.task.change_request, &self.task.external) else {
            return Err(Rejection::Unsupported);
        };
        if cr.state != ChangeState::Open {
            return Err(Rejection::ChangeRequestNotOpen {
                number: cr.number,
                state: cr.state.as_str(),
            });
        }
        if let Some(reason) = external::fix_blocker(self.task) {
            return Err(Rejection::FixNotAvailable { reason });
        }
        let Some(verdict) = self.task.rejected_at(&cr.head_sha) else {
            return Err(Rejection::FixNotAvailable {
                reason: "no_findings",
            });
        };
        let round = FixRound {
            number: self.task.fix_rounds_used() + 1,
            cause: Coded {
                code: BlockerCode::VerdictFailed.as_str().to_string(),
                params: verdict_params(verdict),
            },
        };
        let trigger = if cause == ExternalCause::Auto {
            Trigger::Auto
        } else {
            Trigger::Manual
        };
        Ok(vec![self.fix_run(
            round,
            trigger,
            external::reason(cause, actor),
            ctx,
        )])
    }

    fn state_name(&self) -> String {
        self.item().state.as_str().to_string()
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use crate::model::Retries;

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
