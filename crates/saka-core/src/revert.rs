use crate::Ctx;
use crate::event::Event;
use crate::ids::WorkItemId;
use crate::model::{Task, TaskLink};
use crate::rejection::Rejection;
use crate::vocab::{BlockerResolution, ChangeRole, ChangeState, WorkItemOrigin, WorkItemState};

pub const PRIORITY: i64 = i32::MAX as i64;
const DEPLOY_UNVERIFIED: &str = "deploy_unverified";

fn held_by_deploy(task: &Task) -> bool {
    task.item.state == WorkItemState::Blocked
        && task
            .open_blocker()
            .is_some_and(|b| b.code.as_str() == DEPLOY_UNVERIFIED)
}

fn merged_commit(task: &Task) -> Option<&str> {
    task.change_request
        .as_ref()
        .filter(|cr| cr.role == ChangeRole::Code && cr.state == ChangeState::Merged)
        .and_then(|cr| cr.merge_commit_sha.as_deref())
}

pub fn eligible(task: &Task) -> bool {
    task.item.origin != WorkItemOrigin::Revert
        && merged_commit(task).is_some()
        && (task.item.state == WorkItemState::Done || held_by_deploy(task))
}

pub fn branch_of(task_key: &str) -> String {
    let slug: String = task_key
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("saka/revert/{slug}")
}

pub fn request(
    origin: &Task,
    reverts: &[TaskLink],
    title: &str,
    reason: String,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<Event>, Rejection> {
    if origin.item.origin == WorkItemOrigin::Revert {
        return Err(Rejection::RevertNotAllowed {
            reason: "revert_task",
        });
    }
    if !eligible(origin) {
        return Err(Rejection::InvalidState {
            state: origin.item.state.as_str().to_string(),
        });
    }
    if let Some(done) = reverts.iter().find(|r| r.state == WorkItemState::Done) {
        return Err(Rejection::AlreadyReverted {
            work_item_id: done.id.0.to_string(),
        });
    }
    if let Some(live) = reverts.iter().find(|r| {
        !matches!(
            r.state,
            WorkItemState::Cancelled | WorkItemState::Superseded
        )
    }) {
        return Err(Rejection::RevertInProgress {
            work_item_id: live.id.0.to_string(),
        });
    }
    let revert_id = WorkItemId::new(ctx.ids);
    let task_key = format!("{}-R{}", origin.item.task_key, reverts.len() + 1);
    Ok(vec![
        Event::WorkItemRevertRequested {
            work_item_id: origin.item.id,
            revert_id,
            reason: reason.clone(),
        },
        Event::WorkItemRevertCreated {
            work_item_id: revert_id,
            project_id: origin.item.project_id,
            repo_id: origin.item.repo_id,
            revert_of: origin.item.id,
            revert_of_key: origin.item.task_key.clone(),
            branch: branch_of(&task_key),
            task_key,
            work: title.to_string(),
            priority: PRIORITY,
            reason,
        },
    ])
}

pub fn completed(origin: &Task, revert_id: WorkItemId, merge_commit_sha: String) -> Vec<Event> {
    let mut events = vec![Event::WorkItemReverted {
        work_item_id: origin.item.id,
        revert_id,
        merge_commit_sha,
    }];
    if let Some(blocker) = origin.open_blocker().filter(|_| held_by_deploy(origin)) {
        events.push(Event::BlockerResolved {
            work_item_id: origin.item.id,
            blocker_id: blocker.id,
            resolution: BlockerResolution::Reverted,
            reason: Some("reverted".to_string()),
        });
        events.push(Event::WorkItemCancelled {
            work_item_id: origin.item.id,
            reason: "reverted".to_string(),
        });
    }
    events
}

#[cfg(test)]
mod tests {
    use super::branch_of;

    #[test]
    fn the_revert_branch_is_the_lowercased_key_without_characters_git_dislikes() {
        assert_eq!(branch_of("Two-Step/s1-R1"), "saka/revert/two-step/s1-r1");
        assert_eq!(branch_of("shop#12-R2"), "saka/revert/shop-12-r2");
    }
}
