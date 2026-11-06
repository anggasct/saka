use std::cmp::Reverse;
use std::collections::BTreeMap;

use crate::ids::{RepoId, WorkItemId};
use crate::model::Task;
use crate::vocab::{Stage, StagePhase, WorkItemOrigin, WorkItemState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    pub base_branch: String,
    pub position: u32,
    pub size: u32,
}

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
        && !task.stage_run.as_ref().is_some_and(|r| r.queue_native)
}

pub fn waiting(task: &Task) -> bool {
    group(task).is_some() && task.item.stage_phase == Some(StagePhase::Queued)
}

fn order(task: &Task) -> (Reverse<i64>, i64, WorkItemId) {
    let at = task
        .stage_run
        .as_ref()
        .and_then(|r| r.queued_at)
        .map_or(0, |t| t.0);
    (Reverse(task.item.priority), at, task.item.id)
}

pub fn next_turns(tasks: &[Task]) -> Vec<WorkItemId> {
    let mut held: BTreeMap<(RepoId, &str), bool> = BTreeMap::new();
    let mut next: BTreeMap<(RepoId, &str), (Reverse<i64>, i64, WorkItemId)> = BTreeMap::new();
    for task in tasks {
        let Some(key) = group(task) else { continue };
        if holds(task) {
            held.insert(key, true);
        } else if waiting(task) {
            let entry = order(task);
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

pub fn position(tasks: &[Task], id: WorkItemId) -> Option<Position> {
    let me = tasks.iter().find(|t| t.item.id == id)?;
    let key = group(me)?;
    let peers = tasks.iter().filter(|t| group(t) == Some(key));
    let holders = u32::try_from(peers.clone().filter(|t| holds(t)).count()).ok()?;
    let mut line: Vec<_> = peers.filter(|t| waiting(t)).map(order).collect();
    line.sort();
    let ahead = if holds(me) {
        0
    } else {
        1 + u32::try_from(line.iter().position(|(_, _, other)| *other == id)?).ok()?
    };
    Some(Position {
        base_branch: key.1.to_string(),
        position: ahead,
        size: holders + u32::try_from(line.len()).ok()?,
    })
}
