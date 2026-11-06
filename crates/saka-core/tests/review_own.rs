#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use std::collections::BTreeMap;

use saka_core::Ctx;
use saka_core::event::Event;
use saka_core::ids::{
    ChangeRequestId, GateResultId, IdSource, ProjectId, RepoId, ReviewId, SessionId, StageRunId,
    WorkItemId,
};
use saka_core::model::{ChangeRequest, GateResult, Review, Task, WorkItem};
use saka_core::rejection::Rejection;
use saka_core::review::Finding;
use saka_core::task::{self, LoopConfig, TaskInput, retry_target};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    BlockerCode, ChangeRole, ChangeState, GateOutcome, GateSource, MergeMethod, ReviewBasis,
    ReviewOutcome, Severity, Stage, StagePhase, Trigger, WorkItemOrigin, WorkItemState,
    WorkflowPreset,
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

fn gate(source: GateSource, head: &str, run: u128) -> GateResult {
    GateResult {
        id: GateResultId(Uuid::from_u128(500 + run)),
        stage_run_id: StageRunId(Uuid::from_u128(run)),
        work_item_id: WorkItemId(Uuid::from_u128(1)),
        source,
        head_sha: head.into(),
        outcome: GateOutcome::Passed,
        checks: Vec::new(),
        observed_at: NOW,
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

fn request_merge(task: &Task) -> Result<Vec<Event>, Rejection> {
    let mut ids = Ids(1_000);
    task::decide(
        task,
        TaskInput::RequestMerge {
            expected_version: task.item.version,
            reason: None,
        },
        &cfg(),
        &mut Ctx {
            now: NOW,
            ids: &mut ids,
        },
    )
}

fn refusal(task: &Task) -> (&'static str, Option<String>, Option<&'static str>) {
    match request_merge(task) {
        Err(Rejection::MergeNotReady {
            reason,
            head_sha,
            review_head_sha,
            review_outcome,
            ..
        }) => {
            assert_eq!(head_sha.as_deref(), Some(HEAD));
            (reason, review_head_sha, review_outcome)
        }
        other => panic!("expected a refused merge, got {other:?}"),
    }
}

#[test]
fn a_task_with_a_review_stage_merges_only_at_a_head_with_an_approving_verdict() {
    let mut task = task_at_merge(Workflow::of(WorkflowPreset::FullLoop));
    task = evolved(
        task,
        &run_created(Stage::Merge, StagePhase::AwaitingMerge, 10),
    );
    task.gates = vec![gate(GateSource::Scm, HEAD, 10)];
    assert!(task.has_review_stage());
    assert!(!task::merge_ready(&task));
    assert!(!task::available_actions(&task).contains(&"merge"));
    assert_eq!(refusal(&task), ("review", None, None));

    task.reviews = vec![review(OTHER, ReviewOutcome::Approved)];
    assert!(
        !task::merge_ready(&task),
        "a verdict at another head is not enough"
    );
    assert_eq!(
        refusal(&task),
        ("review", Some(OTHER.to_string()), Some("approved")),
        "the refusal names the head the verdict judged"
    );
    task.reviews = vec![review(HEAD, ReviewOutcome::ChangesRequested)];
    assert!(
        !task::merge_ready(&task),
        "only an approval opens the merge"
    );
    assert_eq!(
        refusal(&task),
        ("review", Some(HEAD.to_string()), Some("changes_requested"))
    );

    task.reviews = vec![review(HEAD, ReviewOutcome::Approved)];
    assert!(task::merge_ready(&task));
    assert!(task::available_actions(&task).contains(&"merge"));
    assert!(request_merge(&task).is_ok());
}

#[test]
fn a_workflow_without_a_review_stage_merges_on_the_gates_alone() {
    let mut task = task_at_merge(saka_workflow_without_review());
    task = evolved(
        task,
        &run_created(Stage::Merge, StagePhase::AwaitingMerge, 10),
    );
    task.gates = vec![gate(GateSource::Scm, HEAD, 10)];
    assert!(!task.has_review_stage());
    assert!(task::merge_ready(&task));
    assert!(request_merge(&task).is_ok());
}

fn saka_workflow_without_review() -> Workflow {
    let mut workflow = Workflow::of(WorkflowPreset::Custom);
    workflow.spec = true;
    workflow.set_stage(Stage::Dev, Trigger::Auto);
    workflow.set_stage(Stage::Merge, Trigger::Manual);
    workflow
}

#[test]
fn the_gates_of_the_judged_head_survive_the_review_and_the_merge_after_it_but_nothing_else() {
    let mut task = task_at_merge(Workflow::of(WorkflowPreset::FullLoop));
    task = evolved(task, &run_created(Stage::Merge, StagePhase::AwaitingCi, 10));
    task.gates = vec![
        gate(GateSource::Scm, HEAD, 10),
        gate(GateSource::Saka, OTHER, 9),
    ];

    let review_run = run_created(Stage::Review, StagePhase::AwaitingReview, 11);
    let reviewing = evolved(task.clone(), &review_run);
    assert_eq!(
        reviewing
            .gates
            .iter()
            .map(|g| g.head_sha.as_str())
            .collect::<Vec<_>>(),
        [HEAD],
        "the review keeps the gates of the head it judges"
    );

    let merging = evolved(
        reviewing.clone(),
        &run_created(Stage::Merge, StagePhase::AwaitingMerge, 12),
    );
    assert_eq!(merging.gates.len(), 1);
    assert!(merging.is_at(Stage::Merge, StagePhase::AwaitingMerge));

    for (stage, entry) in [
        (Stage::Dev, StagePhase::RunningSession),
        (Stage::Merge, StagePhase::AwaitingCi),
    ] {
        let fresh = evolved(reviewing.clone(), &run_created(stage, entry, 13));
        assert!(fresh.gates.is_empty(), "{stage:?} starts from nothing");
    }
}

#[test]
fn a_rejected_verdict_goes_back_to_the_dev_session_when_the_workflow_has_no_fix() {
    let params = json!({ "head_sha": HEAD, "findings": 1, "summary": "Needs work." });
    let mut workflow = Workflow::of(WorkflowPreset::FullLoop);
    workflow.remove_stage(Stage::Fix);
    let mut task = task_at_merge(workflow);
    assert_eq!(
        retry_target(&task, BlockerCode::VerdictFailed, &params),
        Some((Stage::Dev, StagePhase::RunningSession))
    );
    task = evolved(
        task,
        &run_created(Stage::Review, StagePhase::AwaitingReview, 11),
    );
    assert_eq!(
        retry_target(&task, BlockerCode::VerdictFailed, &params),
        Some((Stage::Dev, StagePhase::RunningSession)),
        "the blocked run is the review itself"
    );
    assert_eq!(
        retry_target(&task, BlockerCode::ReviewRoundsExhausted, &params),
        None,
        "without a fix stage there are no rounds to exhaust"
    );
    assert_eq!(
        retry_target(&task, BlockerCode::SpecUnclear, &json!({})),
        Some((Stage::Review, StagePhase::AwaitingReview))
    );
}

fn reviewing(phase: StagePhase) -> Task {
    let mut task = task_at_merge(Workflow::of(WorkflowPreset::FullLoop));
    task = evolved(task, &run_created(Stage::Merge, StagePhase::AwaitingCi, 10));
    task = evolved(
        task,
        &run_created(Stage::Review, StagePhase::AwaitingReview, 11),
    );
    if phase != StagePhase::AwaitingReview {
        task = evolved(
            task,
            &Event::StagePhaseChanged {
                work_item_id: WorkItemId(Uuid::from_u128(1)),
                stage_run_id: StageRunId(Uuid::from_u128(11)),
                phase,
                head_sha: None,
                docs_head_sha: None,
            },
        );
    }
    task
}

fn observe(task: &Task, state: ChangeState, head: &str) -> Vec<Event> {
    let mut ids = Ids(2_000);
    task::decide(
        task,
        TaskInput::ChangeObserved {
            role: ChangeRole::Code,
            state,
            head_sha: head.into(),
            merge_commit_sha: (state == ChangeState::Merged).then(|| "3".repeat(40)),
            merged_at: None,
            checks: Vec::new(),
        },
        &cfg(),
        &mut Ctx {
            now: NOW,
            ids: &mut ids,
        },
    )
    .unwrap()
}

#[test]
fn a_head_that_moves_during_the_review_drops_it_and_sends_the_task_back_to_ci() {
    for phase in [
        StagePhase::AwaitingReview,
        StagePhase::RunningSession,
        StagePhase::PostingReview,
    ] {
        let task = reviewing(phase);
        let events = observe(&task, ChangeState::Open, OTHER);
        assert!(
            events.iter().any(
                |e| matches!(e, Event::StageRunCancelled { reason, .. } if reason == "head_moved")
            ),
            "{phase:?}"
        );
        let after = events.iter().fold(task, evolved);
        assert!(
            after.is_at(Stage::Merge, StagePhase::AwaitingCi),
            "{phase:?}"
        );
        assert_eq!(head_of(&after), OTHER);
        assert!(after.gates.is_empty());
    }
}

fn head_of(task: &Task) -> &str {
    &task.change_request.as_ref().unwrap().head_sha
}

#[test]
fn a_change_closed_during_the_review_blocks_the_task_and_one_merged_outside_ends_it() {
    let task = reviewing(StagePhase::RunningSession);
    let closed = observe(&task, ChangeState::Closed, HEAD)
        .iter()
        .fold(task.clone(), evolved);
    assert_eq!(closed.item.state, WorkItemState::Blocked);
    assert_eq!(
        closed.open_blocker().unwrap().code,
        BlockerCode::ChangeClosed
    );

    let events = observe(&task, ChangeState::Merged, HEAD);
    assert!(events.iter().any(
        |e| matches!(e, Event::StageRunCancelled { reason, .. } if reason == "change_merged")
    ));
    let merged = events.iter().fold(task, evolved);
    assert!(merged.is_at(Stage::Sync, StagePhase::Verifying) || merged.is_at_stage(Stage::Sync));
}
