#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{BlockerCode, Stage, StagePhase, Trigger, WorkItemState};
use saka_testkit::{CiScript, CommandScript, Sim, SimConfig};

const STEPS: usize = 8_000;
const STEP_COMMAND: &str = "cd cli && go test ./...";

fn sim() -> Sim {
    let mut cfg = SimConfig {
        ci: CiScript::Pass,
        ..SimConfig::default()
    };
    cfg.operator.merge = false;
    cfg.operator.retry = vec![];
    Sim::new(cfg)
}

fn crash_gate(sim: &Sim, times: usize) {
    for _ in 0..times {
        sim.world.script_command(STEP_COMMAND, CommandScript::Crash);
    }
}

fn blocked(s: &Sim) -> bool {
    s.task(1).item.state == WorkItemState::Blocked
}

fn runs(sim: &Sim, stage: &str) -> Vec<(Trigger, Option<String>)> {
    let id = sim.task(1).item.id.0.to_string();
    sim.engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT trigger, reason FROM stage_run WHERE work_item_id = ?1 AND stage = ?2 ORDER BY attempt",
            )?;
            let rows = stmt.query_map([id.as_str(), stage], |r| {
                let trigger: String = r.get(0)?;
                Ok((trigger, r.get::<_, Option<String>>(1)?))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (t, reason) = row?;
                out.push((t.parse().unwrap(), reason));
            }
            Ok(out)
        })
        .unwrap()
}

fn auto_resolved(sim: &Sim) -> i64 {
    sim.engine()
        .store()
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM blocker WHERE resolution = 'auto'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

#[test]
fn a_transient_failure_is_retried_twice_with_backoff_then_left_to_the_operator() {
    let mut sim = sim();
    crash_gate(&sim, 9);
    sim.approve();

    assert!(sim.run_until(STEPS, blocked));
    let first = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(first.code, BlockerCode::CiRed);
    assert_eq!(first.params["reason"], "runner_failed");

    assert!(sim.run_until(STEPS, |s| !blocked(s)));
    let waited = sim.engine().now().since(first.opened_at);
    assert!((60_000..300_000).contains(&waited), "{waited}");

    assert!(sim.run_until(STEPS, blocked));
    let second = sim.task(1).open_blocker().cloned().unwrap();
    assert!(sim.run_until(STEPS, |s| !blocked(s)));
    assert!(sim.engine().now().since(second.opened_at) >= 300_000);

    assert!(sim.run_until(STEPS, blocked));
    let third_at = sim.engine().now();
    assert!(!sim.run_until(400, |s| !blocked(s)
        || s.engine().now().since(third_at) > 3_600_000));
    assert!(blocked(&sim), "a third occurrence stays open");
    assert_eq!(auto_resolved(&sim), 2);
    let runs = runs(&sim, "merge");
    assert_eq!(
        runs,
        vec![
            (Trigger::Auto, None),
            (Trigger::Auto, Some("auto retry: ci_red".into())),
            (Trigger::Auto, Some("auto retry: ci_red".into())),
        ]
    );
    let metrics = sim.engine().metrics().unwrap();
    assert_eq!(metrics.auto_retries, vec![("ci_red".to_string(), 2)]);

    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry(sim.task(1).item.id, "runner is back", &cause)
        .unwrap();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    let t = sim.task(1);
    assert_eq!(t.retries[&Stage::Merge].manual, 1);
    assert_eq!(t.retries[&Stage::Merge].auto, 0);
}

#[test]
fn an_unavailable_scm_is_retried_automatically_and_a_manual_retry_restores_the_allowance() {
    let mut sim = sim();
    sim.world.fail_pushes(15);
    sim.approve();

    let mut opened = Vec::new();
    for _ in 0..3 {
        assert!(sim.run_until(STEPS, blocked));
        let b = sim.task(1).open_blocker().cloned().unwrap();
        assert_eq!(b.code, BlockerCode::ScmUnavailable);
        assert_eq!(b.params["phase"], "pushing");
        opened.push(b.opened_at);
        if opened.len() < 3 {
            assert!(sim.run_until(STEPS, |s| !blocked(s)));
        }
    }
    let waits: Vec<i64> = opened.windows(2).map(|w| w[1].since(w[0])).collect();
    assert!(waits[0] >= 60_000 && waits[1] >= 300_000, "{waits:?}");
    let third_at = sim.engine().now();
    assert!(!sim.run_until(400, |s| !blocked(s)
        || s.engine().now().since(third_at) > 3_600_000));
    assert_eq!(auto_resolved(&sim), 2);
    let retried = Some("auto retry: scm_unavailable".to_string());
    assert_eq!(
        runs(&sim, "dev"),
        vec![
            (Trigger::Auto, None),
            (Trigger::Auto, retried.clone()),
            (Trigger::Auto, retried),
        ]
    );

    sim.world.fail_pushes(5);
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry(sim.task(1).item.id, "network is back", &cause)
        .unwrap();
    assert!(sim.run_until(STEPS, |s| s.task(1).item.stage == Some(Stage::Merge)));
    assert_eq!(auto_resolved(&sim), 3);
    let metrics = sim.engine().metrics().unwrap();
    assert_eq!(
        metrics.auto_retries,
        vec![("scm_unavailable".to_string(), 3)]
    );
}

#[test]
fn failures_that_need_a_person_are_never_retried_automatically() {
    let mut cfg = SimConfig {
        ci: CiScript::Fail(vec!["test".into()]),
        ..SimConfig::default()
    };
    cfg.operator.retry = vec![];
    let mut sim = Sim::new(cfg);
    sim.approve();
    assert!(sim.run_until(STEPS, blocked));
    let at = sim.engine().now();
    assert!(!sim.run_until(600, |s| !blocked(s)
        || s.engine().now().since(at) > 3_600_000));
    assert!(blocked(&sim));
    assert_eq!(auto_resolved(&sim), 0);
}
