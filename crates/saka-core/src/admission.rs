use serde::{Deserialize, Serialize};

use crate::ids::{SpecId, WorkItemId};
use crate::model::{Spec, SpecDependency, Task, WorkItemDependency};
use crate::time::Timestamp;
use crate::vocab::{SpecState, WorkItemState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Waiting {
    Step {
        step_id: String,
        work_item_id: WorkItemId,
    },
    Spec {
        slug: String,
        spec_id: SpecId,
    },
}

impl Waiting {
    pub fn kind(&self) -> &'static str {
        match self {
            Waiting::Step { .. } => "step",
            Waiting::Spec { .. } => "spec",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub work_item_id: WorkItemId,
    pub priority: i64,
    pub spec_approved_at: Timestamp,
    pub step_index: u32,
    pub waiting_on: Vec<Waiting>,
    pub spec_has_cancelled_task: bool,
}

pub fn select(
    candidates: &[Candidate],
    active_tasks: usize,
    max_active_tasks: usize,
) -> Vec<WorkItemId> {
    let mut eligible: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.waiting_on.is_empty() && !c.spec_has_cancelled_task)
        .collect();
    eligible.sort_by_key(|c| {
        (
            std::cmp::Reverse(c.priority),
            c.spec_approved_at,
            c.step_index,
            c.work_item_id,
        )
    });
    let free = max_active_tasks.saturating_sub(active_tasks);
    eligible
        .into_iter()
        .take(free)
        .map(|c| c.work_item_id)
        .collect()
}

pub fn waiting_on(
    task: &Task,
    tasks: &[Task],
    specs: &[Spec],
    work_item_dependencies: &[WorkItemDependency],
    spec_dependencies: &[SpecDependency],
) -> Vec<Waiting> {
    if task.item.state != WorkItemState::Queued {
        return Vec::new();
    }
    let mut steps: Vec<(u32, WorkItemId, String)> = work_item_dependencies
        .iter()
        .filter(|d| d.work_item_id == task.item.id)
        .filter_map(|d| {
            match tasks
                .iter()
                .find(|t| t.item.id == d.depends_on_work_item_id)
            {
                Some(t) if t.item.state == WorkItemState::Done => None,
                Some(t) => Some((
                    t.item.step_index.unwrap_or(u32::MAX),
                    t.item.id,
                    t.item.step_id.clone().unwrap_or_default(),
                )),
                None => Some((u32::MAX, d.depends_on_work_item_id, String::new())),
            }
        })
        .collect();
    steps.sort();
    let mut waiting_specs: Vec<(String, SpecId)> = spec_dependencies
        .iter()
        .filter(|d| Some(d.spec_id) == task.item.spec_id)
        .filter(|d| {
            specs
                .iter()
                .find(|s| s.id == d.depends_on_spec_id)
                .is_none_or(|s| s.state != SpecState::Shipped)
        })
        .map(|d| (d.depends_on_slug.clone(), d.depends_on_spec_id))
        .collect();
    waiting_specs.sort();
    steps
        .into_iter()
        .map(|(_, work_item_id, step_id)| Waiting::Step {
            step_id,
            work_item_id,
        })
        .chain(
            waiting_specs
                .into_iter()
                .map(|(slug, spec_id)| Waiting::Spec { slug, spec_id }),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use uuid::Uuid;

    use super::*;

    fn candidate(n: u128, priority: i64, approved: i64, step: u32) -> Candidate {
        Candidate {
            work_item_id: WorkItemId(Uuid::from_u128(n)),
            priority,
            spec_approved_at: Timestamp(approved),
            step_index: step,
            waiting_on: Vec::new(),
            spec_has_cancelled_task: false,
        }
    }

    fn wait_step(n: u128) -> Waiting {
        Waiting::Step {
            step_id: "S1".into(),
            work_item_id: WorkItemId(Uuid::from_u128(n)),
        }
    }

    #[test]
    fn priority_then_age_then_step_then_id_within_free_slots() {
        let old = candidate(1, 0, 100, 1);
        let new = candidate(2, 0, 200, 1);
        let urgent = candidate(3, 5, 300, 1);
        assert_eq!(
            select(&[old.clone(), new.clone(), urgent.clone()], 0, 2),
            vec![urgent.work_item_id, old.work_item_id]
        );
        assert_eq!(select(&[old.clone(), new.clone()], 2, 2), vec![]);

        let later_step = candidate(4, 0, 100, 2);
        let same_step_higher_id = candidate(5, 0, 100, 1);
        assert_eq!(
            select(
                &[later_step.clone(), same_step_higher_id.clone(), old.clone()],
                0,
                3
            ),
            vec![
                old.work_item_id,
                same_step_higher_id.work_item_id,
                later_step.work_item_id
            ]
        );
    }

    #[test]
    fn parallel_steps_of_one_spec_start_together() {
        let s2 = candidate(2, 0, 100, 2);
        let s3 = candidate(3, 0, 100, 3);
        assert_eq!(
            select(&[s2.clone(), s3.clone()], 0, 2),
            vec![s2.work_item_id, s3.work_item_id]
        );
    }

    #[test]
    fn waiting_or_cancelled_candidates_stay_queued() {
        let mut step = candidate(1, 9, 100, 2);
        step.waiting_on = vec![wait_step(9)];
        let mut spec = candidate(2, 9, 100, 1);
        spec.waiting_on = vec![Waiting::Spec {
            slug: "auth".into(),
            spec_id: SpecId(Uuid::from_u128(7)),
        }];
        let mut stopped = candidate(3, 9, 100, 2);
        stopped.spec_has_cancelled_task = true;
        assert_eq!(select(&[step, spec, stopped], 0, 4), vec![]);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn admits_only_ready_candidates_in_order_within_slots(
            raw in prop::collection::vec((-2i64..3, 0i64..5, 1u32..4, any::<bool>(), any::<bool>(), any::<bool>()), 0..12),
            active in 0usize..4,
            max in 0usize..5,
        ) {
            let cs: Vec<Candidate> = raw.iter().enumerate().map(|(i, (p, at, step, step_wait, spec_wait, cancelled))| {
                let mut c = candidate(i as u128 + 100, *p, *at, *step);
                if *step_wait {
                    c.waiting_on.push(wait_step(1));
                }
                if *spec_wait {
                    c.waiting_on.push(Waiting::Spec { slug: "x".into(), spec_id: SpecId(Uuid::from_u128(2)) });
                }
                c.spec_has_cancelled_task = *cancelled;
                c
            }).collect();
            let picked = select(&cs, active, max);
            let ready: Vec<&Candidate> = cs.iter().filter(|c| c.waiting_on.is_empty() && !c.spec_has_cancelled_task).collect();
            prop_assert!(picked.len() <= max.saturating_sub(active));
            prop_assert_eq!(picked.len(), ready.len().min(max.saturating_sub(active)));
            let key = |c: &Candidate| (std::cmp::Reverse(c.priority), c.spec_approved_at, c.step_index, c.work_item_id);
            let chosen: Vec<&Candidate> = picked.iter().map(|id| cs.iter().find(|c| c.work_item_id == *id).unwrap()).collect();
            for c in &chosen {
                prop_assert!(c.waiting_on.is_empty() && !c.spec_has_cancelled_task);
            }
            for pair in chosen.windows(2) {
                prop_assert!(key(pair[0]) < key(pair[1]));
            }
            if let Some(last) = chosen.last() {
                for c in ready.iter().filter(|c| !picked.contains(&c.work_item_id)) {
                    prop_assert!(key(last) < key(c));
                }
            }
        }
    }
}
