use crate::admission::{self, Candidate, Waiting};
use crate::authorship::{ProjectAuthorship, UserLink, resume_trigger};
use crate::ids::{SpecId, WorkItemId};
use crate::job::Job;
use crate::model::{AccountQuota, Spec, SpecDependency, Task, WorkItemDependency};
use crate::react::phase_jobs;
use crate::task::{LoopConfig, TaskInput};
use crate::time::Timestamp;
use crate::vocab::{EnvironmentState, SpecState, WorkItemState};

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
    pub port_blocks: Option<usize>,
    pub user_links: Vec<UserLink>,
    pub authorships: Vec<ProjectAuthorship>,
}

fn holds_port_block(task: &Task) -> bool {
    let live_env = task.environment.as_ref().is_some_and(|e| {
        matches!(
            e.state,
            EnvironmentState::Ready | EnvironmentState::TeardownFailed
        )
    });
    live_env || task.item.state == WorkItemState::Admitted
}

pub fn free_port_blocks(snapshot: &Snapshot) -> Option<usize> {
    let held = snapshot
        .tasks
        .iter()
        .filter(|t| holds_port_block(t))
        .count();
    snapshot.port_blocks.map(|total| total.saturating_sub(held))
}

pub fn waiting(
    snapshot: &Snapshot,
    cfg: &LoopConfig,
    now: Timestamp,
) -> Vec<(WorkItemId, Vec<Waiting>)> {
    let mut quota =
        admission::quota_waits(&snapshot.quotas, &cfg.account, cfg.rate_limit_pause, now);
    if free_port_blocks(snapshot) == Some(0) {
        quota.push(Waiting::PortBlock);
    }
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

fn credential_resume(
    snapshot: &Snapshot,
    task: &Task,
    now: Timestamp,
) -> Option<crate::vocab::ResumeTrigger> {
    if task.item.state != WorkItemState::Blocked {
        return None;
    }
    let authorship = snapshot
        .authorships
        .iter()
        .find(|a| a.project_id == task.item.project_id);
    resume_trigger(task.open_blocker()?, &snapshot.user_links, authorship, now)
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
    let slots = max_active_tasks
        .saturating_sub(active)
        .min(free_port_blocks(snapshot).unwrap_or(usize::MAX));
    for id in admission::select(&candidates, 0, slots) {
        actions.push(Action::Decide(id, TaskInput::Admit));
    }

    for task in tasks {
        actions.extend(phase_jobs(task, cfg, now).into_iter().map(Action::Enqueue));

        if crate::task::needs_tick(task, cfg, now) {
            actions.push(Action::Decide(task.item.id, TaskInput::Tick));
        }
        if crate::task::auto_retry_due(task, now) {
            actions.push(Action::Decide(task.item.id, TaskInput::AutoRetry));
        }
        if let Some(trigger) = credential_resume(snapshot, task, now) {
            actions.push(Action::Decide(task.item.id, TaskInput::Resume { trigger }));
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
