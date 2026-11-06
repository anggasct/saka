#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::hook::{self, HookRun, RawHook};
use saka_core::vocab::{BlockerCode, HookRunState, Stage, StagePhase, WorkItemState};
use saka_engine::{EngineError, HookRunFilter, Page};
use saka_testkit::{Sim, SimConfig};

const STEPS: usize = 8_000;

fn gate_hook(name: &str, mode: &str) -> RawHook {
    RawHook {
        name: name.into(),
        project: "tasks".into(),
        on: "merged".into(),
        mode: Some(mode.into()),
        action: "notify".into(),
        channel: Some("ops".into()),
        ..RawHook::default()
    }
}

fn sim(hooks: Vec<RawHook>) -> Sim {
    Sim::new(SimConfig {
        hooks: hook::parse_all(&hooks).unwrap(),
        ..SimConfig::without_review()
    })
}

fn runs(sim: &Sim) -> Vec<HookRun> {
    let mut all: Vec<HookRun> = sim
        .engine()
        .hook_runs(
            &HookRunFilter {
                project: Some(sim.project_id),
                ..HookRunFilter::default()
            },
            &Page {
                after: None,
                limit: 200,
            },
        )
        .unwrap()
        .items;
    all.sort_by_key(|r| (r.queued_at, r.id.0));
    all
}

fn at_teardown(s: &Sim) -> bool {
    s.task(1).is_at(Stage::Sync, StagePhase::TearingDown)
}

fn done(s: &Sim) -> usize {
    s.tasks()
        .iter()
        .filter(|t| t.item.state == WorkItemState::Done)
        .count()
}

fn blocked_on_hook(s: &Sim) -> bool {
    s.task(1)
        .open_blocker()
        .is_some_and(|b| b.code == BlockerCode::HookFailed)
}

fn fail_notify(sim: &Sim, times: usize) {
    for _ in 0..times {
        sim.hooks.script_notify(saka_ports::PortError::permanent(
            "notify_channel_unconfigured",
            serde_json::json!({ "channel": "ops" }),
        ));
    }
}

#[test]
fn a_task_is_done_only_after_its_gate_hook_passed() {
    let mut sim = sim(vec![gate_hook("smoke", "gate")]);
    sim.hooks.script_notify(saka_ports::PortError::retryable(
        "notify_unavailable",
        serde_json::json!({}),
    ));
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        at_teardown(s)
            && s.task(1)
                .environment
                .as_ref()
                .is_some_and(|e| e.state == saka_core::vocab::EnvironmentState::TornDown)
    }));
    assert_eq!(done(&sim), 0, "the gate is still pending");
    assert_eq!(runs(&sim)[0].state, HookRunState::Pending);

    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(done(&sim), 2);
    let all = runs(&sim);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|r| r.state == HookRunState::Succeeded));
    assert_eq!(all[0].attempts, 2);
    assert_eq!(sim.events_of("delivery.work_item.done").len(), 2);
}

#[test]
fn an_after_hook_never_holds_a_task_even_when_it_fails() {
    let mut sim = sim(vec![gate_hook("tell", "after")]);
    fail_notify(&sim, 2);
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    assert!(runs(&sim).iter().all(|r| r.state == HookRunState::Failed));
}

#[test]
fn a_failed_gate_blocks_the_task_and_a_retry_runs_it_again() {
    let mut sim = sim(vec![gate_hook("smoke", "gate")]);
    fail_notify(&sim, 1);
    sim.approve();
    assert!(sim.run_until(STEPS, blocked_on_hook));
    let blocker = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(blocker.params["hook"], "smoke");
    assert_eq!(blocker.params["action"], "notify");
    assert_eq!(
        blocker.params["error"]["code"],
        "notify_channel_unconfigured"
    );
    let failed = runs(&sim).remove(0);
    assert_eq!(blocker.params["hook_run_id"], serde_json::json!(failed.id));
    assert!(
        sim.task(1).is_at(Stage::Sync, StagePhase::TearingDown)
            || sim.task(1).item.state == WorkItemState::Blocked
    );
    assert_eq!(done(&sim), 0);
    assert_eq!(
        sim.task(2).item.state,
        WorkItemState::Queued,
        "the next step waits for this one"
    );

    for _ in 0..40 {
        sim.step();
    }
    assert!(
        blocked_on_hook(&sim),
        "a hook failure is never retried by itself"
    );

    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry(sim.task(1).item.id, "fixed the hook", &cause)
        .unwrap();
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(done(&sim), 2);
    let all = runs(&sim);
    assert_eq!(all[1].rerun_of, Some(failed.id));
    assert_eq!(all[1].state, HookRunState::Succeeded);
    assert_eq!(sim.events_of("delivery.work_item.done").len(), 2);
}

#[test]
fn a_gate_run_that_holds_a_task_cannot_be_run_again_directly() {
    let mut sim = sim(vec![gate_hook("smoke", "gate")]);
    fail_notify(&sim, 1);
    sim.approve();
    assert!(sim.run_until(STEPS, blocked_on_hook));
    let failed = runs(&sim).remove(0);
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let err = engine
        .rerun_hook(failed.id, "just run it", &cause, None)
        .unwrap_err();
    assert!(
        matches!(
            err,
            EngineError::Rejected(saka_core::rejection::Rejection::HookRunGated { .. })
        ),
        "{err:?}"
    );
}

#[test]
fn a_gate_that_was_removed_from_the_config_is_recorded_as_skipped_and_the_task_goes_on() {
    let mut sim = sim(vec![gate_hook("smoke", "gate")]);
    fail_notify(&sim, 1);
    sim.approve();
    assert!(sim.run_until(STEPS, blocked_on_hook));
    sim.cfg.hooks.clear();
    sim.restart();
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry(sim.task(1).item.id, "dropped the gate", &cause)
        .unwrap();
    assert!(sim.run_to_shipped(STEPS));
    let all = runs(&sim);
    let skipped = all.iter().find(|r| r.rerun_of.is_some()).unwrap();
    assert_eq!(skipped.state, HookRunState::Succeeded);
    assert_eq!(skipped.result.as_ref().unwrap()["skipped"], "hook_removed");
    assert_eq!(skipped.attempts, 0);
}

#[test]
fn a_restart_while_a_gate_is_pending_finishes_the_task_exactly_once() {
    let mut sim = sim(vec![gate_hook("smoke", "gate")]);
    sim.hooks.script_notify(saka_ports::PortError::retryable(
        "notify_unavailable",
        serde_json::json!({}),
    ));
    sim.approve();
    assert!(sim.run_until(STEPS, at_teardown));
    sim.restart();
    assert!(sim.run_to_shipped(STEPS));
    sim.restart();
    for _ in 0..60 {
        sim.step();
    }
    assert_eq!(sim.events_of("delivery.work_item.done").len(), 2);
    assert_eq!(sim.events_of("delivery.stage_run.succeeded").len(), 6);
}

#[test]
fn the_manual_retry_limit_of_the_sync_stage_applies_to_a_failing_gate() {
    let mut sim = sim(vec![gate_hook("smoke", "gate")]);
    fail_notify(&sim, 4);
    sim.approve();
    for round in 0..3 {
        assert!(sim.run_until(STEPS, blocked_on_hook), "round {round}");
        let engine = sim.engine();
        let cause = engine.cause("human:operator:local");
        engine.retry(sim.task(1).item.id, "again", &cause).unwrap();
        assert!(sim.run_until(STEPS, |s| !s.task(1).item.state.is_terminal()
            && s.task(1).open_blocker().is_none()));
    }
    assert!(sim.run_until(STEPS, blocked_on_hook));
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let err = engine
        .retry(sim.task(1).item.id, "once more", &cause)
        .unwrap_err();
    assert!(
        matches!(
            err,
            EngineError::Rejected(saka_core::rejection::Rejection::RetryLimitReached { .. })
        ),
        "{err:?}"
    );
}
