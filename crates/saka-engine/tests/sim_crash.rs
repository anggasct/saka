#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use std::sync::OnceLock;

use proptest::prelude::*;
use saka_core::job::JobKind;
use saka_core::vocab::{
    BlockerCode, ResumeCause, SessionState, SpecState, Stage, StagePhase, WorkItemState,
};
use saka_testkit::{AgentOutcome, AgentScript, Operator, Sim, SimConfig, SimOutcome, Step};
use serde_json::json;

const STEPS: usize = 5_000;

fn waiting_script() -> AgentScript {
    AgentScript {
        outcome: AgentOutcome::Commit,
        ask: Some(("Bash".into(), json!({ "command": "make deploy" }))),
    }
}

fn settle(sim: &mut Sim) {
    for _ in 0..STEPS {
        if sim.step() == Step::Idle {
            return;
        }
    }
    panic!("never settled");
}

#[test]
fn a_restart_loses_live_sessions_and_the_task_resumes_them_once() {
    let mut sim = Sim::new(SimConfig {
        agent: waiting_script(),
        operator: Operator {
            merge: true,
            retry: vec![],
            answer: None,
        },
        ..SimConfig::default()
    });
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Waiting)
    }));
    let runner = sim.world.running_sessions();
    assert_eq!(runner.len(), 1);

    sim.restart();
    let t = sim.task(1);
    assert_eq!(t.item.state, WorkItemState::InStage);
    assert!(t.open_blocker().is_none());
    assert_eq!(t.session.unwrap().state, SessionState::Lost);

    sim.cfg.operator.answer = Some(true);
    assert!(sim.run_to_shipped(STEPS));
    let stats = sim.stats();
    assert_eq!((stats.prs_opened, stats.merges), (2, 2));
    let sessions = sim.sessions(1);
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].state, SessionState::Resumed);
    assert_eq!(sessions[1].resumed_from_session_id, Some(sessions[0].id));
    assert_eq!(sessions[1].resume_cause, Some(ResumeCause::Lost));
}

#[test]
fn a_job_leased_by_a_crashed_process_runs_again_after_its_lease_without_a_second_effect() {
    let mut sim = Sim::new(SimConfig {
        operator: Operator {
            merge: false,
            ..Operator::default()
        },
        ..SimConfig::default()
    });
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    settle(&mut sim);
    let t = sim.task(1);
    let engine = sim.engine();
    engine
        .request_merge(
            t.item.id,
            t.item.version,
            None,
            &engine.cause("human:operator:local"),
        )
        .unwrap();
    drop(engine);

    assert!(sim.crash_mid_job());
    assert_eq!(sim.stats().merges, 1);
    assert!(sim.task(1).is_at(Stage::Merge, StagePhase::Merging));

    // The lease (60 s) holds the job until it runs out.
    let crashed_at = sim.engine().now();
    let mut ran_at = None;
    for _ in 0..STEPS {
        if let Step::Job(report) = sim.step()
            && report.kind == JobKind::ScmMerge
        {
            ran_at = Some(sim.engine().now());
            break;
        }
    }
    assert!(ran_at.unwrap().since(crashed_at) >= saka_store::LEASE_MS);
    assert!(
        sim.task(1).is_at(Stage::Sync, StagePhase::Verifying)
            || sim.task(1).item.stage == Some(Stage::Sync)
    );
    assert_eq!(
        sim.stats().merges,
        1,
        "the repeated job read the merge instead of merging again"
    );

    sim.cfg.operator.merge = true;
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.stats().merges, 2);
}

fn crash_config() -> SimConfig {
    let mut cfg = SimConfig::default();
    cfg.operator.retry = vec![
        BlockerCode::SessionLost,
        BlockerCode::SessionFailed,
        BlockerCode::EnvSetupFailed,
        BlockerCode::ScmUnavailable,
        BlockerCode::DodFailed,
    ];
    cfg
}

#[test]
fn a_crash_while_checking_the_merge_does_not_block_the_task() {
    let mut sim = Sim::new(SimConfig {
        operator: Operator {
            merge: true,
            retry: vec![],
            answer: None,
        },
        ..SimConfig::default()
    });
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Sync, StagePhase::Verifying)
    }));
    assert!(sim.crash_mid_job(), "the check is due");
    let mut blocked = false;
    assert!(sim.run_until(STEPS, |s| {
        blocked |= s.task(1).item.state == WorkItemState::Blocked;
        s.task(1).item.state == WorkItemState::Done
    }));
    assert!(!blocked);
}

fn baseline() -> &'static (SimOutcome, usize) {
    static BASELINE: OnceLock<(SimOutcome, usize)> = OnceLock::new();
    BASELINE.get_or_init(run_baseline)
}

fn run_baseline() -> (SimOutcome, usize) {
    let mut sim = Sim::new(crash_config());
    sim.approve();
    let mut steps = 0;
    while sim.spec().state != SpecState::Shipped {
        sim.operate();
        sim.step();
        steps += 1;
        assert!(steps < STEPS);
    }
    (sim.outcome(), steps)
}

#[test]
fn the_baseline_ships_with_one_change_and_one_merge_per_task() {
    let (outcome, _) = baseline().clone();
    assert_eq!(outcome.spec_state, SpecState::Shipped);
    assert_eq!(
        (outcome.prs_opened, outcome.merges, outcome.pushes),
        (2, 2, 2)
    );
    assert!(outcome.merges_on_base);
    assert!(outcome.leftover_branches.is_empty());
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn crashes_at_random_points_end_the_same_with_no_duplicate_effects(
        raw in prop::collection::vec((0.0f64..1.0, any::<bool>()), 1..4),
    ) {
        let (expected, length) = baseline().clone();
        let mut points: Vec<(usize, bool)> = raw
            .iter()
            .map(|(at, mid_job)| ((at * length as f64) as usize, *mid_job))
            .collect();
        points.sort();

        let mut sim = Sim::new(crash_config());
        sim.approve();
        let mut steps = 0;
        let mut next = points.into_iter().peekable();
        while sim.spec().state != SpecState::Shipped {
            while next.peek().is_some_and(|(at, _)| *at == steps) {
                let (_, mid_job) = next.next().unwrap();
                if !(mid_job && sim.crash_mid_job()) {
                    sim.restart();
                }
            }
            sim.operate();
            sim.step();
            steps += 1;
            prop_assert!(steps < STEPS, "stuck: {:?}", sim.tasks().iter().map(|t| (t.item.state, t.item.stage_phase, t.open_blocker().map(|b| b.code))).collect::<Vec<_>>());
        }
        prop_assert!(sim.restarts >= 1);
        prop_assert_eq!(sim.outcome(), expected);
    }
}
