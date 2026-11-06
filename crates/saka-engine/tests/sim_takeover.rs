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

use saka_core::vocab::BlockerCode;
use saka_testkit::{CiScript, HOME_REPO};

fn second_dev_run_with_a_session(ci_then: CiScript) -> Sim {
    let mut cfg = SimConfig {
        ci: CiScript::Fail(vec!["test".into()]),
        ..SimConfig::default()
    };
    cfg.operator.retry = vec![];
    let mut sim = Sim::new(cfg);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Blocked));
    sim.world.set_script(hanging());
    sim.world.set_ci(ci_then);
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry(sim.task(1).item.id, "try again", &cause)
        .unwrap();
    assert!(sim.run_until(STEPS, in_session));
    sim
}

fn push_by_hand(sim: &Sim) -> String {
    let number = sim.task(1).change_request.unwrap().number;
    sim.world.push_to_change(HOME_REPO, number)
}

#[test]
fn a_push_by_hand_stops_the_agent_and_a_retry_continues_on_top_of_the_commit() {
    let mut sim = second_dev_run_with_a_session(CiScript::Pass);
    let human = push_by_hand(&sim);

    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Blocked));
    let t = sim.task(1);
    assert_eq!(t.open_blocker().unwrap().code, BlockerCode::BranchMoved);
    assert_eq!(t.change_request.as_ref().unwrap().head_sha, human);
    assert_eq!(sim.sessions(1).pop().unwrap().state, SessionState::Ended);

    sim.world.set_script(AgentScript::default());
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine.retry(t.item.id, "keep going", &cause).unwrap();
    assert!(sim.run_to_shipped(STEPS));
    assert!(sim.world.checkouts().iter().any(|(_, sha)| *sha == human));
    assert_eq!(sim.task(1).item.state, WorkItemState::Done);
}

#[test]
fn a_push_by_hand_can_be_taken_over_through_the_primary_action() {
    let mut sim = second_dev_run_with_a_session(CiScript::Pass);
    push_by_hand(&sim);
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Blocked));

    let task = sim.task(1);
    let blocker = task.open_blocker().unwrap().id;
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .resolve_blocker_with(task.item.id, blocker, None, "mine now", &cause, None)
        .unwrap();
    assert_eq!(sim.task(1).item.state, WorkItemState::TakenOver);
}

#[test]
fn a_worktree_with_uncommitted_work_is_never_moved_to_the_commit_pushed_by_hand() {
    let mut sim = second_dev_run_with_a_session(CiScript::Pass);
    let human = push_by_hand(&sim);
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Blocked));

    sim.world.dirty_worktrees();
    sim.world.set_script(AgentScript::default());
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let id = sim.task(1).item.id;
    engine.retry(id, "keep going", &cause).unwrap();
    assert!(sim.run_until(STEPS, |s| s.task(1).is_at_stage(Stage::Merge)
        || s.task(1).item.state == WorkItemState::Blocked));
    assert!(sim.world.checkouts().iter().all(|(_, sha)| *sha != human));
}
