#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use std::collections::BTreeMap;

use proptest::prelude::*;
use saka_core::Ctx;
use saka_core::blocker::primary_action;
use saka_core::event::Event;
use saka_core::ids::{
    ChangeRequestId, EnvironmentId, IdSource, ProjectId, RepoId, ReviewId, SessionId, StageRunId,
    WorkItemId,
};
use saka_core::model::{ChangeRequest, Coded, FixRound, Review, Task, WorkItem};
use saka_core::rejection::Rejection;
use saka_core::review::Finding;
use saka_core::task::{self, LoopConfig, TaskInput, retry_target};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    BlockerAction, BlockerCode, ChangeRole, ChangeState, MergeMethod, ReviewBasis, ReviewEvent,
    ReviewOutcome, Severity, Stage, StagePhase, StageRunState, Trigger, WorkItemOrigin,
    WorkItemState, WorkflowPreset,
};
use saka_core::workflow::Workflow;
use serde_json::json;
use uuid::Uuid;

struct Ids(u128);

impl IdSource for Ids {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

const NOW: Timestamp = Timestamp(1_000);
const HEAD: &str = "1111111111111111111111111111111111111111";
const OTHER: &str = "2222222222222222222222222222222222222222";

fn cfg() -> LoopConfig {
    LoopConfig {
        max_followup_turns: 1,
        ci_appear_timeout_ms: 600_000,
        max_ci_reruns: 3,
        poll_interval_ms: 15_000,
        merge_method: MergeMethod::Squash,
        delete_branch_after_merge: true,
        commit_status: true,
        permission_wait_ms: 3_600_000,
        decision_wait_ms: 72 * 3_600_000,
        idle_ms: 1_800_000,
        budgets: BTreeMap::new(),
        budget_warn_percent: 80,
        rate_limit_pause: 0.9,
        account: "claude:default".into(),
        autonomy: saka_core::autonomy::Ceilings::default(),
    }
}

fn run_created(stage: Stage, entry: StagePhase, id: u128) -> Event {
    Event::StageRunCreated {
        work_item_id: WorkItemId(Uuid::from_u128(1)),
        stage_run_id: StageRunId(Uuid::from_u128(id)),
        stage,
        attempt: 1,
        trigger: Trigger::Auto,
        entry_phase: entry,
        reason: None,
        retry_of: None,
        budget: None,
        head_sha: Some(HEAD.into()),
        fix: None,
    }
}

fn review(head: &str, outcome: ReviewOutcome) -> Review {
    Review {
        id: ReviewId(Uuid::from_u128(700)),
        work_item_id: WorkItemId(Uuid::from_u128(1)),
        stage_run_id: StageRunId(Uuid::from_u128(11)),
        session_id: SessionId(Uuid::from_u128(800)),
        head_sha: head.into(),
        outcome,
        basis: ReviewBasis::Step,
        summary: "Needs work.".into(),
        findings: vec![Finding {
            path: None,
            line: None,
            severity: Severity::Major,
            body: "Add a test.".into(),
        }],
        questions: Vec::new(),
        posted: None,
        created_at: NOW,
    }
}

fn task_at_merge(workflow: Workflow) -> Task {
    let mut task = Task::new(WorkItem {
        kind: Some(saka_core::vocab::WorkKind::Feature),
        id: WorkItemId(Uuid::from_u128(1)),
        project_id: ProjectId(Uuid::from_u128(2)),
        origin: WorkItemOrigin::SpecStep,
        spec_id: None,
        spec_revision_id: None,
        step_id: Some("S1".into()),
        step_index: Some(1),
        task_key: "tasks/s1".into(),
        work: "Add done".into(),
        repo_id: RepoId(Uuid::from_u128(3)),
        branch: "saka/tasks/s1".into(),
        state: WorkItemState::Queued,
        stage: None,
        stage_phase: None,
        priority: 0,
        version: 1,
        admitted_at: None,
        done_at: None,
        start_approved_at: None,
        created_at: NOW,
        updated_at: NOW,
    });
    task.workflow = workflow;
    task.change_request = Some(ChangeRequest {
        id: ChangeRequestId(Uuid::from_u128(4)),
        work_item_id: task.item.id,
        role: ChangeRole::Code,
        repo_id: task.item.repo_id,
        number: 12,
        url: "https://scm.test/acme/app/pull/12".into(),
        head_branch: task.item.branch.clone(),
        base_branch: "main".into(),
        head_sha: HEAD.into(),
        update_head_sha: None,
        replaced_head_sha: None,
        ci_rerun: None,
        state: ChangeState::Open,
        merge_commit_sha: None,
        merged_at: None,
        skipped_at: None,
        last_polled_at: None,
        created_at: NOW,
        updated_at: NOW,
    });
    task
}

fn evolved(task: Task, event: &Event) -> Task {
    task::evolve(Some(task), event, NOW).unwrap()
}

fn round(number: u32) -> FixRound {
    FixRound {
        number,
        cause: Coded {
            code: "verdict_failed".into(),
            params: json!({ "head_sha": HEAD, "summary": "Needs work." }),
        },
    }
}

fn fix_created(id: u128, number: u32, entry: StagePhase, trigger: Trigger) -> Event {
    let Event::StageRunCreated {
        work_item_id,
        stage_run_id,
        attempt,
        reason,
        retry_of,
        budget,
        head_sha,
        ..
    } = run_created(Stage::Fix, entry, id)
    else {
        unreachable!("run_created builds a stage run");
    };
    Event::StageRunCreated {
        work_item_id,
        stage_run_id,
        stage: Stage::Fix,
        attempt,
        trigger,
        entry_phase: entry,
        reason: reason.or_else(|| (trigger == Trigger::Manual).then(|| "retry".into())),
        retry_of,
        budget,
        head_sha,
        fix: Some(round(number)),
    }
}

fn workflow(rounds: u32, fix: Option<Trigger>) -> Workflow {
    let mut workflow = Workflow::of(WorkflowPreset::FullLoop);
    workflow.fix_rounds = rounds;
    workflow.remove_stage(Stage::Fix);
    if let Some(trigger) = fix {
        workflow.set_stage(Stage::Fix, trigger);
    }
    workflow
}

fn posting(workflow: Workflow, used: u32) -> Task {
    let mut task = task_at_merge(workflow);
    task = evolved(task, &run_created(Stage::Merge, StagePhase::AwaitingCi, 10));
    for n in 1..=used {
        task = evolved(
            task,
            &fix_created(
                20 + u128::from(n),
                n,
                StagePhase::RunningSession,
                Trigger::Auto,
            ),
        );
    }
    task = evolved(
        task,
        &run_created(Stage::Review, StagePhase::AwaitingReview, 11),
    );
    task = evolved(
        task,
        &Event::StagePhaseChanged {
            work_item_id: WorkItemId(Uuid::from_u128(1)),
            stage_run_id: StageRunId(Uuid::from_u128(11)),
            phase: StagePhase::PostingReview,
            head_sha: None,
            docs_head_sha: None,
        },
    );
    task.reviews = vec![review(HEAD, ReviewOutcome::ChangesRequested)];
    task
}

fn post(task: &Task) -> Vec<Event> {
    let mut ids = Ids(3_000);
    task::decide(
        task,
        TaskInput::ReviewPosted {
            review_id: ReviewId(Uuid::from_u128(700)),
            event: ReviewEvent::Comment,
            url: None,
        },
        &cfg(),
        &mut Ctx {
            now: NOW,
            ids: &mut ids,
        },
    )
    .unwrap()
}

fn retry(task: &Task) -> Vec<Event> {
    let mut ids = Ids(4_000);
    task::decide(
        task,
        TaskInput::Retry {
            reason: "again".into(),
        },
        &cfg(),
        &mut Ctx {
            now: NOW,
            ids: &mut ids,
        },
    )
    .unwrap()
}

fn created_fix(events: &[Event]) -> Option<(Trigger, StagePhase, FixRound)> {
    events.iter().find_map(|e| match e {
        Event::StageRunCreated {
            stage: Stage::Fix,
            trigger,
            entry_phase,
            fix: Some(fix),
            ..
        } => Some((*trigger, *entry_phase, fix.clone())),
        _ => None,
    })
}

#[test]
fn a_rejected_verdict_with_rounds_left_starts_a_fix_round_instead_of_blocking() {
    let task = posting(workflow(2, Some(Trigger::Auto)), 0);
    let events = post(&task);
    let (trigger, entry, fix) = created_fix(&events).expect("a fix round starts");
    assert_eq!(
        (trigger, entry),
        (Trigger::Auto, StagePhase::PreparingEnvironment)
    );
    assert_eq!(fix.number, 1);
    assert_eq!(fix.cause.code, "verdict_failed");
    assert_eq!(fix.cause.params["head_sha"], HEAD);
    assert_eq!(fix.cause.params["summary"], "Needs work.");
    assert!(events.iter().any(|e| matches!(
        e,
        Event::StageRunSucceeded { stage_run_id, .. } if stage_run_id.0 == Uuid::from_u128(11)
    )));
    let after = events.iter().fold(task, evolved);
    assert_eq!(after.fix_rounds_used(), 1);
    assert!(after.open_blocker().is_none());
    assert_eq!(after.stage_run.as_ref().unwrap().stage, Stage::Fix);
}

#[test]
fn the_round_after_the_last_blocks_with_the_rounds_spent_and_retry_grants_one_more() {
    let task = posting(workflow(2, Some(Trigger::Auto)), 2);
    let events = post(&task);
    assert!(created_fix(&events).is_none());
    let blocked = events.iter().fold(task, evolved);
    let blocker = blocked.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::ReviewRoundsExhausted);
    assert_eq!(blocker.params["rounds"], 2);
    assert_eq!(blocker.params["max"], 2);
    assert_eq!(blocker.params["head_sha"], HEAD);
    assert_eq!(primary_action(&blocked).unwrap().kind, BlockerAction::Retry);
    assert_eq!(
        retry_target(&blocked, blocker.code, &blocker.params),
        Some((Stage::Fix, StagePhase::RunningSession))
    );

    let events = retry(&blocked);
    let (trigger, _, fix) = created_fix(&events).unwrap();
    assert_eq!((trigger, fix.number), (Trigger::Manual, 3));
    assert_eq!(
        fix.cause.code, "verdict_failed",
        "the cause names the verdict, not the exhaustion"
    );
}

#[test]
fn a_manual_fix_waits_for_the_operator_and_a_workflow_without_fix_keeps_the_old_path() {
    let manual = posting(workflow(2, Some(Trigger::Manual)), 0);
    let blocked = post(&manual).iter().fold(manual, evolved);
    assert_eq!(
        blocked.open_blocker().unwrap().code,
        BlockerCode::VerdictFailed
    );
    let (trigger, _, fix) = created_fix(&retry(&blocked)).unwrap();
    assert_eq!((trigger, fix.number), (Trigger::Manual, 1));

    let none = posting(workflow(2, None), 0);
    let blocked = post(&none).iter().fold(none, evolved);
    let blocker = blocked.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::VerdictFailed);
    assert_eq!(
        retry_target(&blocked, blocker.code, &blocker.params),
        Some((Stage::Dev, StagePhase::RunningSession))
    );
}

#[test]
fn a_retry_inside_a_fix_run_goes_on_with_the_same_round() {
    let mut task = posting(workflow(3, Some(Trigger::Auto)), 0);
    task = post(&task).iter().fold(task, evolved);
    let run = task.stage_run.clone().unwrap();
    let blocked = evolved(
        evolved(
            task,
            &Event::StagePhaseChanged {
                work_item_id: WorkItemId(Uuid::from_u128(1)),
                stage_run_id: run.id,
                phase: StagePhase::RunningSession,
                head_sha: None,
                docs_head_sha: None,
            },
        ),
        &Event::WorkItemBlocked {
            work_item_id: WorkItemId(Uuid::from_u128(1)),
            stage_run_id: Some(run.id),
            blocker_id: saka_core::ids::BlockerId(Uuid::from_u128(900)),
            code: BlockerCode::NoChanges,
            params: json!({ "dirty": false, "commits": 0 }),
            stage: Some(Stage::Fix),
            phase: Some(StagePhase::RunningSession),
        },
    );
    let events = retry(&blocked);
    let (_, entry, fix) = created_fix(&events).unwrap();
    assert_eq!(fix.number, 1, "the retry is not another round");
    assert_eq!(fix.cause.code, "verdict_failed");
    assert_eq!(entry, StagePhase::PreparingEnvironment);
    let after = events.iter().fold(blocked, evolved);
    assert_eq!(after.fix_rounds_used(), 1);
}

#[test]
fn a_conflict_blocker_retries_into_a_fix_round_only_when_the_workflow_has_a_fix() {
    let params = json!({ "reason": "rebase_conflict", "head_sha": HEAD, "paths": ["a.rs"] });
    let with = task_at_merge(workflow(2, Some(Trigger::Auto)));
    assert_eq!(
        retry_target(&with, BlockerCode::MergeConflict, &params),
        Some((Stage::Fix, StagePhase::RunningSession))
    );
    let without = task_at_merge(workflow(2, None));
    assert_eq!(
        retry_target(&without, BlockerCode::MergeConflict, &params),
        Some((Stage::Dev, StagePhase::RunningSession))
    );
    assert_eq!(
        retry_target(
            &with,
            BlockerCode::MergeConflict,
            &json!({ "reason": "head_changed" })
        ),
        Some((Stage::Merge, StagePhase::AwaitingCi))
    );
}

proptest! {
    #[test]
    fn automatic_rounds_never_pass_the_limit(limit in 1u32..=10, verdicts in 1usize..=20) {
        let mut task = posting(workflow(limit, Some(Trigger::Auto)), 0);
        let mut started = 0u32;
        for _ in 0..verdicts {
            let events = post(&task);
            started += u32::from(created_fix(&events).is_some());
            task = events.iter().fold(task, evolved);
            if task.open_blocker().is_some() {
                break;
            }
            let fixed = task.stage_run.clone().unwrap();
            task = evolved(
                task,
                &Event::StageRunSucceeded {
                    work_item_id: WorkItemId(Uuid::from_u128(1)),
                    stage_run_id: fixed.id,
                    head_sha: None,
                },
            );
            let active = task
                .retired_runs
                .iter()
                .chain(task.stage_run.iter())
                .filter(|r| r.stage == Stage::Fix
                    && matches!(r.state, StageRunState::Pending | StageRunState::Running))
                .count();
            prop_assert!(active <= 1);
            task = evolved(task, &run_created(Stage::Review, StagePhase::AwaitingReview, 11 + u128::from(started) * 100));
            task = evolved(task, &Event::StagePhaseChanged {
                work_item_id: WorkItemId(Uuid::from_u128(1)),
                stage_run_id: StageRunId(Uuid::from_u128(11 + u128::from(started) * 100)),
                phase: StagePhase::PostingReview,
                head_sha: None,
                docs_head_sha: None,
            });
            task.reviews = vec![review(HEAD, ReviewOutcome::ChangesRequested)];
            task.reviews[0].stage_run_id = StageRunId(Uuid::from_u128(11 + u128::from(started) * 100));
        }
        prop_assert!(started <= limit);
        prop_assert!(task.fix_rounds_used() <= limit);
    }
}

fn taken_over(mut task: Task, environment: bool) -> Task {
    if environment {
        let owner = task.run_owner().unwrap();
        let (repo_id, branch) = (task.item.repo_id, task.item.branch.clone());
        task = evolved(
            task,
            &Event::EnvironmentPrepared {
                owner,
                environment_id: EnvironmentId(Uuid::from_u128(50)),
                repo_id,
                root_path: "/envs/1".into(),
                worktree_path: "/envs/1/work".into(),
                branch: Some(branch),
                base_sha: HEAD.into(),
                port_start: None,
                port_count: None,
                recipe: None,
                recipe_sha256: None,
                setup: Vec::new(),
                toolchains: Vec::new(),
                path_scope: Vec::new(),
                docs: None,
            },
        );
    }
    evolved(
        task,
        &Event::WorkItemTakenOver {
            work_item_id: WorkItemId(Uuid::from_u128(1)),
            reason: "by hand".into(),
            stage: Some(Stage::Review),
            phase: Some(StagePhase::PostingReview),
        },
    )
}

fn give_back(task: &Task, stage: Stage) -> Result<Vec<Event>, Rejection> {
    let mut ids = Ids(5_000);
    task::decide(
        task,
        TaskInput::Return {
            stage: Some(stage),
            reason: "done by hand".into(),
            change: None,
        },
        &cfg(),
        &mut Ctx {
            now: NOW,
            ids: &mut ids,
        },
    )
}

fn refused(result: Result<Vec<Event>, Rejection>) -> (&'static str, &'static str) {
    match result {
        Err(Rejection::ReturnTargetInvalid { stage, reason }) => (stage, reason),
        other => panic!("expected a refused return, got {other:?}"),
    }
}

#[test]
fn a_task_returns_to_fix_with_the_last_verdict_and_the_round_it_had() {
    let task = taken_over(posting(workflow(2, Some(Trigger::Auto)), 1), true);
    assert!(task::takeable(Stage::Fix, Some(StagePhase::RunningSession)));
    let events = give_back(&task, Stage::Fix).unwrap();
    let (trigger, entry, fix) = created_fix(&events).unwrap();
    assert_eq!(
        (trigger, entry),
        (Trigger::Auto, StagePhase::RunningSession)
    );
    assert_eq!(fix.number, 1, "coming back is not another round");
    assert_eq!(fix.cause.code, "verdict_failed");
    assert_eq!(fix.cause.params["head_sha"], HEAD);
    let after = events.iter().fold(task, evolved);
    assert!(after.is_at(Stage::Fix, StagePhase::RunningSession));
    assert_eq!(after.fix_rounds_used(), 1);

    let fresh = taken_over(posting(workflow(2, Some(Trigger::Auto)), 0), true);
    let (_, _, fix) = created_fix(&give_back(&fresh, Stage::Fix).unwrap()).unwrap();
    assert_eq!(
        fix.number, 1,
        "a verdict nobody fixed yet is the first round"
    );
}

#[test]
fn a_return_to_fix_needs_a_fix_stage_and_a_verdict_at_the_head() {
    let mut task = taken_over(posting(workflow(2, Some(Trigger::Auto)), 0), true);
    task.reviews[0].head_sha = OTHER.into();
    assert_eq!(
        refused(give_back(&task, Stage::Fix)),
        ("fix", "no_findings")
    );
    task.reviews.clear();
    assert_eq!(
        refused(give_back(&task, Stage::Fix)),
        ("fix", "no_findings")
    );

    let without = taken_over(posting(workflow(2, None), 0), true);
    assert_eq!(
        refused(give_back(&without, Stage::Fix)),
        ("fix", "stage_not_in_workflow")
    );
}

#[test]
fn a_task_returns_to_review_at_the_head_it_has_now() {
    let task = taken_over(posting(workflow(2, Some(Trigger::Auto)), 0), true);
    let events = give_back(&task, Stage::Review).unwrap();
    let after = events.iter().fold(task.clone(), evolved);
    let run = after.stage_run.as_ref().unwrap();
    assert_eq!(run.stage, Stage::Review);
    assert_eq!(run.head_sha.as_deref(), Some(HEAD));
    assert!(after.is_at(Stage::Review, StagePhase::AwaitingReview));

    let mut build_only = workflow(2, None);
    build_only.remove_stage(Stage::Review);
    let no_review = taken_over(posting(build_only, 0), true);
    assert_eq!(
        refused(give_back(&no_review, Stage::Review)),
        ("review", "stage_not_in_workflow")
    );
    let no_environment = taken_over(posting(workflow(2, Some(Trigger::Auto)), 0), false);
    assert_eq!(
        refused(give_back(&no_environment, Stage::Review)),
        ("review", "environment_missing")
    );
}
