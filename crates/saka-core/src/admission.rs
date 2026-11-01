//! Which queued tasks start next.

use crate::ids::{SpecId, WorkItemId};
use crate::time::Timestamp;

/// A queued task whose spec is approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub work_item_id: WorkItemId,
    pub spec_id: SpecId,
    pub priority: i64,
    pub spec_approved_at: Timestamp,
    pub step_index: u32,
    /// The previous step of the same spec is done (always true for the first step).
    pub previous_step_done: bool,
    /// Another task of the same spec is admitted, in a stage, or blocked.
    pub spec_has_active_task: bool,
    /// A task of the same spec was cancelled; the rest of the spec never starts.
    pub spec_has_cancelled_task: bool,
}

/// Picks tasks to admit: eligible candidates ordered by priority (highest first), then
/// spec approval time, then step, then id, at most one per spec, within the free slots.
pub fn select(
    candidates: &[Candidate],
    active_tasks: usize,
    max_active_tasks: usize,
) -> Vec<WorkItemId> {
    let mut eligible: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.previous_step_done && !c.spec_has_active_task && !c.spec_has_cancelled_task)
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
    let mut picked: Vec<&Candidate> = Vec::new();
    for c in eligible {
        if picked.len() == free {
            break;
        }
        if picked.iter().all(|p| p.spec_id != c.spec_id) {
            picked.push(c);
        }
    }
    picked.into_iter().map(|c| c.work_item_id).collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use uuid::Uuid;

    use super::*;

    fn candidate(n: u128, spec: u128, priority: i64, approved: i64, step: u32) -> Candidate {
        Candidate {
            work_item_id: WorkItemId(Uuid::from_u128(n)),
            spec_id: SpecId(Uuid::from_u128(spec)),
            priority,
            spec_approved_at: Timestamp(approved),
            step_index: step,
            previous_step_done: true,
            spec_has_active_task: false,
            spec_has_cancelled_task: false,
        }
    }

    #[test]
    fn priority_then_age_within_free_slots() {
        let old = candidate(1, 10, 0, 100, 1);
        let new = candidate(2, 20, 0, 200, 1);
        let urgent = candidate(3, 30, 5, 300, 1);
        assert_eq!(
            select(&[old.clone(), new.clone(), urgent.clone()], 0, 2),
            vec![urgent.work_item_id, old.work_item_id]
        );
        assert_eq!(select(&[old.clone(), new], 2, 2), vec![]);
    }

    #[test]
    fn steps_wait_for_their_predecessor_and_spec() {
        let mut waiting = candidate(1, 10, 0, 100, 2);
        waiting.previous_step_done = false;
        let mut busy = candidate(2, 20, 0, 100, 1);
        busy.spec_has_active_task = true;
        let mut stopped = candidate(3, 30, 0, 100, 2);
        stopped.spec_has_cancelled_task = true;
        assert_eq!(select(&[waiting, busy, stopped], 0, 4), vec![]);
    }

    proptest! {
        #[test]
        fn never_exceeds_slots_or_admits_two_per_spec(
            raw in prop::collection::vec((0u128..4, -2i64..3, 0i64..5, 1u32..4, any::<bool>(), any::<bool>()), 0..12),
            active in 0usize..4,
            max in 0usize..5,
        ) {
            let cs: Vec<Candidate> = raw.iter().enumerate().map(|(i, (spec, p, at, step, prev, busy))| {
                let mut c = candidate(i as u128 + 100, *spec, *p, *at, *step);
                c.previous_step_done = *prev;
                c.spec_has_active_task = *busy;
                c
            }).collect();
            let picked = select(&cs, active, max);
            prop_assert!(picked.len() <= max.saturating_sub(active));
            let specs: Vec<SpecId> = picked.iter().map(|id| cs.iter().find(|c| c.work_item_id == *id).unwrap().spec_id).collect();
            let mut unique = specs.clone();
            unique.sort();
            unique.dedup();
            prop_assert_eq!(unique.len(), specs.len());
            for id in &picked {
                let c = cs.iter().find(|c| c.work_item_id == *id).unwrap();
                prop_assert!(c.previous_step_done && !c.spec_has_active_task);
            }
        }
    }
}
