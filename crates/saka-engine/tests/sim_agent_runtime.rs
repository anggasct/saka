#![allow(
    clippy::unwrap_used,
    reason = "simulations fail loudly on a broken setup"
)]

use saka_core::model::Session;
use saka_core::vocab::{ResumeCause, SessionState, WorkItemState};
use saka_testkit::{AgentOutcome, AgentScript, Operator, Sim, SimConfig};
use serde_json::json;

const STEPS: usize = 5_000;

fn sim(agent: AgentScript) -> Sim {
    let mut sim = Sim::new(SimConfig {
        agent,
        operator: Operator {
            merge: true,
            retry: vec![],
            answer: Some(true),
        },
        ..SimConfig::default()
    });
    sim.approve();
    sim
}

fn hanging() -> AgentScript {
    AgentScript {
        outcome: AgentOutcome::Hang,
        ..AgentScript::default()
    }
}

fn session(sim: &Sim) -> Session {
    sim.task(1).session.unwrap()
}

fn run_until_initialized(sim: &mut Sim) -> Session {
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Running && x.cli_session_id.is_some())
    }));
    session(sim)
}

fn lost_cause(s: &Session) -> serde_json::Value {
    s.end_reason.as_ref().unwrap().params["cause"].clone()
}

fn assert_resumed_once(sim: &Sim, first: &Session, cause: &str) {
    let sessions = sim.sessions(1);
    assert_eq!(sessions.len(), 2, "{sessions:?}");
    assert_eq!(sessions[0].id, first.id);
    assert_eq!(sessions[0].state, SessionState::Resumed);
    assert_eq!(lost_cause(&sessions[0]), json!(cause));
    assert_eq!(sessions[1].resumed_from_session_id, Some(first.id));
    assert_eq!(sessions[1].resume_cause, Some(ResumeCause::Lost));
    assert_eq!(sessions[1].state, SessionState::Ended);
    assert!(
        sim.world
            .resumes()
            .contains(&Some(format!("cli-{}", first.id))),
        "the replacement continues the CLI session"
    );
    assert!(
        sim.world
            .prompts()
            .iter()
            .any(|p| p.starts_with("Your previous session stopped (the runner was lost).")),
        "the replacement opens with the resume turn"
    );
}

#[test]
fn a_restart_while_a_turn_runs_keeps_the_session_and_processes_each_event_once() {
    let mut sim = sim(AgentScript::default());
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Running)
    }));
    let id = session(&sim).id;
    assert!(
        sim.world.has_events(),
        "the turn's events are still in flight"
    );

    sim.restart();
    assert!(
        !sim.engine().readiness().sessions_reattached,
        "readiness waits for the reattach"
    );
    assert!(sim.run_to_shipped(STEPS));
    assert!(sim.engine().readiness().sessions_reattached);

    let sessions = sim.sessions(1);
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        (sessions[0].id, sessions[0].state),
        (id, SessionState::Ended)
    );
    assert_eq!(sim.usage(1).len() as u32, sessions[0].turns);
    let rows = sim.timeline(id);
    let log = sim.world.session_log(id);
    assert_eq!(
        rows.iter().map(|r| r.offset).collect::<Vec<_>>(),
        log.iter().map(|e| e.offset).collect::<Vec<_>>(),
        "one timeline row per runner event"
    );
    assert_eq!(sessions[0].last_offset, log.last().unwrap().offset);
    assert!(sim.stats().reattaches >= 1);
}

#[test]
fn a_restart_while_a_permission_waits_still_delivers_the_answer_once() {
    let mut sim = sim(AgentScript {
        ask: Some(("Bash".into(), json!({ "command": "make deploy" }))),
        ..AgentScript::default()
    });
    sim.cfg.operator.answer = None;
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Waiting)
    }));
    sim.restart();
    sim.cfg.operator.answer = Some(true);
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.sessions(1).len(), 1);
    assert_eq!(sim.stats().answers_to_cli, 2, "one answer per task");
}

#[test]
fn a_dead_runner_unit_loses_the_session_and_it_resumes_once() {
    let mut sim = sim(hanging());
    let first = run_until_initialized(&mut sim);
    sim.world.kill_runner(first.id);
    sim.world.set_script(AgentScript::default());
    assert!(sim.run_to_shipped(STEPS));
    assert_resumed_once(&sim, &first, "runner_exited");
}

#[test]
fn a_silent_runner_is_reconnected_three_times_then_stopped_and_resumed() {
    let mut sim = sim(hanging());
    let first = run_until_initialized(&mut sim);
    let silent_from = sim.engine().now();
    sim.world.silence_runner(first.id);
    sim.world.set_script(AgentScript::default());
    assert!(sim.run_until(STEPS, |s| s.sessions(1).len() == 2));
    let lost = &sim.sessions(1)[0];
    assert!(
        lost.ended_at.unwrap().since(silent_from) >= 30_000,
        "a runner is given 30 s of silence"
    );
    assert!(sim.run_to_shipped(STEPS));
    assert_resumed_once(&sim, &first, "runner_silent");
    assert!(
        sim.stats().sessions_stopped >= 1,
        "the silent unit is stopped"
    );
}

#[test]
fn a_silent_runner_whose_unit_died_is_lost_without_reconnecting() {
    let mut sim = sim(hanging());
    let first = run_until_initialized(&mut sim);
    sim.world.silence_runner(first.id);
    sim.world.kill_runner(first.id);
    sim.world.set_script(AgentScript::default());
    assert!(sim.run_to_shipped(STEPS));
    assert_resumed_once(&sim, &first, "runner_exited");
}

#[test]
fn a_runner_from_another_binary_is_stopped_and_its_session_resumed() {
    let mut sim = sim(hanging());
    let first = run_until_initialized(&mut sim);
    sim.world.swap_runner_version(first.id);
    sim.restart();
    sim.world.set_script(AgentScript::default());
    assert!(sim.run_to_shipped(STEPS));
    assert_resumed_once(&sim, &first, "version_mismatch");
    assert_eq!(sim.task(1).item.state, WorkItemState::Done);
}
