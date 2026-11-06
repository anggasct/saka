#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::autonomy::Ceilings;
use saka_core::vocab::{Autonomy, ReloadOutcome, Stage, StagePhase, Trigger, WorkItemState};
use saka_testkit::{Layout, SHOP_REPO, Sim, SimConfig, repo_entry, saka_toml};

fn layout(extra: &str) -> Layout {
    Layout {
        saka_toml: saka_toml("shop", &[repo_entry("shop", SHOP_REPO, extra)]),
        ..Layout::default()
    }
}

fn ceiling(level: Autonomy) -> SimConfig {
    let mut cfg = SimConfig::default();
    cfg.loop_cfg.autonomy = Ceilings {
        default: level,
        repos: Vec::new(),
    };
    cfg
}

fn requested(sim: &Sim) -> Option<Autonomy> {
    sim.project().project.repos[0].autonomy
}

#[test]
fn a_request_in_saka_toml_is_stored_per_repo_and_follows_every_reload() {
    let sim = Sim::multi_repo(
        ceiling(Autonomy::Autonomous),
        &layout("autonomy = \"strict\"\n"),
    );
    assert_eq!(requested(&sim), Some(Autonomy::Strict));

    sim.world.put_doc(
        "docs/saka.toml",
        layout("autonomy = \"standard\"\n").saka_toml.as_bytes(),
    );
    assert_eq!(sim.reload().unwrap().outcome, ReloadOutcome::Changed);
    assert_eq!(requested(&sim), Some(Autonomy::Standard));

    sim.world
        .put_doc("docs/saka.toml", layout("").saka_toml.as_bytes());
    assert_eq!(sim.reload().unwrap().outcome, ReloadOutcome::Changed);
    assert_eq!(requested(&sim), None);
}

#[test]
fn a_task_runs_at_the_lowest_of_the_ceiling_and_the_request() {
    for (ceiling_level, asks, expected) in [
        (Autonomy::Standard, Some("autonomous"), Autonomy::Standard),
        (Autonomy::Autonomous, Some("strict"), Autonomy::Strict),
        (Autonomy::Autonomous, None, Autonomy::Autonomous),
        (Autonomy::Strict, Some("autonomous"), Autonomy::Strict),
    ] {
        let extra = asks.map_or(String::new(), |a| format!("autonomy = \"{a}\"\n"));
        let mut sim = Sim::multi_repo(ceiling(ceiling_level), &layout(&extra));
        sim.approve();
        let cfg = &sim.cfg.loop_cfg.autonomy;
        assert_eq!(
            sim.task(1).autonomy(cfg),
            expected,
            "{ceiling_level} asks {asks:?}"
        );
    }
}

const STEPS: usize = 12_000;

fn start(sim: &Sim, step: u32) {
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let task = sim.task(step);
    engine
        .start_step_with(task.item.id, task.item.version, &cause, None)
        .unwrap();
}

#[test]
fn a_strict_repo_waits_for_a_start_before_each_step_and_a_standard_one_never_does() {
    let mut cfg = ceiling(Autonomy::Strict);
    cfg.operator.start = false;
    let mut sim = Sim::new(cfg);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).item.state == WorkItemState::AwaitingStart
    }));
    assert_eq!(sim.task(2).item.state, WorkItemState::Queued);
    for _ in 0..200 {
        sim.step();
    }
    assert_eq!(
        sim.task(1).item.state,
        WorkItemState::AwaitingStart,
        "nothing starts the step but the operator"
    );
    assert!(sim.events_of("runtime.session.started").is_empty());

    start(&sim, 1);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).item.state == WorkItemState::Done
            && s.task(2).item.state == WorkItemState::AwaitingStart
    }));
    start(&sim, 2);
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.events_of("delivery.work_item.awaiting_start").len(), 2);
    assert_eq!(sim.events_of("delivery.work_item.start_approved").len(), 2);

    let mut standard = Sim::new(ceiling(Autonomy::Standard));
    standard.approve();
    assert!(standard.run_to_shipped(STEPS));
    assert!(
        standard
            .events_of("delivery.work_item.awaiting_start")
            .is_empty()
    );
}

#[test]
fn lowering_the_ceiling_does_not_release_a_step_that_already_waits() {
    let mut cfg = ceiling(Autonomy::Strict);
    cfg.operator.start = false;
    let mut sim = Sim::new(cfg);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).item.state == WorkItemState::AwaitingStart
    }));
    sim.cfg.loop_cfg.autonomy.default = Autonomy::Standard;
    sim.restart();
    for _ in 0..200 {
        sim.step();
    }
    assert_eq!(sim.task(1).item.state, WorkItemState::AwaitingStart);
    start(&sim, 1);
    assert!(sim.run_to_shipped(STEPS));
}

fn autonomous() -> SimConfig {
    let mut cfg = ceiling(Autonomy::Autonomous);
    cfg.operator.merge = false;
    cfg
}

fn merge_reasons(sim: &Sim) -> Vec<String> {
    sim.events_of("delivery.merge.requested")
        .iter()
        .filter_map(|e| e.payload["reason"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn an_autonomous_repo_ships_a_spec_with_no_touch_after_the_approval() {
    let mut sim = Sim::new(autonomous());
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.stats().merges, 2);
    let reasons = merge_reasons(&sim);
    assert_eq!(
        reasons.iter().filter(|r| *r == "autonomy").count(),
        2,
        "{reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .all(|r| r == "autonomy" || r == "merge queue"),
        "the operator never tapped: {reasons:?}"
    );
    assert!(
        sim.events_of("delivery.work_item.awaiting_start")
            .is_empty()
    );
}

#[test]
fn a_standard_repo_and_a_manual_merge_trigger_still_wait_for_the_tap() {
    let mut standard = ceiling(Autonomy::Standard);
    standard.operator.merge = false;
    let mut manual = autonomous();
    manual.workflow.set_stage(Stage::Merge, Trigger::Manual);
    for cfg in [standard, manual] {
        let mut sim = Sim::new(cfg);
        sim.approve();
        assert!(sim.run_until(STEPS, |s| {
            s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
        }));
        for _ in 0..300 {
            sim.step();
        }
        assert!(sim.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge));
        assert_eq!(sim.stats().merges, 0);
        assert!(merge_reasons(&sim).is_empty());
    }
}

#[test]
fn an_autonomous_repo_without_a_reviewer_never_merges_alone() {
    let mut cfg = autonomous();
    cfg.workflow = SimConfig::without_review().workflow;
    cfg.workflow.set_stage(Stage::Merge, Trigger::Auto);
    let mut sim = Sim::new(cfg);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    for _ in 0..300 {
        sim.step();
    }
    assert!(sim.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge));
    assert_eq!(sim.stats().merges, 0);
}

#[test]
fn a_head_pushed_after_the_merge_was_asked_for_stops_it_before_the_provider_is_called() {
    let mut sim = Sim::new(autonomous());
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::Merging)
    }));
    let number = sim.task(1).change_request.unwrap().number;
    sim.world.push_to_change("acme/tasks", number);

    assert!(sim.run_to_shipped(STEPS));
    let dequeued = sim.events_of("delivery.merge.dequeued");
    assert_eq!(dequeued[0].payload["reason"], "head_moved");
    assert!(sim.events_of("delivery.work_item.blocked").is_empty());
    assert_eq!(
        sim.world.merge_calls().len(),
        2,
        "one merge per step, none at the head that was left behind"
    );
    let reviews = sim.events_of("delivery.review.recorded");
    assert!(
        reviews.len() >= 3,
        "the new head was reviewed again: {}",
        reviews.len()
    );
}
