#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::Ctx;
use saka_core::event::Event;
use saka_core::ids::{IdSource, ProjectId, RepoId, SpecId, SpecRevisionId, WorkItemId};
use saka_core::model::{SpecStanding, Task, TaskLink, WorkItem};
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
        work: "work 2".into(),
        repo_id: RepoId(Uuid::from_u128(901)),
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

fn request(origin: &Task) -> Result<Vec<Event>, Rejection> {
    let mut ids = Counter(500);
    let mut ctx = Ctx {
        now: NOW,
        ids: &mut ids,
    };
    redo::request(origin, "again".into(), Some("note".into()), &mut ctx)
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
fn only_a_cancelled_step_of_an_approved_spec_at_the_approved_revision_can_be_redone() {
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

    let mut moved = cancelled("shop/s2", "saka/shop/s2");
    moved.spec_standing = Some(SpecStanding {
        state: SpecState::Approved,
        approved_revision_id: Some(SpecRevisionId(Uuid::from_u128(11))),
    });
    assert!(matches!(
        request(&moved),
        Err(Rejection::RedoRevisionChanged)
    ));

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
