//! The merge gate in the simulation: merge only at a head whose CI passed, red CI and
//! missing CI block, and a change merged or closed on the host is followed.

#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::rejection::Rejection;
use saka_core::vocab::{BlockerCode, ChangeState, GateOutcome, Stage, StagePhase, WorkItemState};
use saka_engine::EngineError;
use saka_testkit::{CiScript, Sim, SimConfig};
use serde_json::json;

const STEPS: usize = 5_000;

fn sim(ci: CiScript) -> Sim {
    let mut cfg = SimConfig {
        ci,
        ..SimConfig::default()
    };
    cfg.operator.merge = false;
    let mut sim = Sim::new(cfg);
    sim.approve();
    sim
}

fn blocked(sim: &Sim) -> bool {
    sim.task(1).item.state == WorkItemState::Blocked
}

fn request_merge(sim: &Sim) -> Result<(), EngineError> {
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let task = sim.task(1);
    engine
        .request_merge(task.item.id, task.item.version, None, &cause)
        .map(|_| ())
}

#[test]
fn merge_is_refused_until_the_gate_passes_at_the_current_head() {
    let mut sim = sim(CiScript::Silent);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingCi)
    }));
    assert!(matches!(
        request_merge(&sim),
        Err(EngineError::Rejected(Rejection::MergeNotReady { .. }))
    ));

    sim.world.set_ci(CiScript::Pass);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    let t = sim.task(1);
    let gate = t.gate.unwrap();
    assert_eq!(gate.outcome, GateOutcome::Passed);
    assert_eq!(gate.head_sha, t.change_request.unwrap().head_sha);
    assert_eq!(sim.stats().merges, 0, "nothing merges without the operator");

    request_merge(&sim).unwrap();
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Done));
    assert_eq!(sim.stats().merges, 1);
}

#[test]
fn a_failing_check_blocks_as_ci_red_with_the_failing_names() {
    let mut sim = sim(CiScript::Fail(vec!["lint".into(), "test".into()]));
    assert!(sim.run_until(STEPS, blocked));
    let t = sim.task(1);
    let head = t.change_request.as_ref().unwrap().head_sha.clone();
    let blocker = t.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::CiRed);
    assert_eq!(
        blocker.params,
        json!({ "checks": ["lint", "test"], "head_sha": head })
    );
    match request_merge(&sim) {
        Err(EngineError::Rejected(Rejection::MergeNotReady { gate_outcome, .. })) => {
            assert_eq!(gate_outcome.as_deref(), Some("failed"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(sim.stats().merges, 0);
}

#[test]
fn no_check_within_the_appear_timeout_blocks_as_ci_red_no_checks() {
    let mut sim = sim(CiScript::Silent);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingCi)
    }));
    let waiting_from = sim.engine().now();
    assert!(sim.run_until(STEPS, blocked));
    let t = sim.task(1);
    let blocker = t.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::CiRed);
    assert_eq!(blocker.params["reason"], json!("no_checks"));
    let waited = blocker.opened_at.since(waiting_from);
    let timeout = SimConfig::default().loop_cfg.ci_appear_timeout_ms;
    assert!(
        waited > timeout - 15_000 && waited <= timeout + 2 * 15_000,
        "{waited}"
    );
}

#[test]
fn a_change_merged_on_the_host_goes_on_to_sync() {
    let mut sim = sim(CiScript::Pass);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    let number = sim.task(1).change_request.unwrap().number;
    sim.world.merge_outside(number);
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Done));
    let t = sim.task(1);
    let cr = t.change_request.unwrap();
    assert_eq!(cr.state, ChangeState::Merged);
    assert!(sim.world.is_ancestor(
        cr.merge_commit_sha.as_ref().unwrap(),
        &sim.world.base_head()
    ));
    // The one merge was the host's.
    assert_eq!(sim.stats().merges, 1);
}

#[test]
fn a_change_closed_without_merge_blocks_as_change_closed() {
    let mut sim = sim(CiScript::Silent);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingCi)
    }));
    let number = sim.task(1).change_request.unwrap().number;
    sim.world.close_outside(number);
    assert!(sim.run_until(STEPS, blocked));
    let t = sim.task(1);
    assert_eq!(t.open_blocker().unwrap().code, BlockerCode::ChangeClosed);
    assert_eq!(t.change_request.unwrap().state, ChangeState::Closed);
}
