#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail loudly on a broken setup"
)]

use std::collections::BTreeMap;

use saka_core::Ctx;
use saka_core::autonomy::Ceilings;
use saka_core::blocker::primary_action;
use saka_core::event::Event;
use saka_core::external::{self, Incoming};
use saka_core::gate::Check;
use saka_core::ids::{IdSource, ProjectId, RepoId, ReviewId, SessionId, StageRunId};
use saka_core::job::JobKind;
use saka_core::model::Task;
use saka_core::react::react;
use saka_core::rejection::Rejection;
use saka_core::task::{
    self, BranchUpdate, Integration, LoopConfig, TaskInput, auto_merge, evolve, merge_ready,
    update_merge,
};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    Autonomy, BlockerAction, BlockerCode, ChangeRole, ChangeState, CheckConclusion, CheckKind,
    CheckStatus, ExternalCause, MergeMethod, ReviewBasis, ReviewEvent, ReviewOutcome, Stage,
    StagePhase, StageRunState, Trigger, WorkItemState, WorkflowPreset,
};
use saka_core::workflow::Workflow;
use uuid::Uuid;

struct Counter(u128);

impl IdSource for Counter {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

const NOW: Timestamp = Timestamp(1_000);
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MOVED: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BASE: &str = "cccccccccccccccccccccccccccccccccccccccc";

fn cfg() -> LoopConfig {
    LoopConfig {
        max_followup_turns: 1,
        permission_wait_ms: 24 * 3_600_000,
        decision_wait_ms: 72 * 3_600_000,
        idle_ms: 30 * 60_000,
        budgets: BTreeMap::new(),
        budget_warn_percent: 80,
        rate_limit_pause: 0.9,
        account: "claude:default".into(),
        autonomy: Ceilings::default(),
        ci_appear_timeout_ms: 600_000,
        max_ci_reruns: 3,
        poll_interval_ms: 15_000,
        merge_method: MergeMethod::Squash,
        delete_branch_after_merge: true,
        commit_status: true,
    }
}

fn incoming(from_fork: bool) -> Incoming {
    Incoming {
        project_id: ProjectId(Uuid::from_u128(1)),
        repo_id: RepoId(Uuid::from_u128(2)),
        repo_name: "shop".into(),
        number: 7,
        title: "Add retries".into(),
        url: "https://scm.test/acme/shop/pull/7".into(),
        head_branch: "feature/retries".into(),
        base_branch: "main".into(),
        head_sha: HEAD.into(),
        author: "alice".into(),
        from_fork,
        draft: false,
        spec: None,
    }
}

struct Case {
    ids: Counter,
    task: Task,
}

fn apply(case: &mut Case, events: &[Event]) {
    for e in events {
        case.task = evolve(Some(case.task.clone()), e, NOW).unwrap();
    }
}

fn decide(case: &mut Case, input: TaskInput) -> Result<Vec<Event>, Rejection> {
    let events = task::decide(
        &case.task,
        input,
        &cfg(),
        &mut Ctx {
            now: NOW,
            ids: &mut case.ids,
        },
    )?;
    apply(case, &events);
    Ok(events)
}

fn place(case: &Case) -> (WorkItemState, Option<Stage>, Option<StagePhase>) {
    let item = &case.task.item;
    (item.state, item.stage, item.stage_phase)
}

fn reviewing(workflow: &Workflow, from_fork: bool) -> Case {
    let mut ids = Counter(100);
    let (cause, actor) = if workflow.stage_trigger(Stage::Review) == Some(Trigger::Manual) {
        (ExternalCause::Comment, Some("bob".to_string()))
    } else {
        (ExternalCause::Auto, None)
    };
    let events = external::accept(
        workflow,
        incoming(from_fork),
        cause,
        actor,
        &mut Ctx {
            now: NOW,
            ids: &mut ids,
        },
    )
    .unwrap();
    let mut task = None;
    for e in &events {
        task = evolve(task, e, NOW);
    }
    let mut task = task.unwrap();
    task.external.as_mut().unwrap().workflow = workflow.clone();
    task.workflow = workflow.clone();
    let mut case = Case { ids, task };
    let run = case.task.stage_run.as_ref().unwrap().id;
    let events = [Event::StagePhaseChanged {
        work_item_id: case.task.item.id,
        stage_run_id: run,
        phase: StagePhase::PostingReview,
        head_sha: None,
        docs_head_sha: None,
    }];
    apply(&mut case, &events);
    case
}

fn record(case: &mut Case, outcome: ReviewOutcome, head: &str) -> ReviewId {
    let review_id = ReviewId(Uuid::from_u128(900));
    let run: StageRunId = case.task.stage_run.as_ref().unwrap().id;
    apply(
        case,
        &[Event::ReviewRecorded {
            work_item_id: case.task.item.id,
            stage_run_id: run,
            review_id,
            session_id: SessionId(Uuid::from_u128(901)),
            head_sha: head.into(),
            outcome,
            basis: ReviewBasis::None,
            summary: String::new(),
            findings: Vec::new(),
            questions: Vec::new(),
        }],
    );
    review_id
}

fn post(case: &mut Case, review_id: ReviewId) -> Vec<Event> {
    decide(
        case,
        TaskInput::ReviewPosted {
            review_id,
            event: ReviewEvent::Comment,
            url: None,
        },
    )
    .unwrap()
}

fn passing() -> Vec<Check> {
    vec![Check {
        name: "ci".into(),
        kind: CheckKind::CheckRun,
        status: CheckStatus::Completed,
        conclusion: Some(CheckConclusion::Success),
        url: None,
    }]
}

fn observed(head: &str, checks: Vec<Check>) -> TaskInput {
    TaskInput::ChangeObserved {
        role: ChangeRole::Code,
        state: ChangeState::Open,
        head_sha: head.into(),
        merge_commit_sha: None,
        merged_at: None,
        checks,
    }
}

fn approved_and_green(workflow: &Workflow, from_fork: bool) -> Case {
    let mut case = reviewing(workflow, from_fork);
    let review = record(&mut case, ReviewOutcome::Approved, HEAD);
    post(&mut case, review);
    decide(&mut case, observed(HEAD, passing())).unwrap();
    case
}

fn requested(case: &mut Case) {
    let version = case.task.item.version;
    decide(
        case,
        TaskInput::RequestMerge {
            expected_version: version,
            reason: Some("comment by bob".into()),
        },
    )
    .unwrap();
    decide(case, TaskInput::MergeTurn).unwrap();
}

fn behind(case: &mut Case, updatable: bool) {
    decide(
        case,
        TaskInput::Integrated(Integration::Behind {
            head_sha: HEAD.into(),
            base_sha: BASE.into(),
            updatable,
        }),
    )
    .unwrap();
}

fn review_only() -> Workflow {
    Workflow::of(WorkflowPreset::ReviewOnly)
}

#[test]
fn an_approved_outside_change_enters_the_merge_stage_and_waits_for_the_gate_at_that_head() {
    let mut case = reviewing(&review_only(), false);
    let review = record(&mut case, ReviewOutcome::Approved, HEAD);
    post(&mut case, review);
    assert_eq!(
        place(&case),
        (
            WorkItemState::InStage,
            Some(Stage::Merge),
            Some(StagePhase::AwaitingCi)
        )
    );
    assert!(!merge_ready(&case.task));

    decide(&mut case, observed(HEAD, passing())).unwrap();
    assert_eq!(
        place(&case),
        (
            WorkItemState::InStage,
            Some(Stage::Merge),
            Some(StagePhase::AwaitingMerge)
        )
    );
    assert!(merge_ready(&case.task));
}

#[test]
fn a_rejected_outside_review_and_a_workflow_without_merge_stay_reviewed() {
    let mut case = reviewing(&review_only(), false);
    let review = record(&mut case, ReviewOutcome::ChangesRequested, HEAD);
    post(&mut case, review);
    assert_eq!(
        place(&case),
        (
            WorkItemState::InStage,
            Some(Stage::Review),
            Some(StagePhase::Reviewed)
        )
    );

    let mut without = review_only();
    without.remove_stage(Stage::Merge);
    let mut case = reviewing(&without, false);
    let review = record(&mut case, ReviewOutcome::Approved, HEAD);
    post(&mut case, review);
    assert_eq!(
        place(&case),
        (
            WorkItemState::InStage,
            Some(Stage::Review),
            Some(StagePhase::Reviewed)
        )
    );
}

#[test]
fn a_merge_request_needs_the_approval_at_the_exact_head_and_is_never_automatic() {
    let mut case = approved_and_green(&review_only(), false);
    let version = case.task.item.version;
    let events = decide(
        &mut case,
        TaskInput::RequestMerge {
            expected_version: version,
            reason: Some("comment by bob".into()),
        },
    )
    .unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::MergeRequested { head_sha, .. } if head_sha == HEAD))
    );

    let mut moved = approved_and_green(&review_only(), false);
    let events = [Event::ChangeRequestUpdated {
        work_item_id: moved.task.item.id,
        change_request_id: moved.task.change_request.as_ref().unwrap().id,
        role: ChangeRole::Code,
        head_sha: MOVED.into(),
        replaced_head_sha: None,
    }];
    apply(&mut moved, &events);
    let run = moved.task.stage_run.as_ref().unwrap().id;
    let events = [Event::GateObserved {
        work_item_id: moved.task.item.id,
        stage_run_id: run,
        gate_result_id: saka_core::ids::GateResultId(Uuid::from_u128(902)),
        source: saka_core::vocab::GateSource::Scm,
        head_sha: MOVED.into(),
        outcome: saka_core::vocab::GateOutcome::Passed,
        checks: passing(),
        passed_after_rerun: None,
    }];
    apply(&mut moved, &events);
    let version = moved.task.item.version;
    let err = decide(
        &mut moved,
        TaskInput::RequestMerge {
            expected_version: version,
            reason: None,
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        Rejection::MergeNotReady {
            reason: "review",
            ..
        }
    ));

    let mut workflow = review_only();
    workflow.set_stage(Stage::Merge, Trigger::Auto);
    let ready = approved_and_green(&workflow, false);
    assert!(merge_ready(&ready.task));
    let autonomous = Ceilings {
        default: Autonomy::Autonomous,
        repos: Vec::new(),
    };
    assert!(!auto_merge(&ready.task, &autonomous));
}

#[test]
fn a_head_the_gate_never_saw_cannot_merge_and_a_head_without_checks_passes_after_the_window() {
    let mut case = reviewing(&review_only(), false);
    let review = record(&mut case, ReviewOutcome::Approved, HEAD);
    post(&mut case, review);
    decide(&mut case, observed(HEAD, Vec::new())).unwrap();
    assert!(!merge_ready(&case.task));
    assert_eq!(place(&case).2, Some(StagePhase::AwaitingCi));

    let later = Timestamp(NOW.0 + 600_001);
    let events = task::decide(
        &case.task,
        TaskInput::Tick,
        &cfg(),
        &mut Ctx {
            now: later,
            ids: &mut case.ids,
        },
    )
    .unwrap();
    for e in &events {
        case.task = evolve(Some(case.task.clone()), e, later).unwrap();
    }
    assert_eq!(place(&case).2, Some(StagePhase::AwaitingMerge));
    assert!(merge_ready(&case.task));
}

#[test]
fn an_outside_change_behind_its_base_blocks_with_an_update_action_and_never_rebases() {
    let mut case = approved_and_green(&review_only(), false);
    requested(&mut case);
    assert_eq!(place(&case).2, Some(StagePhase::Integrating));
    behind(&mut case, true);

    assert_eq!(case.task.item.state, WorkItemState::Blocked);
    let blocker = case.task.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::ChangeBehindBase);
    assert_eq!(blocker.params["base_branch"], "main");
    assert_eq!(blocker.params["base_sha"], BASE);
    assert_eq!(blocker.params["head_sha"], HEAD);
    assert_eq!(blocker.params["updatable"], true);
    assert!(!case.task.is_queued());
    assert_eq!(
        primary_action(&case.task).unwrap().kind,
        BlockerAction::UpdateBranch
    );
}

#[test]
fn a_fork_or_an_unsupported_provider_is_not_updatable_and_asks_the_author_with_one_comment() {
    for (from_fork, updatable) in [(true, true), (false, false)] {
        let mut case = approved_and_green(&review_only(), from_fork);
        requested(&mut case);
        let events = decide(
            &mut case,
            TaskInput::Integrated(Integration::Behind {
                head_sha: HEAD.into(),
                base_sha: BASE.into(),
                updatable,
            }),
        )
        .unwrap();
        let blocker = case.task.open_blocker().unwrap();
        assert_eq!(blocker.params["updatable"], false);
        assert_eq!(
            primary_action(&case.task).unwrap().kind,
            BlockerAction::Retry
        );
        let jobs: Vec<_> = events
            .iter()
            .flat_map(|e| react(&case.task, e, &cfg(), NOW))
            .filter(|j| j.kind == JobKind::ScmPostComment)
            .collect();
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            jobs[0].idempotency_key,
            format!(
                "scm.comment:{}:{HEAD}:change_behind_base",
                case.task.item.id
            )
        );
    }
}

#[test]
fn the_update_action_requests_one_update_at_the_blocked_head_and_returns_to_the_checks() {
    let mut case = approved_and_green(&review_only(), false);
    requested(&mut case);
    behind(&mut case, true);
    let blocker = case.task.open_blocker().unwrap().id;
    let events = decide(
        &mut case,
        TaskInput::ResolveBlocker {
            blocker_id: blocker,
            expected: Some(BlockerAction::UpdateBranch),
            reason: "operator".into(),
        },
    )
    .unwrap();
    assert!(case.task.open_blocker().is_none());
    assert_eq!(
        place(&case),
        (
            WorkItemState::InStage,
            Some(Stage::Merge),
            Some(StagePhase::AwaitingCi)
        )
    );
    let jobs: Vec<_> = events
        .iter()
        .flat_map(|e| react(&case.task, e, &cfg(), NOW))
        .filter(|j| j.kind == JobKind::ScmUpdateBranch)
        .collect();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].payload["expected_head_sha"], HEAD);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ChangeRequestUpdateRequested { expected_head_sha, .. } if expected_head_sha == HEAD
    )));
}

fn updating() -> Case {
    let mut case = approved_and_green(&review_only(), false);
    requested(&mut case);
    behind(&mut case, true);
    let blocker = case.task.open_blocker().unwrap().id;
    decide(
        &mut case,
        TaskInput::ResolveBlocker {
            blocker_id: blocker,
            expected: None,
            reason: "operator".into(),
        },
    )
    .unwrap();
    case
}

fn settle(case: &mut Case, outcome: BranchUpdate) -> Vec<Event> {
    decide(
        case,
        TaskInput::BranchUpdated {
            expected_head_sha: HEAD.into(),
            outcome,
        },
    )
    .unwrap()
}

#[test]
fn a_proven_update_asks_for_a_new_review_and_carries_the_merge_request_to_that_head() {
    let mut case = updating();
    settle(
        &mut case,
        BranchUpdate::Updated {
            head_sha: MOVED.into(),
        },
    );
    let cr = case.task.change_request.as_ref().unwrap();
    assert_eq!(cr.head_sha, MOVED);
    assert_eq!(cr.update_head_sha.as_deref(), Some(MOVED));
    assert_eq!(
        place(&case),
        (
            WorkItemState::InStage,
            Some(Stage::Review),
            Some(StagePhase::AwaitingReview)
        )
    );
    assert_eq!(
        case.task.stage_run.as_ref().unwrap().head_sha.as_deref(),
        Some(MOVED)
    );

    let run = case.task.stage_run.as_ref().unwrap().id;
    let events = [Event::StagePhaseChanged {
        work_item_id: case.task.item.id,
        stage_run_id: run,
        phase: StagePhase::PostingReview,
        head_sha: None,
        docs_head_sha: None,
    }];
    apply(&mut case, &events);
    let review = record(&mut case, ReviewOutcome::Approved, MOVED);
    post(&mut case, review);
    decide(&mut case, observed(MOVED, passing())).unwrap();
    assert!(update_merge(&case.task));
    assert!(!update_merge(
        &approved_and_green(&review_only(), false).task
    ));
    let version = case.task.item.version;
    decide(
        &mut case,
        TaskInput::RequestMerge {
            expected_version: version,
            reason: Some(task::UPDATE_REASON.into()),
        },
    )
    .unwrap();
    assert!(
        case.task
            .change_request
            .as_ref()
            .unwrap()
            .update_head_sha
            .is_none()
    );
    assert!(!update_merge(&case.task));
}

#[test]
fn an_update_that_made_another_head_revokes_the_request_and_stays_at_the_checks() {
    let mut case = updating();
    let events = settle(
        &mut case,
        BranchUpdate::Unexpected {
            head_sha: MOVED.into(),
        },
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::MergeDequeued { reason, .. } if reason == "head_moved"))
    );
    assert_eq!(case.task.change_request.as_ref().unwrap().head_sha, MOVED);
    assert!(
        case.task
            .change_request
            .as_ref()
            .unwrap()
            .update_head_sha
            .is_none()
    );
    assert_eq!(
        place(&case),
        (
            WorkItemState::InStage,
            Some(Stage::Merge),
            Some(StagePhase::AwaitingCi)
        )
    );
}

#[test]
fn a_conflicting_update_blocks_with_the_conflict_and_asks_the_author_once() {
    let mut case = updating();
    let events = settle(
        &mut case,
        BranchUpdate::Conflict {
            head_sha: HEAD.into(),
            base_sha: BASE.into(),
        },
    );
    let blocker = case.task.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::MergeConflict);
    assert_eq!(blocker.params["reason"], "update_conflict");
    assert_eq!(blocker.params["base_sha"], BASE);
    let jobs: Vec<_> = events
        .iter()
        .flat_map(|e| react(&case.task, e, &cfg(), NOW))
        .filter(|j| j.kind == JobKind::ScmPostComment)
        .collect();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].payload["code"], "update_conflict");
}

#[test]
fn a_provider_that_saw_the_head_move_leaves_the_task_at_the_checks_without_a_blocker() {
    for outcome in [BranchUpdate::HeadChanged, BranchUpdate::Unchanged] {
        let mut case = updating();
        let events = settle(&mut case, outcome);
        assert!(events.is_empty());
        assert!(case.task.open_blocker().is_none());
        assert_eq!(place(&case).2, Some(StagePhase::AwaitingCi));
    }
}

#[test]
fn an_author_push_closes_a_blocker_that_is_not_updatable_and_restarts_the_checks() {
    let mut case = approved_and_green(&review_only(), true);
    requested(&mut case);
    behind(&mut case, true);
    assert_eq!(case.task.item.state, WorkItemState::Blocked);
    decide(&mut case, observed(MOVED, Vec::new())).unwrap();
    assert!(case.task.open_blocker().is_none());
    assert_eq!(case.task.change_request.as_ref().unwrap().head_sha, MOVED);
    assert_eq!(
        place(&case),
        (
            WorkItemState::InStage,
            Some(Stage::Merge),
            Some(StagePhase::AwaitingCi)
        )
    );
    assert_eq!(
        case.task.stage_run.as_ref().unwrap().state,
        StageRunState::Running
    );
}

#[test]
fn a_new_head_at_the_checks_asks_for_the_review_the_workflow_triggers() {
    let mut case = approved_and_green(&review_only(), false);
    decide(&mut case, observed(MOVED, passing())).unwrap();
    assert_eq!(
        place(&case),
        (
            WorkItemState::InStage,
            Some(Stage::Review),
            Some(StagePhase::AwaitingReview)
        )
    );

    let mut manual = review_only();
    manual.set_stage(Stage::Review, Trigger::Manual);
    let mut case = approved_and_green(&manual, false);
    decide(&mut case, observed(MOVED, passing())).unwrap();
    assert_eq!(
        place(&case),
        (
            WorkItemState::InStage,
            Some(Stage::Review),
            Some(StagePhase::AwaitingTrigger)
        )
    );
}

#[test]
fn a_merge_the_provider_reports_ends_the_task_and_a_closed_change_cancels_it() {
    let mut case = approved_and_green(&review_only(), false);
    decide(
        &mut case,
        TaskInput::ChangeObserved {
            role: ChangeRole::Code,
            state: ChangeState::Merged,
            head_sha: HEAD.into(),
            merge_commit_sha: Some(MOVED.into()),
            merged_at: None,
            checks: Vec::new(),
        },
    )
    .unwrap();
    assert_eq!(case.task.item.state, WorkItemState::Done);

    let mut case = approved_and_green(&review_only(), false);
    decide(
        &mut case,
        TaskInput::ChangeObserved {
            role: ChangeRole::Code,
            state: ChangeState::Closed,
            head_sha: HEAD.into(),
            merge_commit_sha: None,
            merged_at: None,
            checks: Vec::new(),
        },
    )
    .unwrap();
    assert_eq!(case.task.item.state, WorkItemState::Cancelled);
}
