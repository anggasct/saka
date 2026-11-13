#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{BlockerAction, BlockerCode, GateSource, Stage, StagePhase, WorkItemState};
use saka_testkit::{CiScript, RerunScript, Sim, SimConfig};
use serde_json::json;

const STEPS: usize = 8_000;
const CAUSE: &str = "human:operator:local";

fn sim(ci_reruns: u32, rerun: RerunScript, merge: bool) -> Sim {
    let mut cfg = SimConfig {
        ci: CiScript::Fail(vec!["test".into()]),
        ..SimConfig::default()
    };
    cfg.workflow.ci_reruns = ci_reruns;
    cfg.operator.merge = merge;
    cfg.operator.retry = vec![];
    let mut sim = Sim::new(cfg);
    sim.world.set_rerun(rerun);
    sim.approve();
    sim
}

fn blocked(s: &Sim) -> bool {
    s.task(1).item.state == WorkItemState::Blocked
}

fn rerun_requests(sim: &Sim) -> Vec<serde_json::Value> {
    sim.events_of("delivery.gate.rerun_requested")
        .into_iter()
        .map(|e| e.payload)
        .collect()
}

#[test]
fn a_check_that_fails_once_goes_on_to_review_and_merge_with_the_flake_on_record() {
    let mut sim = sim(1, RerunScript::Passes, true);
    assert!(sim.run_to_shipped(STEPS));

    assert_eq!(sim.events_of("delivery.work_item.blocked").len(), 0);
    let requests = rerun_requests(&sim);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["number"], json!(1));
    assert_eq!(requests[0]["checks"], json!(["test"]));
    assert_eq!(requests[0]["manual"], json!(false));
    assert_eq!(sim.stats().ci_reruns, 1);

    let evidence: Vec<serde_json::Value> = sim
        .events_of("delivery.gate.observed")
        .into_iter()
        .filter_map(|e| e.payload.get("passed_after_rerun").cloned())
        .collect();
    assert_eq!(evidence, [json!({ "reruns": 1, "checks": ["test"] })]);

    let metrics = sim.engine().metrics().unwrap();
    assert!(metrics.ci_reruns.contains(&("passed".to_string(), 1)));
}

#[test]
fn a_gate_that_passes_after_a_rerun_keeps_the_evidence_on_the_task() {
    let mut sim = sim(1, RerunScript::Passes, false);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    let task = sim.task(1);
    let head = task.change_request.clone().unwrap().head_sha;
    let gate = task.gate_for(GateSource::Scm, &head).unwrap();
    let evidence = gate.passed_after_rerun.clone().unwrap();
    assert_eq!((evidence.reruns, evidence.checks), (1, vec!["test".into()]));
}

#[test]
fn a_failure_after_the_rerun_blocks_with_retry_as_the_primary_action() {
    let mut sim = sim(1, RerunScript::StillFails, false);
    assert!(sim.run_until(STEPS, blocked));
    let task = sim.task(1);
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::CiRed);
    assert_eq!(blocker.params["reruns"], json!(1));
    assert_eq!(blocker.params["rerunnable"], json!(true));
    assert_eq!(
        saka_core::blocker::primary_action(&task).unwrap().kind,
        BlockerAction::Retry
    );
    assert_eq!(rerun_requests(&sim).len(), 1);
    assert_eq!(sim.stats().ci_reruns, 1);
    let metrics = sim.engine().metrics().unwrap();
    assert!(metrics.ci_reruns.contains(&("failed".to_string(), 1)));
}

#[test]
fn a_provider_that_reruns_nothing_blocks_at_once_and_is_counted_unsupported() {
    let mut sim = sim(1, RerunScript::Unsupported, false);
    assert!(sim.run_until(STEPS, blocked));
    let blocker = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(blocker.params["rerunnable"], json!(false));
    assert_eq!(sim.stats().ci_reruns, 0);
    let metrics = sim.engine().metrics().unwrap();
    assert!(metrics.ci_reruns.contains(&("unsupported".to_string(), 1)));
}

#[test]
fn a_refused_rerun_is_the_same_as_nothing_to_rerun() {
    let mut sim = sim(1, RerunScript::Refused, false);
    assert!(sim.run_until(STEPS, blocked));
    let blocker = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(blocker.params["rerun_unsupported"], json!(true));
}

#[test]
fn with_automatic_reruns_off_ci_red_comes_first_and_rerun_checks_resumes_at_the_same_head() {
    let mut sim = sim(0, RerunScript::Passes, false);
    assert!(sim.run_until(STEPS, blocked));
    assert!(rerun_requests(&sim).is_empty());
    let task = sim.task(1);
    let head = task.change_request.clone().unwrap().head_sha;
    let blocker = task.open_blocker().cloned().unwrap();
    assert_eq!(blocker.params["reruns"], json!(0));
    assert_eq!(
        saka_core::blocker::primary_action(&task).unwrap().kind,
        BlockerAction::RerunChecks
    );

    let engine = sim.engine();
    engine
        .resolve_blocker_with(
            task.item.id,
            blocker.id,
            Some(BlockerAction::RerunChecks),
            "looks flaky",
            &engine.cause(CAUSE),
            None,
        )
        .unwrap();
    let after = sim.task(1);
    assert!(after.is_at(Stage::Merge, StagePhase::AwaitingCi));
    assert_eq!(after.change_request.as_ref().unwrap().head_sha, head);
    let requests = rerun_requests(&sim);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["manual"], json!(true));
    assert_eq!(requests[0]["head_sha"], json!(head));

    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    assert_eq!(sim.stats().ci_reruns, 1);
    assert_eq!(sim.events_of("delivery.work_item.blocked").len(), 1);
}

#[test]
fn the_dedicated_action_does_the_same_and_retry_stays_available() {
    let mut sim = sim(0, RerunScript::Passes, false);
    assert!(sim.run_until(STEPS, blocked));
    let task = sim.task(1);
    let actions = saka_core::task::available_actions(&task);
    assert!(actions.contains(&"rerun_checks") && actions.contains(&"retry"));
    let engine = sim.engine();
    engine
        .rerun_checks_with(task.item.id, "flaky", &engine.cause(CAUSE), None)
        .unwrap();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
}

#[test]
fn the_operator_limit_stops_manual_reruns_at_one_head() {
    let mut cfg = SimConfig {
        ci: CiScript::Fail(vec!["test".into()]),
        ..SimConfig::default()
    };
    cfg.workflow.ci_reruns = 0;
    cfg.loop_cfg.max_ci_reruns = 1;
    cfg.operator.merge = false;
    cfg.operator.retry = vec![];
    let mut sim = Sim::new(cfg);
    sim.world.set_rerun(RerunScript::StillFails);
    sim.approve();
    assert!(sim.run_until(STEPS, blocked));
    let engine = sim.engine();
    let id = sim.task(1).item.id;
    engine
        .rerun_checks_with(id, "once", &engine.cause(CAUSE), None)
        .unwrap();
    assert!(sim.run_until(STEPS, blocked));
    let task = sim.task(1);
    assert_eq!(
        task.open_blocker().unwrap().params["rerunnable"],
        json!(false)
    );
    let refused = engine.rerun_checks_with(id, "twice", &engine.cause(CAUSE), None);
    assert!(matches!(
        refused,
        Err(saka_engine::EngineError::Rejected(
            saka_core::rejection::Rejection::CiRerunUnavailable { .. }
        ))
    ));
}
