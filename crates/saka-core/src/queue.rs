use std::cmp::Reverse;
use std::collections::BTreeMap;

use crate::ids::{RepoId, WorkItemId};
use crate::model::Task;
use crate::vocab::{Stage, StagePhase, WorkItemOrigin, WorkItemState};

fn group(task: &Task) -> Option<(RepoId, &str)> {
    let cr = task.change_request.as_ref()?;
    let queued = task.item.origin == WorkItemOrigin::SpecStep
        && task.item.state == WorkItemState::InStage
        && task.item.stage == Some(Stage::Merge)
        && task.is_queued();
    queued.then_some((cr.repo_id, cr.base_branch.as_str()))
}

pub fn holds(task: &Task) -> bool {
    group(task).is_some()
        && matches!(
            task.item.stage_phase,
            Some(
                StagePhase::Integrating
                    | StagePhase::AwaitingCi
                    | StagePhase::AwaitingMerge
                    | StagePhase::Merging
            )
        )
}

pub fn waiting(task: &Task) -> bool {
    group(task).is_some() && task.item.stage_phase == Some(StagePhase::Queued)
}

pub fn next_turns(tasks: &[Task]) -> Vec<WorkItemId> {
    let mut held: BTreeMap<(RepoId, &str), bool> = BTreeMap::new();
    let mut next: BTreeMap<(RepoId, &str), (Reverse<i64>, i64, WorkItemId)> = BTreeMap::new();
    for task in tasks {
        let Some(key) = group(task) else { continue };
        if holds(task) {
            held.insert(key, true);
        } else if waiting(task) {
            let at = task
                .stage_run
                .as_ref()
                .and_then(|r| r.queued_at)
                .map_or(0, |t| t.0);
            let entry = (Reverse(task.item.priority), at, task.item.id);
            let slot = next.entry(key).or_insert(entry);
            if entry < *slot {
                *slot = entry;
            }
        }
    }
    next.into_iter()
        .filter(|(key, _)| !held.contains_key(key))
        .map(|(_, (_, _, id))| id)
        .collect()
}
