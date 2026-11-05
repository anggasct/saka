#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::event::Event;
use saka_core::hook::{self, Facts, RepoRef};
use saka_core::ids::{ChangeRequestId, ProjectId, RepoId, SpecId, WorkItemId};
use saka_core::model::{ChangeRequest, Task, WorkItem};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    BlockerCode, ChangeCredentialKind, ChangeRole, ChangeState, HookEvent, WorkItemOrigin,
    WorkItemState,
};
use uuid::Uuid;

const WORK: WorkItemId = WorkItemId(Uuid::from_u128(1));
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn task() -> Task {
    let mut task = Task::new(WorkItem {
        id: WORK,
        project_id: ProjectId(Uuid::from_u128(2)),
        origin: WorkItemOrigin::ExternalChange,
        spec_id: Some(SpecId(Uuid::from_u128(3))),
        spec_revision_id: None,
        step_id: None,
        step_index: None,
        task_key: "tasks/s1".into(),
        work: "work".into(),
        repo_id: RepoId(Uuid::from_u128(4)),
        branch: "saka/tasks/s1".into(),
        state: WorkItemState::InStage,
        stage: None,
        stage_phase: None,
        priority: 0,
        version: 1,
        admitted_at: None,
        done_at: None,
        created_at: Timestamp(1),
        updated_at: Timestamp(1),
    });
    task.change_request = Some(ChangeRequest {
        id: ChangeRequestId(Uuid::from_u128(5)),
        work_item_id: WORK,
        role: ChangeRole::Code,
        repo_id: RepoId(Uuid::from_u128(4)),
        number: 42,
        url: "https://github.com/acme/api/pull/42".into(),
        head_branch: "saka/tasks/s1".into(),
        base_branch: "release/1".into(),
        head_sha: SHA.into(),
        state: ChangeState::Open,
        merge_commit_sha: None,
        merged_at: None,
        skipped_at: None,
        last_polled_at: None,
        created_at: Timestamp(1),
        updated_at: Timestamp(1),
    });
    task
}

fn facts() -> Facts {
    Facts {
        project_id: ProjectId(Uuid::from_u128(2)),
        project_name: "tasks".into(),
        repo: Some(RepoRef {
            id: RepoId(Uuid::from_u128(4)),
            name: "api".into(),
            full_name: "acme/api".into(),
        }),
        spec_path: Some("specs/auth.md".into()),
    }
}

fn context(event: &Event) -> Option<hook::HookContext> {
    hook::task_context(event, &task(), &facts(), Timestamp(1_793_786_400_123))
}

fn merged(role: ChangeRole) -> Event {
    Event::ChangeRequestMerged {
        work_item_id: WORK,
        change_request_id: ChangeRequestId(Uuid::from_u128(5)),
        role,
        merge_commit_sha: SHA.into(),
        merged_at: None,
        merged_by_saka: true,
    }
}

#[test]
fn each_trigger_event_becomes_a_context_with_the_sha_branch_and_references() {
    let ctx = context(&merged(ChangeRole::Code)).unwrap();
    assert_eq!(ctx.event, HookEvent::Merged);
    assert_eq!(ctx.sha.as_deref(), Some(SHA));
    assert_eq!(ctx.branch.as_deref(), Some("release/1"));
    assert_eq!(ctx.repo.as_ref().unwrap().name, "api");
    assert_eq!(ctx.task.as_ref().unwrap().key, "tasks/s1");
    assert_eq!(ctx.spec.as_ref().unwrap().path, "specs/auth.md");
    assert_eq!(ctx.change.as_ref().unwrap().number, 42);
    assert_eq!(ctx.occurred_at, "2026-11-04T10:00:00.123Z");

    let done = context(&Event::WorkItemDone {
        work_item_id: WORK,
        merge_commit_sha: SHA.into(),
    })
    .unwrap();
    assert_eq!(done.event, HookEvent::TaskDone);
    assert_eq!(done.sha.as_deref(), Some(SHA));

    let opened = context(&Event::ChangeRequestOpened {
        work_item_id: WORK,
        change_request_id: ChangeRequestId(Uuid::from_u128(5)),
        role: ChangeRole::Code,
        repo_id: RepoId(Uuid::from_u128(4)),
        number: 42,
        url: "u".into(),
        head_branch: "saka/tasks/s1".into(),
        base_branch: "release/1".into(),
        head_sha: SHA.into(),
        opened_with: ChangeCredentialKind::App,
    })
    .unwrap();
    assert_eq!(opened.event, HookEvent::ChangeOpened);
    assert_eq!(opened.sha.as_deref(), Some(SHA));
}

#[test]
fn a_blocker_triggers_with_its_code_except_hook_failed_which_never_triggers() {
    let blocked = |code| Event::WorkItemBlocked {
        work_item_id: WORK,
        stage_run_id: None,
        blocker_id: saka_core::ids::BlockerId(Uuid::from_u128(6)),
        code,
        params: serde_json::json!({}),
        stage: None,
        phase: None,
    };
    let ctx = context(&blocked(BlockerCode::CiRed)).unwrap();
    assert_eq!(ctx.event, HookEvent::Blocked);
    assert_eq!(ctx.blocker.unwrap().code, BlockerCode::CiRed);
    assert!(context(&blocked(BlockerCode::HookFailed)).is_none());
}

#[test]
fn docs_changes_and_unrelated_events_trigger_nothing() {
    assert!(context(&merged(ChangeRole::Docs)).is_none());
    assert!(
        context(&Event::WorkItemCancelled {
            work_item_id: WORK,
            reason: "x".into()
        })
        .is_none()
    );
}
