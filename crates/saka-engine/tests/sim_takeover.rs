#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{
    ChangeState, GateOutcome, GateSource, SessionState, SpecState, Stage, StagePhase, Trigger,
    WorkItemState,
};
use saka_testkit::{AgentOutcome, AgentScript, Sim, SimConfig, one_step_spec};

const STEPS: usize = 8_000;
const OTHER_PATH: &str = "docs/specs/other.md";

fn hanging() -> AgentScript {
    AgentScript {
        outcome: AgentOutcome::Hang,
        ..AgentScript::default()
    }
}

fn takeover(sim: &Sim) {
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .takeover_with(sim.task(1).item.id, "I will do this one", &cause, None)
        .unwrap();
}

fn give_back(sim: &Sim, stage: Option<Stage>) -> Result<(), saka_engine::EngineError> {
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let id = sim.task(1).item.id;
    sim.block_on(engine.return_with(id, stage, "done by hand", &cause, None))
        .map(|_| ())
}

fn in_session(sim: &Sim) -> bool {
    sim.task(1).live_session().is_some()
}

#[test]
fn a_task_taken_over_mid_session_stops_the_agent_and_frees_its_slot() {
    let mut sim = Sim::new(SimConfig {
        max_active_tasks: 1,
        agent: hanging(),
        ..SimConfig::default()
    });
    let other = sim.register(OTHER_PATH, &one_step_spec("Other work", ""));
    sim.approve();
    sim.approve_spec(other.spec_id, other.revision_id).unwrap();
    assert!(sim.run_until(STEPS, in_session));
    assert_eq!(
        sim.tasks_of(other.spec_id)[0].item.state,
        WorkItemState::Queued
    );

    takeover(&sim);
    assert_eq!(sim.task(1).item.state, WorkItemState::TakenOver);
    assert_eq!(sim.events_of("delivery.work_item.taken_over").len(), 1);
    assert!(sim.run_until(STEPS, |s| {
        s.tasks_of(other.spec_id)[0].item.state != WorkItemState::Queued
    }));
    let session = sim.sessions(1).pop().unwrap();
    assert_eq!(session.state, SessionState::Ended);

    let frozen = sim.task(1);
    sim.run_until(600, |_| false);
    assert_eq!(
        sim.task(1),
        frozen,
        "a taken over task never changes by itself"
    );
    assert_eq!(sim.spec().state, SpecState::Approved);
}

#[test]
fn a_task_taken_over_during_dev_returns_to_the_loop_and_ships() {
    let mut sim = Sim::new(SimConfig {
        agent: hanging(),
        ..SimConfig::default()
    });
    sim.approve();
    assert!(sim.run_until(STEPS, in_session));
    takeover(&sim);
    sim.world.set_script(AgentScript::default());

    give_back(&sim, Some(Stage::Dev)).unwrap();
    assert_eq!(sim.events_of("delivery.work_item.returned").len(), 1);
    assert!(sim.run_to_shipped(STEPS));
    let t = sim.task(1);
    assert_eq!(t.item.state, WorkItemState::Done);
    assert!(t.retries.values().all(|r| r.manual == 0 && r.auto == 0));
}

#[test]
fn a_head_pushed_by_hand_during_takeover_is_gated_and_merged_after_returning_to_merge() {
    let mut sim = Sim::new(SimConfig::default());
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingCi)
    }));
    takeover(&sim);
    assert_eq!(sim.task(1).item.state, WorkItemState::TakenOver);
    let number = sim.task(1).change_request.unwrap().number;
    let human = sim.world.push_to_change(saka_testkit::HOME_REPO, number);

    give_back(&sim, None).unwrap();
    let t = sim.task(1);
    assert_eq!(t.change_request.as_ref().unwrap().head_sha, human);
    let last = t.stage_run.as_ref().unwrap();
    assert_eq!((last.stage, last.trigger), (Stage::Merge, Trigger::Auto));
    assert!(t.gates.is_empty());

    assert!(sim.run_to_shipped(STEPS));
    let first = sim.task(1);
    for source in [GateSource::Scm, GateSource::Saka] {
        let gate = first.gate_for(source, &human).unwrap();
        assert_eq!(gate.outcome, GateOutcome::Passed, "{source}");
    }
    assert_eq!(first.change_request.unwrap().state, ChangeState::Merged);
    assert!(sim.world.checkouts().iter().any(|(_, sha)| *sha == human));
}

#[test]
fn a_closed_change_cannot_be_returned_to_the_loop() {
    let mut sim = Sim::new(SimConfig::default());
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingCi)
    }));
    takeover(&sim);
    let number = sim.task(1).change_request.unwrap().number;
    sim.world.close_outside(number);

    let err = give_back(&sim, Some(Stage::Dev)).unwrap_err();
    assert_eq!(
        err.to_string(),
        saka_core::rejection::Rejection::ChangeRequestNotOpen {
            number,
            state: "closed"
        }
        .to_string()
    );
    assert_eq!(sim.task(1).item.state, WorkItemState::TakenOver);
}
