use crate::admission::{self, Candidate, Waiting};
use crate::ids::{SpecId, WorkItemId};
use crate::job::Job;
use crate::model::{AccountQuota, Spec, SpecDependency, Task, WorkItemDependency};
use crate::react::phase_jobs;
use crate::task::{LoopConfig, TaskInput};
use crate::time::Timestamp;
use crate::vocab::{SpecState, WorkItemState};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Decide(WorkItemId, TaskInput),
    ShipSpec(SpecId),
    Enqueue(Job),
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub specs: Vec<Spec>,
    pub tasks: Vec<Task>,
    pub work_item_dependencies: Vec<WorkItemDependency>,
    pub spec_dependencies: Vec<SpecDependency>,
    pub quotas: Vec<AccountQuota>,
}

pub fn waiting(
    snapshot: &Snapshot,
    cfg: &LoopConfig,
    now: Timestamp,
) -> Vec<(WorkItemId, Vec<Waiting>)> {
    let quota = admission::quota_waits(&snapshot.quotas, &cfg.account, cfg.rate_limit_pause, now);
    snapshot
        .tasks
        .iter()
        .filter(|t| t.item.state == WorkItemState::Queued)
        .map(|t| {
            let mut waits = admission::waiting_on(
                t,
                &snapshot.tasks,
                &snapshot.specs,
                &snapshot.work_item_dependencies,
                &snapshot.spec_dependencies,
            );
            waits.extend(quota.iter().cloned());
            (t.item.id, waits)
        })
        .filter(|(_, waits)| !waits.is_empty())
        .collect()
}

pub fn reconcile(
    snapshot: &Snapshot,
    cfg: &LoopConfig,
    max_active_tasks: usize,
    now: Timestamp,
) -> Vec<Action> {
    let mut actions = Vec::new();
    let tasks = &snapshot.tasks;
    let of_spec = |spec: SpecId| tasks.iter().filter(move |t| t.item.spec_id == Some(spec));

    let active = tasks
        .iter()
        .filter(|t| {
            matches!(
                t.item.state,
                WorkItemState::Admitted | WorkItemState::InStage
            )
        })
        .count();
    let waits = waiting(snapshot, cfg, now);
    let candidates: Vec<Candidate> = tasks
        .iter()
        .filter(|t| t.item.state == WorkItemState::Queued)
        .filter_map(|t| {
            let spec_id = t.item.spec_id?;
            let spec = snapshot
                .specs
                .iter()
                .find(|s| s.id == spec_id && s.state == SpecState::Approved)?;
            Some(Candidate {
                work_item_id: t.item.id,
                priority: t.item.priority,
                spec_approved_at: spec.approved_at.unwrap_or(spec.created_at),
                step_index: t.item.step_index?,
                waiting_on: waits
                    .iter()
                    .find(|(id, _)| *id == t.item.id)
                    .map(|(_, w)| w.clone())
                    .unwrap_or_default(),
                spec_has_cancelled_task: of_spec(spec_id)
                    .any(|o| o.item.state == WorkItemState::Cancelled),
            })
        })
        .collect();
    for id in admission::select(&candidates, active, max_active_tasks) {
        actions.push(Action::Decide(id, TaskInput::Admit));
    }

    for task in tasks {
        actions.extend(phase_jobs(task, cfg, now).into_iter().map(Action::Enqueue));

        if crate::task::needs_tick(task, cfg, now) {
            actions.push(Action::Decide(task.item.id, TaskInput::Tick));
        }
    }

    for spec in snapshot
        .specs
        .iter()
        .filter(|s| s.state == SpecState::Approved)
    {
        if crate::spec::ships(of_spec(spec.id).map(|t| t.item.state)) {
            actions.push(Action::ShipSpec(spec.id));
        }
    }
    actions
}
