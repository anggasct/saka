#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::Ctx;
use saka_core::autonomy::{Ceilings, RepoPolicy};
use saka_core::event::Event;
use saka_core::ids::{IdSource, ProjectId, RepoId, SpecId, SpecRevisionId, WorkItemId};
use saka_core::model::{Spec, Task, WorkItem, WorkItemDependency};
use saka_core::reconcile::{Action, Snapshot, reconcile};
use saka_core::rejection::Rejection;
use saka_core::task::{self, LoopConfig, TaskInput};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    Autonomy, MergeMethod, SpecOrigin, SpecState, WorkItemOrigin, WorkItemState,
};
use uuid::Uuid;

struct Counter(u128);

impl IdSource for Counter {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

const PROJECT: ProjectId = ProjectId(Uuid::from_u128(900));
const REPO: RepoId = RepoId(Uuid::from_u128(901));
const SPEC: SpecId = SpecId(Uuid::from_u128(1));
const REVISION: SpecRevisionId = SpecRevisionId(Uuid::from_u128(10));
const NOW: Timestamp = Timestamp(5_000);

fn cfg(ceiling: Autonomy) -> LoopConfig {
    LoopConfig {
        max_followup_turns: 1,
        permission_wait_ms: 24 * 3_600_000,
        idle_ms: 30 * 60_000,
        budgets: std::collections::BTreeMap::new(),
        budget_warn_percent: 80,
        rate_limit_pause: 0.9,
        account: "claude:default".into(),
        autonomy: Ceilings {
            default: ceiling,
            repos: Vec::new(),
        },
        ci_appear_timeout_ms: 600_000,
        poll_interval_ms: 15_000,
        merge_method: MergeMethod::Squash,
        delete_branch_after_merge: true,
        commit_status: true,
    }
}

fn spec() -> Spec {
    Spec {
        id: SPEC,
        project_id: PROJECT,
        path: "docs/specs/t.md".into(),
        slug: "t".into(),
        title: "T".into(),
        state: SpecState::Approved,
        origin: SpecOrigin::Manual,
        priority: 0,
        latest_revision_id: REVISION,
        approved_revision_id: Some(REVISION),
        approved_at: Some(Timestamp(1_000)),
        shipped_at: None,
        created_at: Timestamp(1),
        updated_at: Timestamp(1_000),
    }
}

fn task(n: u32, state: WorkItemState, approved: bool, requested: Option<Autonomy>) -> Task {
    let mut t = Task::new(WorkItem {
        id: WorkItemId(Uuid::from_u128(100 + u128::from(n))),
        project_id: PROJECT,
        origin: WorkItemOrigin::SpecStep,
        spec_id: Some(SPEC),
        spec_revision_id: Some(REVISION),
        step_id: Some(format!("S{n}")),
        step_index: Some(n),
        task_key: format!("t/s{n}"),
        work: format!("work {n}"),
        repo_id: REPO,
        branch: format!("saka/t/s{n}"),
        state,
        stage: None,
        stage_phase: None,
        priority: 0,
        version: 3,
        admitted_at: None,
        done_at: None,
        start_approved_at: approved.then_some(Timestamp(2_000)),
        created_at: Timestamp(1_000),
        updated_at: Timestamp(1_000),
    });
    t.policy = RepoPolicy {
        full_name: "acme/app".into(),
        requested,
    };
    t
}

fn snapshot(tasks: Vec<Task>) -> Snapshot {
    let ids: Vec<WorkItemId> = tasks.iter().map(|t| t.item.id).collect();
    let deps = ids
        .windows(2)
        .map(|w| WorkItemDependency {
            work_item_id: w[1],
            depends_on_work_item_id: w[0],
            project_id: PROJECT,
        })
        .collect();
    Snapshot {
        specs: vec![spec()],
        tasks,
        work_item_dependencies: deps,
        ..Snapshot::default()
    }
}

fn decisions(snapshot: &Snapshot, ceiling: Autonomy) -> Vec<(u128, &'static str)> {
    reconcile(snapshot, &cfg(ceiling), 4, NOW)
        .into_iter()
        .filter_map(|a| match a {
            Action::Decide(id, TaskInput::Admit) => Some((id.0.as_u128() - 100, "admit")),
            Action::Decide(id, TaskInput::AwaitStart) => {
                Some((id.0.as_u128() - 100, "await_start"))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_strict_repo_asks_for_the_first_ready_step_and_admits_nothing() {
    let s = snapshot(vec![
        task(1, WorkItemState::Queued, false, None),
        task(2, WorkItemState::Queued, false, None),
    ]);
    assert_eq!(
        decisions(&s, Autonomy::Strict),
        [(1, "await_start")],
        "step 2 waits for step 1, so nobody asks about it yet"
    );
}

#[test]
fn a_repo_file_can_make_a_standard_repo_strict_but_not_the_other_way() {
    let ready = |requested| snapshot(vec![task(1, WorkItemState::Queued, false, requested)]);
    assert_eq!(
        decisions(&ready(Some(Autonomy::Strict)), Autonomy::Autonomous),
        [(1, "await_start")]
    );
    assert_eq!(
        decisions(&ready(Some(Autonomy::Autonomous)), Autonomy::Strict),
        [(1, "await_start")]
    );
    assert_eq!(decisions(&ready(None), Autonomy::Standard), [(1, "admit")]);
    assert_eq!(
        decisions(&ready(None), Autonomy::Autonomous),
        [(1, "admit")]
    );
}

#[test]
fn a_started_step_is_admitted_like_any_other_and_a_waiting_step_is_left_alone() {
    let started = snapshot(vec![task(1, WorkItemState::Queued, true, None)]);
    assert_eq!(decisions(&started, Autonomy::Strict), [(1, "admit")]);
    let waiting = snapshot(vec![task(1, WorkItemState::AwaitingStart, false, None)]);
    assert!(decisions(&waiting, Autonomy::Strict).is_empty());
}

#[test]
fn a_spec_with_a_cancelled_step_asks_for_no_more_starts() {
    let s = snapshot(vec![
        task(1, WorkItemState::Cancelled, false, None),
        task(2, WorkItemState::Queued, false, None),
    ]);
    assert!(decisions(&s, Autonomy::Strict).is_empty());
}

#[test]
fn a_step_moves_to_awaiting_start_and_back_to_queued_only_by_the_start_action() {
    let queued = task(1, WorkItemState::Queued, false, None);
    let mut ids = Counter(500);
    let mut ctx = Ctx {
        now: NOW,
        ids: &mut ids,
    };
    let c = cfg(Autonomy::Strict);
    let events = task::decide(&queued, TaskInput::AwaitStart, &c, &mut ctx).unwrap();
    assert!(matches!(events[..], [Event::WorkItemAwaitingStart { .. }]));
    let waiting = events.iter().fold(queued.clone(), |t, e| {
        task::evolve(Some(t), e, NOW).unwrap()
    });
    assert_eq!(waiting.item.state, WorkItemState::AwaitingStart);
    assert_eq!(
        task::available_actions(&waiting),
        ["start", "cancel"],
        "a step that waits can be started or cancelled"
    );
    assert!(
        task::decide(&waiting, TaskInput::AwaitStart, &c, &mut ctx)
            .unwrap()
            .is_empty()
    );

    let stale = TaskInput::ApproveStart {
        expected_version: 2,
    };
    assert_eq!(
        task::decide(&waiting, stale, &c, &mut ctx),
        Err(Rejection::VersionStale)
    );
    let start = TaskInput::ApproveStart {
        expected_version: waiting.item.version,
    };
    assert_eq!(
        task::decide(&queued, start.clone(), &c, &mut ctx),
        Err(Rejection::VersionStale),
        "the version moves with every event"
    );
    let events = task::decide(&waiting, start, &c, &mut ctx).unwrap();
    let started = events.iter().fold(waiting.clone(), |t, e| {
        task::evolve(Some(t), e, NOW).unwrap()
    });
    assert_eq!(started.item.state, WorkItemState::Queued);
    assert_eq!(started.item.start_approved_at, Some(NOW));
    assert!(!started.needs_start_approval(&c.autonomy));
    assert!(!task::available_actions(&started).contains(&"start"));
}

#[test]
fn a_waiting_step_can_be_cancelled_and_a_started_one_cannot_be_started_again() {
    let waiting = task(1, WorkItemState::AwaitingStart, false, None);
    let mut ids = Counter(500);
    let mut ctx = Ctx {
        now: NOW,
        ids: &mut ids,
    };
    let c = cfg(Autonomy::Strict);
    let events = task::decide(
        &waiting,
        TaskInput::Cancel {
            reason: "not now".into(),
        },
        &c,
        &mut ctx,
    )
    .unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::WorkItemCancelled { .. }))
    );
    let queued = task(1, WorkItemState::Queued, true, None);
    assert!(matches!(
        task::decide(
            &queued,
            TaskInput::ApproveStart {
                expected_version: 3
            },
            &c,
            &mut ctx
        ),
        Err(Rejection::InvalidState { .. })
    ));
}
