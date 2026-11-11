#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use std::collections::BTreeMap;

use saka_core::Ctx;
use saka_core::autonomy::{Ceilings, RepoPolicy};
use saka_core::event::Event;
use saka_core::ids::{
    ChangeRequestId, GateResultId, IdSource, ProjectId, RepoId, ReviewId, SessionId, StageRunId,
    WorkItemId,
};
use saka_core::model::{ChangeRequest, GateResult, Review, Task, WorkItem};
use saka_core::reconcile::{Action, Snapshot, reconcile};
use saka_core::review::Finding;
use saka_core::task::{self, LoopConfig, TaskInput};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    Autonomy, ChangeRole, ChangeState, GateOutcome, GateSource, MergeMethod, ReviewBasis,
    ReviewOutcome, Severity, Stage, StagePhase, Trigger, WorkItemOrigin, WorkItemState,
    WorkflowPreset,
};
use saka_core::workflow::Workflow;
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
        autonomy: Ceilings {
            default: Autonomy::Autonomous,
            repos: Vec::new(),
        },
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
        update_head_sha: None,
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

fn ready(workflow: Workflow) -> Task {
    let mut task = task_at_merge(workflow);
    task = evolved(
        task,
        &run_created(Stage::Merge, StagePhase::AwaitingMerge, 10),
    );
    task.gates = vec![gate(GateSource::Scm, HEAD, 10)];
    task.reviews = vec![review(HEAD, ReviewOutcome::Approved)];
    task.policy = RepoPolicy {
        full_name: "acme/app".into(),
        requested: None,
    };
    task
}

fn full(merge: Trigger) -> Workflow {
    let mut workflow = Workflow::of(WorkflowPreset::FullLoop);
    workflow.set_stage(Stage::Merge, merge);
    workflow
}

fn auto(task: &Task) -> bool {
    task::auto_merge(task, &cfg().autonomy)
}

#[test]
fn a_ready_task_in_an_autonomous_repo_with_an_automatic_merge_is_merged_by_the_loop() {
    let task = ready(full(Trigger::Auto));
    assert!(auto(&task));
    assert!(task::auto_merge_enabled(&task, &cfg().autonomy));
}

#[test]
fn nothing_but_an_autonomous_repo_with_an_automatic_merge_and_a_reviewer_merges_alone() {
    let mut task = ready(full(Trigger::Auto));
    task.policy.requested = Some(Autonomy::Standard);
    assert!(!auto(&task), "a repo file asks for a lower level");
    assert!(!task::auto_merge_enabled(&task, &cfg().autonomy));

    let mut standard = cfg().autonomy;
    standard.default = Autonomy::Standard;
    assert!(!task::auto_merge(&ready(full(Trigger::Auto)), &standard));

    assert!(
        !auto(&ready(full(Trigger::Manual))),
        "manual always means one tap"
    );

    let mut no_review = Workflow::of(WorkflowPreset::Custom);
    no_review.spec = true;
    no_review.set_stage(Stage::Dev, Trigger::Auto);
    no_review.set_stage(Stage::Merge, Trigger::Auto);
    assert!(
        !auto(&ready(no_review)),
        "nobody independent has judged the change"
    );

    let mut external = ready(full(Trigger::Auto));
    external.item.origin = WorkItemOrigin::ExternalChange;
    assert!(!auto(&external));
}

#[test]
fn an_unready_task_is_never_merged_by_the_loop() {
    let mut no_verdict = ready(full(Trigger::Auto));
    no_verdict.reviews = vec![review(OTHER, ReviewOutcome::Approved)];
    assert!(!auto(&no_verdict));
    assert!(task::auto_merge_enabled(&no_verdict, &cfg().autonomy));

    let mut changes = ready(full(Trigger::Auto));
    changes.reviews = vec![review(HEAD, ReviewOutcome::ChangesRequested)];
    assert!(!auto(&changes));

    let mut no_gate = ready(full(Trigger::Auto));
    no_gate.gates.clear();
    assert!(!auto(&no_gate));

    let waiting = evolved(
        ready(full(Trigger::Auto)),
        &run_created(Stage::Merge, StagePhase::AwaitingCi, 11),
    );
    assert!(!auto(&waiting));

    let mut taken = ready(full(Trigger::Auto));
    taken.item.state = WorkItemState::TakenOver;
    assert!(!auto(&taken));
}

fn requests(snapshot: &Snapshot) -> Vec<(WorkItemId, Option<String>)> {
    reconcile(snapshot, &cfg(), 4, NOW)
        .into_iter()
        .filter_map(|a| match a {
            Action::Decide(id, TaskInput::RequestMerge { reason, .. }) => Some((id, reason)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_reconciler_requests_the_merge_exactly_when_the_loop_may_and_the_task_is_ready() {
    let go = ready(full(Trigger::Auto));
    let id = go.item.id;
    let snapshot = Snapshot {
        tasks: vec![go],
        ..Snapshot::default()
    };
    assert_eq!(requests(&snapshot), [(id, Some("autonomy".to_string()))]);

    let mut manual = ready(full(Trigger::Manual));
    manual.item.id = WorkItemId(Uuid::from_u128(9));
    let mut unready = ready(full(Trigger::Auto));
    unready.reviews.clear();
    unready.item.id = WorkItemId(Uuid::from_u128(8));
    let snapshot = Snapshot {
        tasks: vec![manual, unready],
        ..Snapshot::default()
    };
    assert!(requests(&snapshot).is_empty());
}

#[test]
fn a_request_made_by_the_loop_takes_the_task_into_the_queue_like_a_tap() {
    let task = ready(full(Trigger::Auto));
    let mut ids = Ids(1_000);
    let events = task::decide(
        &task,
        TaskInput::RequestMerge {
            expected_version: task.item.version,
            reason: Some("autonomy".into()),
        },
        &cfg(),
        &mut Ctx {
            now: NOW,
            ids: &mut ids,
        },
    )
    .unwrap();
    let queued = events.iter().fold(task, evolved);
    assert!(queued.is_at(Stage::Merge, StagePhase::Queued));
    assert!(!task::merge_ready(&queued));
    assert!(!auto(&queued), "asked once, not again");
}

fn merging(native: bool) -> Task {
    let mut task = ready(full(Trigger::Auto));
    task = evolved(task, &run_created(Stage::Merge, StagePhase::Merging, 12));
    task.stage_run.as_mut().unwrap().queued_at = Some(NOW);
    task.stage_run.as_mut().unwrap().queue_native = native;
    task
}

fn observe(task: &Task, head: &str) -> Vec<Event> {
    let mut ids = Ids(2_000);
    task::decide(
        task,
        TaskInput::ChangeObserved {
            role: ChangeRole::Code,
            state: ChangeState::Open,
            head_sha: head.into(),
            merge_commit_sha: None,
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
fn the_poll_watches_a_merge_in_flight_and_a_head_that_moved_withdraws_it_without_a_blocker() {
    let task = merging(false);
    assert!(task::watches_change(&task));
    assert!(
        observe(&task, HEAD)
            .iter()
            .all(|e| !matches!(e, Event::MergeDequeued { .. }))
    );

    let events = observe(&task, OTHER);
    assert!(
        matches!(
            events.as_slice(),
            [
                Event::ChangeRequestUpdated { .. },
                Event::MergeDequeued { reason, .. },
                Event::StagePhaseChanged { phase: StagePhase::AwaitingCi, .. },
            ] if reason == "head_moved"
        ),
        "{events:?}"
    );
    let after = events.iter().fold(task, evolved);
    assert!(after.is_at(Stage::Merge, StagePhase::AwaitingCi));
    assert_eq!(after.change_request.as_ref().unwrap().head_sha, OTHER);
    assert!(after.open_blocker().is_none());
    assert!(after.stage_run.as_ref().unwrap().queued_at.is_none());
}

#[test]
fn a_merge_in_the_providers_own_queue_is_left_to_the_provider() {
    let task = merging(true);
    assert!(
        !observe(&task, OTHER)
            .iter()
            .any(|e| matches!(e, Event::MergeDequeued { .. }))
    );
}

#[test]
fn a_merge_that_is_no_longer_allowed_when_the_job_runs_goes_back_to_the_checks() {
    let task = merging(false);
    let mut ids = Ids(3_000);
    let events = task::decide(
        &task,
        TaskInput::MergeNotReady,
        &cfg(),
        &mut Ctx {
            now: NOW,
            ids: &mut ids,
        },
    )
    .unwrap();
    assert!(
        matches!(
            events.as_slice(),
            [
                Event::MergeDequeued { reason, .. },
                Event::StagePhaseChanged { phase: StagePhase::AwaitingCi, .. },
            ] if reason == "not_ready"
        ),
        "{events:?}"
    );

    let idle = ready(full(Trigger::Auto));
    assert!(
        task::decide(
            &idle,
            TaskInput::MergeNotReady,
            &cfg(),
            &mut Ctx {
                now: NOW,
                ids: &mut ids
            }
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn a_merge_is_allowed_only_at_the_recorded_head_with_clear_gates_and_a_verdict_on_it() {
    let task = ready(full(Trigger::Auto));
    assert!(task::merge_allowed(&task, HEAD));
    assert!(!task::merge_allowed(&task, OTHER));
    let mut no_verdict = task.clone();
    no_verdict.reviews.clear();
    assert!(!task::merge_allowed(&no_verdict, HEAD));
    let mut no_gate = task;
    no_gate.gates.clear();
    assert!(!task::merge_allowed(&no_gate, HEAD));
}
