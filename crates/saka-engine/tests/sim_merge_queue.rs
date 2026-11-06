#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::ids::SpecId;
use saka_core::queue;
use saka_core::vocab::{BlockerCode, SpecState, Stage, StagePhase, WorkItemOrigin, WorkItemState};
use saka_ports::ScmCapabilities;
use saka_testkit::{FAKE_CAPABILITIES, Operator, Sim, SimConfig, one_step_spec};

const STEPS: usize = 12_000;
const FIRST: &str = "docs/specs/first.md";
const SECOND: &str = "docs/specs/second.md";

fn edit(path: &str, text: &str) -> Vec<(String, Option<String>)> {
    vec![(path.to_string(), Some(text.to_string()))]
}

struct Pair {
    sim: Sim,
    specs: [SpecId; 2],
}

impl Pair {
    fn new(cfg: SimConfig, edits: Vec<Vec<(String, Option<String>)>>) -> Pair {
        let sim = Sim::new(cfg);
        sim.world.script_edits(edits);
        let first = sim.register(FIRST, &one_step_spec("First", ""));
        let second = sim.register(SECOND, &one_step_spec("Second", ""));
        sim.approve_spec(first.spec_id, first.revision_id).unwrap();
        sim.approve_spec(second.spec_id, second.revision_id)
            .unwrap();
        Pair {
            sim,
            specs: [first.spec_id, second.spec_id],
        }
    }

    fn run(&mut self, mut each: impl FnMut(&Sim)) -> bool {
        let specs = self.specs;
        self.sim.run_until(STEPS, |s| {
            each(s);
            specs
                .iter()
                .all(|id| s.spec_of(*id).state == SpecState::Shipped)
        })
    }
}

#[test]
fn parallel_tasks_merge_one_at_a_time_and_the_second_is_rebased_on_the_first() {
    let mut pair = Pair::new(
        SimConfig::default(),
        vec![edit("a.txt", "a\n"), edit("b.txt", "b\n")],
    );
    let mut most_holders = 0;
    let mut waited = false;
    assert!(pair.run(|s| {
        let tasks = s.all_tasks();
        most_holders = most_holders.max(tasks.iter().filter(|t| queue::holds(t)).count());
        waited |= tasks
            .iter()
            .any(|t| t.is_at(Stage::Merge, StagePhase::Queued));
    }));
    assert!(most_holders <= 1);
    assert!(waited, "the second request waits for its turn");

    let sim = &pair.sim;
    let rebased = sim.events_of("delivery.merge.rebased");
    assert_eq!(rebased.len(), 1);
    let payload = &rebased[0].payload;
    let head = payload["head_sha"].as_str().unwrap();
    let base = payload["base_sha"].as_str().unwrap();
    assert_ne!(payload["from_sha"], payload["head_sha"]);
    let prs = sim.world.prs();
    assert!(
        prs.iter()
            .any(|p| p.merge_commit_sha.as_deref() == Some(base)),
        "the rebase sits on the first merge"
    );
    assert!(sim.world.is_ancestor(base, head));
    assert!(prs.iter().any(|p| p.merged_head.as_deref() == Some(head)));

    let stats = sim.stats();
    assert_eq!((stats.merges, stats.pushes), (2, 3));
    let gated: Vec<_> = sim
        .world
        .recipe_runs()
        .into_iter()
        .filter(|r| r.phase == "gate")
        .collect();
    assert!(!gated.is_empty());
    assert_eq!(sim.events_of("delivery.merge.dequeued").len(), 0);
}

#[test]
fn a_single_task_on_an_unmoved_base_merges_without_a_rebase_or_a_push() {
    let mut sim = Sim::new(SimConfig::default());
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.events_of("delivery.merge.rebased").len(), 0);
    assert_eq!(sim.events_of("delivery.merge.requested").len(), 2);
    let stats = sim.stats();
    assert_eq!((stats.merges, stats.pushes), (2, 2));
    assert!(
        sim.world
            .rebases()
            .iter()
            .all(|(_, head, base)| head != base)
    );
}

#[test]
fn a_conflict_in_a_workflow_without_fix_goes_back_to_the_agent_with_the_paths() {
    let mut pair = Pair::new(
        SimConfig {
            operator: Operator {
                start: true,
                retry: vec![BlockerCode::MergeConflict],
                ..Operator::default()
            },
            ..SimConfig::without_fix()
        },
        vec![
            edit("shared.txt", "one\n"),
            edit("shared.txt", "two\n"),
            edit("shared.txt", "resolved\n"),
        ],
    );
    let mut blocked = None;
    assert!(pair.run(|s| {
        for t in s.all_tasks() {
            if let Some(b) = t
                .open_blocker()
                .filter(|b| b.code == BlockerCode::MergeConflict)
            {
                blocked.get_or_insert_with(|| b.params.clone());
            }
        }
    }));
    let params = blocked.expect("the second task conflicts with the first");
    assert_eq!(params["reason"], "rebase_conflict");
    assert_eq!(params["paths"], serde_json::json!(["shared.txt"]));
    assert_eq!(params["base_branch"], "main");
    assert!(
        pair.sim
            .world
            .prompts()
            .iter()
            .any(|p| p.contains("conflicts with `origin/main` in: shared.txt")),
        "the agent is told which paths conflict"
    );
    assert_eq!(pair.sim.stats().merges, 2);
}

#[test]
fn a_conflict_between_two_tasks_is_fixed_by_a_round_that_carries_the_paths() {
    let mut pair = Pair::new(
        SimConfig::default(),
        vec![
            edit("shared.txt", "one\n"),
            edit("shared.txt", "two\n"),
            edit("shared.txt", "resolved\n"),
        ],
    );
    let mut blocked = false;
    assert!(pair.run(|s| {
        blocked |= s.all_tasks().iter().any(|t| {
            t.open_blocker()
                .is_some_and(|b| b.code == BlockerCode::MergeConflict)
        });
    }));
    assert!(
        !blocked,
        "the fix round takes the conflict before anyone is asked"
    );
    let rounds: Vec<_> = pair
        .specs
        .iter()
        .flat_map(|id| pair.sim.tasks_of(*id))
        .map(|t| t.fix_rounds_used())
        .filter(|n| *n > 0)
        .collect();
    assert_eq!(rounds, [1], "only the second task conflicts");
    assert!(
        pair.sim
            .world
            .prompts()
            .iter()
            .any(|p| p.contains("conflicts with `origin/main` in: shared.txt")),
        "the agent is told which paths conflict"
    );
    assert_eq!(pair.sim.stats().merges, 2);
    assert_eq!(pair.sim.events_of("delivery.merge.dequeued").len(), 1);
}

#[test]
fn a_head_pushed_from_outside_while_queued_drops_the_request_without_blocking() {
    let mut pair = Pair::new(
        SimConfig::default(),
        vec![edit("a.txt", "a\n"), edit("b.txt", "b\n")],
    );
    let specs = pair.specs;
    let waiting = |s: &Sim| {
        let busy = s.all_tasks().iter().any(queue::holds);
        specs.iter().find_map(|id| {
            let t = s.tasks_of(*id).into_iter().next().unwrap();
            (busy && t.is_at(Stage::Merge, StagePhase::Queued)).then_some(t)
        })
    };
    assert!(pair.sim.run_until(STEPS, |s| waiting(s).is_some()));
    let t = waiting(&pair.sim).unwrap();
    let cr = t.change_request.clone().unwrap();
    pair.sim.world.push_to_change("acme/tasks", cr.number);

    assert!(pair.run(|_| {}));
    let dequeued = pair.sim.events_of("delivery.merge.dequeued");
    assert_eq!(dequeued.len(), 1);
    assert_eq!(dequeued[0].payload["reason"], "head_moved");
    assert!(
        pair.sim
            .events_of("delivery.work_item.blocked")
            .iter()
            .all(|e| e.payload["code"] != "branch_moved")
    );
}

#[test]
fn a_crash_in_the_middle_of_a_rebase_converges_on_the_same_head() {
    let mut pair = Pair::new(
        SimConfig::default(),
        vec![edit("a.txt", "a\n"), edit("b.txt", "b\n")],
    );
    let mut crashed = false;
    assert!(pair.sim.run_until(STEPS, |s| {
        s.all_tasks()
            .iter()
            .any(|t| t.is_at(Stage::Merge, StagePhase::Integrating))
            && s.stats().merges == 1
    }));
    for _ in 0..20 {
        let before = pair.sim.world.rebases().len();
        assert!(pair.sim.crash_mid_job());
        if pair.sim.world.rebases().len() > before {
            crashed = true;
            break;
        }
    }
    assert!(crashed, "a rebase ran before the crash");
    assert!(pair.run(|_| {}));
    assert!(pair.sim.events_of("delivery.merge.rebased").len() <= 1);
    assert_eq!(pair.sim.stats().pushes, 3);
    assert!(pair.sim.events_of("delivery.work_item.blocked").is_empty());
    assert!(
        pair.sim
            .all_tasks()
            .iter()
            .all(|t| t.item.origin == WorkItemOrigin::SpecStep
                && t.item.state == WorkItemState::Done)
    );
}

fn native(pair: &Pair) {
    pair.sim.world.set_capabilities(ScmCapabilities {
        merge_queue: true,
        ..FAKE_CAPABILITIES
    });
    pair.sim.world.set_native_queue(true);
}

fn native_pair() -> Pair {
    let pair = Pair::new(
        SimConfig::default(),
        vec![edit("a.txt", "a\n"), edit("b.txt", "b\n")],
    );
    native(&pair);
    pair
}

fn both_native(s: &Sim) -> bool {
    let tasks = s.all_tasks();
    tasks.len() == 2
        && tasks.iter().all(|t| {
            t.is_at(Stage::Merge, StagePhase::Merging)
                && t.stage_run.as_ref().is_some_and(|r| r.queue_native)
        })
}

#[test]
fn a_provider_with_a_queue_gets_the_change_without_a_rebase_and_without_taking_saka_s_turn() {
    let mut pair = native_pair();
    pair.sim.world.hold_native_queue(true);
    assert!(pair.sim.run_until(STEPS, both_native));
    let sim = &pair.sim;
    assert_eq!(sim.world.enqueue_calls().len(), 2);
    assert!(sim.world.rebases().is_empty());
    assert!(sim.world.merge_calls().is_empty());
    assert!(sim.all_tasks().iter().all(|t| !queue::holds(t)));
    assert_eq!(sim.stats().pushes, 2);
    assert_eq!(sim.events_of("delivery.merge.enqueued").len(), 2);

    pair.sim.world.hold_native_queue(false);
    assert!(pair.run(|_| {}));
    assert_eq!(pair.sim.stats().merges, 2);
    assert!(pair.sim.world.merge_calls().is_empty());
}

#[test]
fn without_the_capability_or_a_queue_on_the_branch_saka_keeps_its_own_queue() {
    let mut pair = Pair::new(
        SimConfig::default(),
        vec![edit("a.txt", "a\n"), edit("b.txt", "b\n")],
    );
    pair.sim.world.set_native_queue(true);
    assert!(pair.run(|_| {}));
    assert!(pair.sim.world.enqueue_calls().is_empty());
    assert_eq!(pair.sim.world.rebases().len(), 3);

    let mut pair = native_pair();
    pair.sim.world.set_native_queue(false);
    assert!(pair.run(|_| {}));
    assert!(pair.sim.world.enqueue_calls().is_empty());
    assert_eq!(pair.sim.world.rebases().len(), 3);
}

#[test]
fn a_change_the_provider_drops_from_its_queue_blocks_instead_of_waiting_forever() {
    let mut pair = native_pair();
    pair.sim.world.hold_native_queue(true);
    assert!(pair.sim.run_until(STEPS, both_native));
    let number = pair.sim.all_tasks()[0]
        .change_request
        .as_ref()
        .unwrap()
        .number;
    pair.sim.world.eject_from_queue(number);
    assert!(pair.sim.run_until(STEPS, |s| {
        s.all_tasks().iter().any(|t| {
            t.open_blocker()
                .is_some_and(|b| b.code == BlockerCode::MergeConflict)
        })
    }));
    let blocked = pair
        .sim
        .all_tasks()
        .into_iter()
        .find(|t| t.open_blocker().is_some())
        .unwrap();
    let params = blocked.open_blocker().unwrap().params.clone();
    assert_eq!(params["reason"], "removed_from_queue");
    assert_eq!(blocked.change_request.as_ref().unwrap().number, number);
}

#[test]
fn a_refused_enqueue_blocks_with_the_providers_reason() {
    let mut pair = native_pair();
    pair.sim.world.refuse_enqueues(vec!["not_mergeable"]);
    assert!(pair.sim.run_until(STEPS, |s| {
        s.all_tasks().iter().any(|t| {
            t.open_blocker()
                .is_some_and(|b| b.code == BlockerCode::MergeConflict)
        })
    }));
    let blocked = pair
        .sim
        .all_tasks()
        .into_iter()
        .find(|t| t.open_blocker().is_some())
        .unwrap();
    assert_eq!(
        blocked.open_blocker().unwrap().params["reason"],
        "not_mergeable"
    );
}

#[test]
fn a_rebased_head_is_reviewed_before_it_merges_and_the_old_verdict_does_not_carry_over() {
    let mut pair = Pair::new(
        SimConfig::default(),
        vec![edit("a.txt", "a\n"), edit("b.txt", "b\n")],
    );
    assert!(pair.run(|_| {}));
    let sim = &pair.sim;
    let rebased = sim.events_of("delivery.merge.rebased");
    assert_eq!(rebased.len(), 1);
    let head = rebased[0].payload["head_sha"].as_str().unwrap();
    let reviewed: Vec<_> = sim
        .events_of("delivery.review.recorded")
        .into_iter()
        .filter(|e| e.payload["head_sha"] == head)
        .collect();
    assert_eq!(reviewed.len(), 1, "the rebased head has its own verdict");
    assert_eq!(sim.events_of("delivery.review.recorded").len(), 3);
}
