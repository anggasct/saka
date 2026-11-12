#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::autonomy::Ceilings;
use saka_core::blocker::primary_action;
use saka_core::ids::{BlockerId, ProjectId, RepoId, SpecId, SpecRevisionId, WorkItemId};
use saka_core::inbox::{Scope, Source, desired, dismissible, scope};
use saka_core::model::{Blocker, Spec, Task, WorkItem};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    BlockerCode, InboxActionKind, InboxKind, InboxSubject, SpecOrigin, SpecState, WorkItemOrigin,
    WorkItemState,
};
use serde_json::json;
use uuid::Uuid;

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn task(state: WorkItemState) -> Task {
    Task::new(WorkItem {
        kind: Some(saka_core::vocab::WorkKind::Feature),
        id: WorkItemId(id(1)),
        project_id: ProjectId(id(2)),
        origin: WorkItemOrigin::SpecStep,
        spec_id: Some(SpecId(id(3))),
        spec_revision_id: Some(SpecRevisionId(id(4))),
        step_id: Some("s1".into()),
        step_index: Some(1),
        task_key: "feat-x/s1".into(),
        work: "Do it".into(),
        repo_id: RepoId(id(5)),
        branch: "saka/feat-x/s1".into(),
        state,
        stage: None,
        stage_phase: None,
        priority: 0,
        version: 7,
        admitted_at: None,
        done_at: None,
        start_approved_at: None,
        created_at: Timestamp(1),
        updated_at: Timestamp(1),
    })
}

fn blocked(code: BlockerCode, params: serde_json::Value) -> Task {
    let mut t = task(WorkItemState::Blocked);
    t.blocker = Some(Blocker {
        id: BlockerId(id(9)),
        work_item_id: t.item.id,
        stage_run_id: None,
        code,
        params,
        resolution: None,
        opened_at: Timestamp(2),
        resolved_at: None,
    });
    t
}

fn spec(state: SpecState) -> Spec {
    Spec {
        kind: saka_core::vocab::WorkKind::Feature,
        id: SpecId(id(3)),
        project_id: ProjectId(id(2)),
        path: "specs/feat-x.md".into(),
        slug: "feat-x".into(),
        title: "Feat X".into(),
        state,
        origin: SpecOrigin::Manual,
        priority: 0,
        latest_revision_id: SpecRevisionId(id(4)),
        approved_revision_id: None,
        approved_at: None,
        shipped_at: None,
        created_at: Timestamp(1),
        updated_at: Timestamp(1),
    }
}

#[test]
fn every_blocker_code_opens_one_item_whose_action_is_the_primary_action() {
    for code in BlockerCode::ALL {
        let t = blocked(*code, json!({ "names": ["A"], "integration_id": "gh" }));
        let items = desired(Source::Task(&t), &Ceilings::default());
        assert_eq!(items.len(), 1, "{code}");
        let item = &items[0];
        let primary = primary_action(&t).unwrap();
        assert_eq!(
            item.action.kind,
            InboxActionKind::from(primary.kind),
            "{code}"
        );
        assert_eq!(item.subject, InboxSubject::Blocker, "{code}");
        assert_eq!(item.source_id, id(9).to_string(), "{code}");
        let expected = if *code == BlockerCode::SpecUnclear {
            InboxKind::SpecUnclear
        } else {
            InboxKind::Blocker
        };
        assert_eq!(item.kind, expected, "{code}");
        assert_eq!(item.params["code"], json!(code), "{code}");
        assert_eq!(item.params["names"], json!(["A"]), "{code}");
        let mut params = primary.params;
        params["blocker_id"] = json!(BlockerId(id(9)));
        assert_eq!(item.action.params, params, "{code}");
    }
}

#[test]
fn a_task_the_operator_holds_has_no_item() {
    let mut t = blocked(BlockerCode::CiRed, json!({}));
    t.item.state = WorkItemState::TakenOver;
    assert!(desired(Source::Task(&t), &Ceilings::default()).is_empty());
}

#[test]
fn a_task_waiting_to_start_asks_for_a_start_at_its_current_version() {
    let t = task(WorkItemState::AwaitingStart);
    let items = desired(Source::Task(&t), &Ceilings::default());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].subject, InboxSubject::StepStart);
    assert_eq!(items[0].action.kind, InboxActionKind::StartStep);
    assert_eq!(
        items[0].action.params,
        json!({ "work_item_id": t.item.id, "version": 7 })
    );
}

#[test]
fn a_ready_spec_asks_for_approval_of_its_latest_revision_and_nothing_else_does() {
    let items = desired(Source::Spec(&spec(SpecState::Ready)), &Ceilings::default());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].source_id, format!("{}:{}", id(3), id(4)));
    assert_eq!(items[0].action.kind, InboxActionKind::ApproveSpec);
    for state in [
        SpecState::Draft,
        SpecState::Approved,
        SpecState::Shipped,
        SpecState::Withdrawn,
    ] {
        assert!(desired(Source::Spec(&spec(state)), &Ceilings::default()).is_empty());
    }
}

#[test]
fn only_a_spec_approval_can_be_dismissed() {
    for kind in InboxKind::ALL {
        for subject in InboxSubject::ALL {
            let expected = *kind == InboxKind::Approval && *subject == InboxSubject::Spec;
            assert_eq!(dismissible(*kind, *subject), expected, "{kind} {subject}");
        }
    }
}

#[test]
fn a_source_names_the_scope_it_owns() {
    let t = task(WorkItemState::Queued);
    assert_eq!(scope(Source::Task(&t)), Scope::WorkItem(t.item.id));
    assert_eq!(
        scope(Source::Spec(&spec(SpecState::Ready))),
        Scope::Spec(SpecId(id(3)))
    );
}
