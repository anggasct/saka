#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::Ctx;
use saka_core::event::Event;
use saka_core::ids::{IdSource, ProjectId, RepoId, SpecId, SpecRevisionId, WorkItemId};
use saka_core::model::{
    ProjectRepo, RevisionView, SpecStanding, StepPlan, Task, TaskLink, WorkItem,
};
use saka_core::redo;
use saka_core::rejection::Rejection;
use saka_core::task;
use saka_core::time::Timestamp;
use saka_core::vocab::{SpecState, SupersedeReason, WorkItemOrigin, WorkItemState, WorkKind};
use uuid::Uuid;

struct Counter(u128);

impl IdSource for Counter {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

const REVISION: SpecRevisionId = SpecRevisionId(Uuid::from_u128(10));
const NOW: Timestamp = Timestamp(5_000);
const NEW_REVISION: SpecRevisionId = SpecRevisionId(Uuid::from_u128(11));
const REPO: RepoId = RepoId(Uuid::from_u128(901));

fn step(id: &str, index: u32, after: &[&str]) -> StepPlan {
    StepPlan {
        id: id.into(),
        index,
        work: format!("work {id}"),
        ac: Vec::new(),
        commands: Vec::new(),
        after: after.iter().map(|a| (*a).into()).collect(),
        repo: None,
        line: 0,
    }
}

fn revision(id: SpecRevisionId, steps: Vec<StepPlan>) -> RevisionView {
    RevisionView {
        id,
        lint_ok: true,
        issues: serde_json::Value::Null,
        steps,
        parser_version: 2,
        kind: WorkKind::Bugfix,
        priority: 7,
        depends_on: Vec::new(),
    }
}

fn repos() -> Vec<ProjectRepo> {
    vec![ProjectRepo {
        repo_id: REPO,
        name: "app".into(),
        is_default: true,
        base_branch: "main".into(),
        path_scope: vec!["**".into()],
        autonomy: None,
    }]
}

fn cancelled(key: &str, branch: &str) -> Task {
    let mut t = Task::new(WorkItem {
        kind: Some(WorkKind::Bugfix),
        id: WorkItemId(Uuid::from_u128(101)),
        project_id: ProjectId(Uuid::from_u128(900)),
        origin: WorkItemOrigin::SpecStep,
        spec_id: Some(SpecId(Uuid::from_u128(1))),
        spec_revision_id: Some(REVISION),
        step_id: Some("S2".into()),
        step_index: Some(2),
        task_key: key.into(),
        work: "work S2".into(),
        repo_id: REPO,
        branch: branch.into(),
        state: WorkItemState::Cancelled,
        stage: None,
        stage_phase: None,
        priority: 7,
        version: 4,
        admitted_at: None,
        done_at: None,
        start_approved_at: None,
        created_at: Timestamp(1_000),
        updated_at: Timestamp(1_000),
    });
    t.spec_standing = Some(SpecStanding {
        state: SpecState::Approved,
        approved_revision_id: Some(REVISION),
    });
    t
}

fn request_in(
    origin: &Task,
    current: &RevisionView,
    tasks: &[WorkItem],
) -> Result<redo::Requested, Rejection> {
    let mut ids = Counter(500);
    let mut ctx = Ctx {
        now: NOW,
        ids: &mut ids,
    };
    let repos = repos();
    let current = redo::Current {
        revision: current,
        repos: &repos,
        tasks,
    };
    redo::request(
        origin,
        "again".into(),
        Some("note".into()),
        Some(&current),
        &mut ctx,
    )
}

fn request(origin: &Task) -> Result<Vec<Event>, Rejection> {
    let current = revision(REVISION, vec![step("S1", 1, &[]), step("S2", 2, &["S1"])]);
    request_in(origin, &current, &[]).map(|r| r.events)
}

fn created(events: &[Event]) -> Task {
    let made = events
        .iter()
        .find(|e| matches!(e, Event::WorkItemRedoCreated { .. }))
        .unwrap();
    task::evolve(None, made, NOW).unwrap()
}

#[test]
fn a_redo_supersedes_the_original_and_creates_the_same_step_as_attempt_two() {
    let origin = cancelled("shop/s2", "saka/shop/s2");
    let events = request(&origin).unwrap();
    assert!(matches!(
        events[..],
        [
            Event::WorkItemRedoRequested { .. },
            Event::WorkItemSuperseded {
                reason: SupersedeReason::Redone,
                ..
            },
            Event::WorkItemRedoCreated { .. }
        ]
    ));
    let redo = created(&events);
    assert_eq!(redo.item.task_key, "shop/s2.2");
    assert_eq!(redo.item.branch, "saka/shop/s2-2");
    assert_eq!(redo.item.state, WorkItemState::Queued);
    assert_eq!(redo.item.step_id.as_deref(), Some("S2"));
    assert_eq!(redo.item.spec_revision_id, Some(REVISION));
    assert_eq!(redo.item.kind, Some(WorkKind::Bugfix));
    assert_eq!(redo.item.priority, 7);
    let link = redo.redo.unwrap();
    assert_eq!(link.of, origin.item.id);
    assert_eq!(link.of_key, "shop/s2");
    assert_eq!(link.note.as_deref(), Some("note"));
}

#[test]
fn a_redo_of_a_redo_counts_up_instead_of_nesting() {
    let first = request(&cancelled("shop/s2", "saka/shop/s2")).unwrap();
    let mut second_origin = created(&first);
    second_origin.item.state = WorkItemState::Cancelled;
    second_origin.spec_standing = cancelled("x", "y").spec_standing;
    let second = created(&request(&second_origin).unwrap());
    assert_eq!(second.item.task_key, "shop/s2.3");
    assert_eq!(second.item.branch, "saka/shop/s2-3");
}

#[test]
fn only_a_cancelled_step_of_an_approved_spec_can_be_redone() {
    let mut running = cancelled("shop/s2", "saka/shop/s2");
    running.item.state = WorkItemState::InStage;
    assert!(matches!(
        request(&running),
        Err(Rejection::InvalidState { .. })
    ));

    let mut withdrawn = cancelled("shop/s2", "saka/shop/s2");
    withdrawn.spec_standing = Some(SpecStanding {
        state: SpecState::Withdrawn,
        approved_revision_id: Some(REVISION),
    });
    assert!(matches!(
        request(&withdrawn),
        Err(Rejection::InvalidState { .. })
    ));
    assert!(!redo::eligible(&withdrawn));

    let mut outside = cancelled("shop/s2", "saka/shop/s2");
    outside.item.origin = WorkItemOrigin::Revert;
    assert!(matches!(
        request(&outside),
        Err(Rejection::InvalidState { .. })
    ));
}

#[test]
fn a_step_that_was_redone_says_so_before_anything_else() {
    let mut origin = cancelled("shop/s2", "saka/shop/s2");
    origin.item.state = WorkItemState::Superseded;
    origin.redone_by = Some(TaskLink {
        id: WorkItemId(Uuid::from_u128(102)),
        key: "shop/s2.2".into(),
        state: WorkItemState::Queued,
    });
    assert!(matches!(
        request(&origin),
        Err(Rejection::AlreadyRedone { .. })
    ));
    assert!(!redo::eligible(&origin));
}

#[test]
fn a_cancelled_step_is_eligible_until_it_has_a_redo() {
    let origin = cancelled("shop/s2", "saka/shop/s2");
    assert!(redo::eligible(&origin));
    assert!(task::available_actions(&origin).contains(&"redo"));
}

#[test]
fn a_redo_follows_the_approved_revision_for_the_index_the_work_and_the_dependencies() {
    let mut origin = cancelled("shop/s2", "saka/shop/s2");
    origin.spec_standing = Some(SpecStanding {
        state: SpecState::Approved,
        approved_revision_id: Some(NEW_REVISION),
    });
    let mut inserted = step("S2", 3, &["S1", "S0"]);
    inserted.work = "work S2 as approved".into();
    let current = revision(
        NEW_REVISION,
        vec![step("S0", 1, &[]), step("S1", 2, &[]), inserted],
    );
    let live = |step_id: &str, n: u128, state: WorkItemState| {
        let mut item = cancelled("shop/x", "saka/shop/x").item;
        item.id = WorkItemId(Uuid::from_u128(n));
        item.step_id = Some(step_id.into());
        item.state = state;
        item
    };
    let tasks = [
        live("S0", 200, WorkItemState::Done),
        live("S1", 201, WorkItemState::Superseded),
        live("S1", 202, WorkItemState::InStage),
    ];
    let requested = request_in(&origin, &current, &tasks).unwrap();
    let redo = created(&requested.events);
    assert_eq!(redo.item.spec_revision_id, Some(NEW_REVISION));
    assert_eq!(redo.item.step_index, Some(3));
    assert_eq!(redo.item.work, "work S2 as approved");
    assert_eq!(
        requested.depends_on,
        vec![
            WorkItemId(Uuid::from_u128(202)),
            WorkItemId(Uuid::from_u128(200))
        ]
    );
    assert!(requested.events.iter().any(|e| matches!(
        e,
        Event::WorkItemSuperseded { spec_revision_id, .. } if *spec_revision_id == NEW_REVISION
    )));
}

#[test]
fn a_step_missing_from_the_approved_revision_cannot_be_redone() {
    let origin = cancelled("shop/s2", "saka/shop/s2");
    let current = revision(REVISION, vec![step("S1", 1, &[])]);
    assert!(matches!(
        request_in(&origin, &current, &[]),
        Err(Rejection::RedoStepRemoved { step_id }) if step_id == "S2"
    ));
}
