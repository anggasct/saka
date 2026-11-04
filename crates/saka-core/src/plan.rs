use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ids::{SpecRevisionId, WorkItemId};
use crate::model::{RevisionView, StepPlan, WorkItem};
use crate::vocab::{ConflictReason, PlanAction, StepField, WorkItemState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub step_id: String,
    pub action: PlanAction,
    pub work_item_id: Option<WorkItemId>,
    pub state: Option<WorkItemState>,
    pub reason: Option<ConflictReason>,
    pub fields: Vec<StepField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionPlan {
    pub from_revision_id: SpecRevisionId,
    pub to_revision_id: SpecRevisionId,
    pub ready: bool,
    pub steps: Vec<PlanStep>,
}

impl RevisionPlan {
    pub fn conflicts(&self) -> Vec<&PlanStep> {
        self.steps
            .iter()
            .filter(|s| s.action == PlanAction::Conflict)
            .collect()
    }

    pub fn conflict_params(&self) -> Value {
        Value::Array(
            self.conflicts()
                .into_iter()
                .map(|s| {
                    json!({
                        "step_id": s.step_id,
                        "reason": s.reason,
                        "fields": s.fields,
                        "work_item_id": s.work_item_id,
                        "state": s.state,
                    })
                })
                .collect(),
        )
    }

    pub fn count(&self, action: PlanAction) -> usize {
        self.steps.iter().filter(|s| s.action == action).count()
    }
}

pub fn changed_fields(from: &StepPlan, to: &StepPlan) -> Vec<StepField> {
    let mut fields = Vec::new();
    if from.work != to.work {
        fields.push(StepField::Work);
    }
    if from.ac != to.ac {
        fields.push(StepField::Ac);
    }
    if from.commands != to.commands {
        fields.push(StepField::Commands);
    }
    if from.after != to.after {
        fields.push(StepField::After);
    }
    if from.repo != to.repo {
        fields.push(StepField::Repo);
    }
    fields
}

pub fn plan_revision(
    from: &RevisionView,
    to: &RevisionView,
    tasks: &[WorkItem],
    parser_version: u32,
) -> RevisionPlan {
    let task_of = |step_id: &str| {
        let mut mine = tasks
            .iter()
            .filter(|t| t.step_id.as_deref() == Some(step_id));
        let first = mine.next();
        mine.find(|t| t.state != WorkItemState::Superseded)
            .or(first)
    };
    let step = |action, task: Option<&WorkItem>, id: &str| PlanStep {
        step_id: id.to_string(),
        action,
        work_item_id: task.map(|t| t.id),
        state: task.map(|t| t.state),
        reason: None,
        fields: Vec::new(),
    };
    let mut steps = Vec::new();
    let mut ordered: Vec<&StepPlan> = to.steps.iter().collect();
    ordered.sort_by_key(|s| s.index);
    for next in ordered {
        let task = task_of(&next.id);
        let planned = match task {
            None => step(PlanAction::Add, None, &next.id),
            Some(t) if t.state == WorkItemState::Superseded => {
                step(PlanAction::Restore, task, &next.id)
            }
            Some(t) if !t.state.has_started() => step(PlanAction::Repin, task, &next.id),
            Some(_) => {
                let fields = match from.steps.iter().find(|s| s.id == next.id) {
                    Some(before) => changed_fields(before, next),
                    None => StepField::ALL.to_vec(),
                };
                if fields.is_empty() {
                    step(PlanAction::Keep, task, &next.id)
                } else {
                    PlanStep {
                        reason: Some(ConflictReason::Changed),
                        fields,
                        ..step(PlanAction::Conflict, task, &next.id)
                    }
                }
            }
        };
        steps.push(planned);
    }
    let mut removed: Vec<(u32, &WorkItem)> = tasks
        .iter()
        .filter(|t| t.state != WorkItemState::Superseded)
        .filter_map(|t| {
            let id = t.step_id.as_deref()?;
            if to.steps.iter().any(|s| s.id == id) {
                return None;
            }
            let index = from
                .steps
                .iter()
                .find(|s| s.id == id)
                .map_or(t.step_index.unwrap_or(u32::MAX), |s| s.index);
            Some((index, t))
        })
        .collect();
    removed.sort_by_key(|(index, t)| (*index, t.id));
    for (_, t) in removed {
        let id = t.step_id.clone().unwrap_or_default();
        if t.state.has_started() {
            steps.push(PlanStep {
                reason: Some(ConflictReason::Removed),
                ..step(PlanAction::Conflict, Some(t), &id)
            });
        } else {
            steps.push(step(PlanAction::Supersede, Some(t), &id));
        }
    }
    let ready = to.lint_ok
        && to.parser_version >= parser_version
        && !steps.iter().any(|s| s.action == PlanAction::Conflict);
    RevisionPlan {
        from_revision_id: from.id,
        to_revision_id: to.id,
        ready,
        steps,
    }
}
