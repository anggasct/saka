//! From the projections, derive the work that should exist. Pure and safe to run as often
//! as wanted: commands are re-checked by the decisions and jobs carry fixed keys.

use crate::admission::{self, Candidate};
use crate::ids::{SpecId, WorkItemId};
use crate::job::Job;
use crate::model::{Spec, Task};
use crate::react::phase_jobs;
use crate::task::{LoopConfig, TaskInput};
use crate::time::Timestamp;
use crate::vocab::{SessionState, SpecState, Stage, StagePhase, WorkItemState};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Decide(WorkItemId, TaskInput),
    ShipSpec(SpecId),
    Enqueue(Job),
}

/// Specs that are approved and every non-terminal task of those specs.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub specs: Vec<Spec>,
    pub tasks: Vec<Task>,
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

    // Admission.
    let active = tasks
        .iter()
        .filter(|t| {
            matches!(
                t.item.state,
                WorkItemState::Admitted | WorkItemState::InStage
            )
        })
        .count();
    let candidates: Vec<Candidate> = tasks
        .iter()
        .filter(|t| t.item.state == WorkItemState::Queued)
        .filter_map(|t| {
            let spec_id = t.item.spec_id?;
            let spec = snapshot
                .specs
                .iter()
                .find(|s| s.id == spec_id && s.state == SpecState::Approved)?;
            let index = t.item.step_index?;
            Some(Candidate {
                work_item_id: t.item.id,
                spec_id,
                priority: t.item.priority,
                spec_approved_at: spec.approved_at.unwrap_or(spec.created_at),
                step_index: index,
                previous_step_done: index == 1
                    || of_spec(spec_id).any(|o| {
                        o.item.step_index == Some(index - 1) && o.item.state == WorkItemState::Done
                    }),
                spec_has_active_task: of_spec(spec_id).any(|o| {
                    matches!(
                        o.item.state,
                        WorkItemState::Admitted | WorkItemState::InStage | WorkItemState::Blocked
                    )
                }),
                spec_has_cancelled_task: of_spec(spec_id)
                    .any(|o| o.item.state == WorkItemState::Cancelled),
            })
        })
        .collect();
    for id in admission::select(&candidates, active, max_active_tasks) {
        actions.push(Action::Decide(id, TaskInput::Admit));
    }

    for task in tasks {
        // Jobs the current place needs.
        actions.extend(phase_jobs(task, cfg, now).into_iter().map(Action::Enqueue));

        // Timers.
        let over_time = task.live_session().is_some_and(|s| {
            matches!(
                s.state,
                SessionState::Running | SessionState::Waiting | SessionState::Idle
            ) && s.running_ms_at(now) > cfg.max_running_ms
        });
        let no_checks = task.is_at(Stage::Merge, StagePhase::AwaitingCi)
            && task.gate.as_ref().is_none_or(|g| g.checks.is_empty())
            && task
                .stage_run
                .as_ref()
                .and_then(|r| r.started_at)
                .is_some_and(|at| now.since(at) > cfg.ci_appear_timeout_ms);
        if over_time || no_checks {
            actions.push(Action::Decide(task.item.id, TaskInput::Tick));
        }
    }

    // Shipping.
    for spec in snapshot
        .specs
        .iter()
        .filter(|s| s.state == SpecState::Approved)
    {
        let mut all = of_spec(spec.id).peekable();
        if all.peek().is_some() && of_spec(spec.id).all(|t| t.item.state == WorkItemState::Done) {
            actions.push(Action::ShipSpec(spec.id));
        }
    }
    actions
}
