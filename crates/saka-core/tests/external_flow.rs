#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail loudly on a broken setup"
)]

use saka_core::Ctx;
use saka_core::event::Event;
use saka_core::external::{self, Incoming};
use saka_core::ids::{IdSource, ProjectId, RepoId};
use saka_core::job::JobKind;
use saka_core::model::Task;
use saka_core::react::react;
use saka_core::rejection::Rejection;
use saka_core::task::{self, LoopConfig, TaskInput, available_actions};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    ChangeRole, ChangeState, ExternalCause, MergeMethod, Stage, StagePhase, StageRunState, Trigger,
    WorkItemState, WorkflowPreset,
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
const NEXT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn cfg() -> LoopConfig {
    LoopConfig {
        max_followup_turns: 1,
        permission_wait_ms: 24 * 3_600_000,
        decision_wait_ms: 72 * 3_600_000,
        idle_ms: 30 * 60_000,
        budgets: std::collections::BTreeMap::new(),
        budget_warn_percent: 80,
        rate_limit_pause: 0.9,
        account: "claude:default".into(),
        autonomy: saka_core::autonomy::Ceilings::default(),
        ci_appear_timeout_ms: 600_000,
        max_ci_reruns: 3,
        poll_interval_ms: 15_000,
        merge_method: MergeMethod::Squash,
        delete_branch_after_merge: true,
        commit_status: true,
    }
}

fn incoming() -> Incoming {
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
        from_fork: false,
        draft: false,
        spec: None,
    }
}

fn review_only(review: Trigger) -> Workflow {
    let mut w = Workflow::of(WorkflowPreset::ReviewOnly);
    w.set_stage(Stage::Review, review);
    w
}

struct Case {
    ids: Counter,
    task: Task,
}

fn ctx(ids: &mut Counter) -> Ctx<'_> {
    Ctx { now: NOW, ids }
}

fn accepted(workflow: &Workflow, cause: ExternalCause) -> Case {
    let mut ids = Counter(100);
    let events = external::accept(
        workflow,
        incoming(),
        cause,
        (cause != ExternalCause::Auto).then(|| "bob".to_string()),
        &mut ctx(&mut ids),
    )
    .unwrap();
    let mut task = None;
    for e in &events {
        task = task::evolve(task, e, NOW);
    }
    let mut task = task.unwrap();
    task.external.as_mut().unwrap().workflow = workflow.clone();
    Case { ids, task }
}

fn apply(case: &mut Case, events: &[Event]) {
    for e in events {
        case.task = task::evolve(Some(case.task.clone()), e, NOW).unwrap();
    }
}

fn decide(case: &mut Case, input: TaskInput) -> Result<Vec<Event>, Rejection> {
    let events = task::decide(&case.task, input, &cfg(), &mut ctx(&mut case.ids))?;
    apply(case, &events);
    Ok(events)
}

fn observed(state: ChangeState, head: &str, merge: Option<&str>) -> TaskInput {
    TaskInput::ChangeObserved {
        role: ChangeRole::Code,
        state,
        head_sha: head.into(),
        merge_commit_sha: merge.map(String::from),
        merged_at: None,
        checks: Vec::new(),
    }
}

fn request(head: &str, cause: ExternalCause) -> TaskInput {
    TaskInput::ReviewRequested {
        head_sha: head.into(),
        cause,
        actor: Some("bob".into()),
    }
}

#[test]
fn an_auto_review_trigger_queues_a_pending_run_at_the_head_when_the_change_is_taken() {
    let case = accepted(&review_only(Trigger::Auto), ExternalCause::Auto);
    let t = &case.task;
    assert!(t.is_at(Stage::Review, StagePhase::AwaitingReview));
    let run = t.stage_run.as_ref().unwrap();
    assert_eq!(
        (run.state, run.trigger, run.attempt),
        (StageRunState::Pending, Trigger::Auto, 1)
    );
    assert_eq!(run.head_sha.as_deref(), Some(HEAD));
    assert_eq!(run.reason, None);
    assert_eq!(t.item.task_key, "shop#7");
    assert_eq!(t.item.spec_id, None);
    assert!(available_actions(t).iter().all(|a| *a != "review"));
}

#[test]
fn a_manual_review_trigger_waits_unless_a_command_took_the_change() {
    let waiting = accepted(&review_only(Trigger::Manual), ExternalCause::Auto);
    assert!(
        waiting
            .task
            .is_at(Stage::Review, StagePhase::AwaitingTrigger)
    );
    assert!(waiting.task.stage_run.is_none());
    assert!(available_actions(&waiting.task).contains(&"review"));

    let asked = accepted(&review_only(Trigger::Manual), ExternalCause::Comment);
    assert!(asked.task.is_at(Stage::Review, StagePhase::AwaitingReview));
    let run = asked.task.stage_run.as_ref().unwrap();
    assert_eq!(run.trigger, Trigger::Manual);
    assert_eq!(run.reason.as_deref(), Some("comment by bob"));
}

#[test]
fn a_request_at_a_head_that_already_has_one_is_refused_and_a_new_head_is_not() {
    let mut case = accepted(&review_only(Trigger::Manual), ExternalCause::Comment);
    let again = decide(&mut case, request(HEAD, ExternalCause::Label)).unwrap_err();
    assert_eq!(
        again,
        Rejection::ReviewAlreadyRequested {
            head_sha: HEAD.into()
        }
    );
    let events = decide(&mut case, request(NEXT, ExternalCause::Label)).unwrap();
    assert!(matches!(events[0], Event::ChangeRequestUpdated { .. }));
    let run = case.task.stage_run.as_ref().unwrap();
    assert_eq!(run.attempt, 2);
    assert_eq!(run.head_sha.as_deref(), Some(NEXT));
    assert_eq!(case.task.retired_runs[0].state, StageRunState::Cancelled);
    assert_eq!(
        case.task.change_request.as_ref().unwrap().head_sha,
        NEXT,
        "a command made at a newer head moves the head too"
    );
}

#[test]
fn a_workflow_without_review_or_a_closed_change_refuses_the_request() {
    let mut case = accepted(&review_only(Trigger::Manual), ExternalCause::Auto);
    case.task.external.as_mut().unwrap().workflow = Workflow::of(WorkflowPreset::BuildOnly);
    assert_eq!(
        decide(&mut case, request(NEXT, ExternalCause::Operator)).unwrap_err(),
        Rejection::StageNotInWorkflow { stage: "review" }
    );
    let mut case = accepted(&review_only(Trigger::Manual), ExternalCause::Auto);
    decide(&mut case, observed(ChangeState::Closed, HEAD, None)).unwrap();
    assert_eq!(case.task.item.state, WorkItemState::Cancelled);
    assert!(matches!(
        decide(&mut case, request(HEAD, ExternalCause::Operator)).unwrap_err(),
        Rejection::InvalidState { .. }
    ));
}

#[test]
fn a_head_that_moves_replaces_the_request_when_auto_and_resets_to_waiting_when_manual() {
    let mut auto = accepted(&review_only(Trigger::Auto), ExternalCause::Auto);
    decide(&mut auto, observed(ChangeState::Open, NEXT, None)).unwrap();
    let run = auto.task.stage_run.as_ref().unwrap();
    assert_eq!(
        (run.attempt, run.head_sha.as_deref(), run.trigger),
        (2, Some(NEXT), Trigger::Auto)
    );
    assert!(auto.task.is_at(Stage::Review, StagePhase::AwaitingReview));

    let mut manual = accepted(&review_only(Trigger::Manual), ExternalCause::Comment);
    decide(&mut manual, observed(ChangeState::Open, NEXT, None)).unwrap();
    assert!(
        manual
            .task
            .is_at(Stage::Review, StagePhase::AwaitingTrigger)
    );
    assert_eq!(
        manual.task.stage_run.as_ref().unwrap().state,
        StageRunState::Cancelled
    );
    assert_eq!(manual.task.change_request.as_ref().unwrap().head_sha, NEXT);
    assert!(
        decide(&mut manual, observed(ChangeState::Open, NEXT, None))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn merge_ends_the_task_done_and_close_ends_it_cancelled_without_a_pending_request() {
    let mut merged = accepted(&review_only(Trigger::Auto), ExternalCause::Auto);
    let events = decide(&mut merged, observed(ChangeState::Merged, HEAD, Some(NEXT))).unwrap();
    assert!(matches!(events.last(), Some(Event::WorkItemDone { .. })));
    assert_eq!(merged.task.item.state, WorkItemState::Done);
    assert_eq!(
        merged.task.stage_run.as_ref().unwrap().state,
        StageRunState::Cancelled
    );
    let mut missing_commit = accepted(&review_only(Trigger::Auto), ExternalCause::Auto);
    assert!(
        decide(
            &mut missing_commit,
            observed(ChangeState::Merged, HEAD, None)
        )
        .unwrap()
        .is_empty()
    );

    let mut closed = accepted(&review_only(Trigger::Auto), ExternalCause::Auto);
    decide(&mut closed, observed(ChangeState::Closed, HEAD, None)).unwrap();
    assert_eq!(closed.task.item.state, WorkItemState::Cancelled);
    assert_eq!(
        closed.task.stage_run.as_ref().unwrap().state,
        StageRunState::Cancelled
    );
}

#[test]
fn only_the_inputs_an_outside_change_understands_are_accepted() {
    let mut case = accepted(&review_only(Trigger::Auto), ExternalCause::Auto);
    assert!(decide(&mut case, TaskInput::Tick).unwrap().is_empty());
    assert_eq!(
        decide(&mut case, TaskInput::Admit).unwrap_err(),
        Rejection::Unsupported
    );
    decide(
        &mut case,
        TaskInput::Cancel {
            reason: "not needed".into(),
        },
    )
    .unwrap();
    assert_eq!(case.task.item.state, WorkItemState::Cancelled);
    assert!(matches!(
        decide(
            &mut case,
            TaskInput::Cancel {
                reason: "again".into()
            }
        )
        .unwrap_err(),
        Rejection::InvalidState { .. }
    ));
}

#[test]
fn a_live_outside_change_is_polled_and_a_finished_one_is_not() {
    let mut case = accepted(&review_only(Trigger::Manual), ExternalCause::Auto);
    let polls = |t: &Task, e: &Event| {
        react(t, e, &cfg(), NOW)
            .into_iter()
            .filter(|j| j.kind == JobKind::ScmPollChangeRequest)
            .count()
    };
    let queued = Event::StageRunCancelled {
        work_item_id: case.task.item.id,
        stage_run_id: saka_core::ids::StageRunId(Uuid::from_u128(999)),
        reason: "head_moved".into(),
    };
    assert!(saka_core::task::watches_change(&case.task));
    let events = decide(&mut case, request(HEAD, ExternalCause::Operator)).unwrap();
    assert_eq!(polls(&case.task, &events[0]), 1);
    decide(&mut case, observed(ChangeState::Closed, HEAD, None)).unwrap();
    assert!(!saka_core::task::watches_change(&case.task));
    assert_eq!(polls(&case.task, &queued), 0);
}
