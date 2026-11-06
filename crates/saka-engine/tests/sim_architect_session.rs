#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use proptest::prelude::*;
use saka_core::decision::{DecisionOption, Question};
use saka_core::ids::SpecDraftId;
use saka_core::model::Draft;
use saka_core::rejection::Rejection;
use saka_core::vocab::{
    DecisionState, LintOutcome, SessionState, SpecDraftState, SpecOrigin, SpecState,
};
use saka_engine::EngineError;
use saka_testkit::{AgentScript, DraftTurn, Operator, Sim, SimConfig};
use serde_json::Value;

const STEPS: usize = 4_000;
const PATH: &str = "docs/specs/export.md";
const IDEA: &str = "Let people export their task list as a CSV file.";
const HOUR_MS: i64 = 3_600_000;

fn bad() -> Vec<u8> {
    b"---
saka: spec
---
# Export tasks

## Acceptance criteria

- **AC-01**: `tasks export` writes a CSV file.

## Steps

| Step | Work | AC | Done when |
|------|------|----|-----------|
| S1 | Add the export command | AC-02 | `make test` |
"
    .to_vec()
}

fn good() -> Vec<u8> {
    b"---
saka: spec
---
# Export tasks

## Acceptance criteria

- **AC-01**: `tasks export` writes a CSV file.

## Done when

- `make test`
"
    .to_vec()
}

fn write(bytes: Vec<u8>) -> DraftTurn {
    DraftTurn::Write {
        path: PATH.into(),
        bytes,
    }
}

fn sim(tune: impl FnOnce(&mut SimConfig)) -> Sim {
    let mut cfg = SimConfig {
        operator: Operator::default(),
        ..SimConfig::default()
    };
    tune(&mut cfg);
    Sim::new(cfg)
}

fn conversation(sim: &Sim, turns: Vec<DraftTurn>) -> SpecDraftId {
    sim.world.script_draft(turns);
    sim.create_session(PATH, IDEA, &[]).unwrap().draft.id
}

fn run_to(sim: &mut Sim, id: SpecDraftId, done: impl Fn(&Draft) -> bool) -> Draft {
    assert!(
        sim.run_until(STEPS, |s| done(&s.draft(id))),
        "draft never reached the state: {:?}",
        sim.draft(id).draft
    );
    sim.draft(id)
}

fn awaiting(sim: &mut Sim, id: SpecDraftId, rounds: u32) -> Draft {
    run_to(sim, id, |d| {
        d.draft.state == SpecDraftState::AwaitingOperator
            && d.attempt.as_ref().is_some_and(|a| a.rounds == rounds)
    })
}

fn finished(sim: &mut Sim, id: SpecDraftId) -> Draft {
    let d = run_to(sim, id, |d| d.draft.state.is_finished());
    assert!(
        sim.run_until(STEPS, |s| s.world.running_sessions().is_empty()),
        "runner left behind"
    );
    d
}

fn rejected(result: Result<Vec<saka_core::event::Event>, EngineError>) -> Rejection {
    match result.unwrap_err() {
        EngineError::Rejected(r) => r,
        other => panic!("expected a rejection, got {other:?}"),
    }
}

fn message(sim: &Sim, id: SpecDraftId, text: &str) -> Result<(), EngineError> {
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .message_spec_draft(id, text, &cause, None)
        .map(|_| ())
}

fn commit(
    sim: &Sim,
    id: SpecDraftId,
    note: Option<&str>,
) -> Result<Vec<saka_core::event::Event>, EngineError> {
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine.commit_spec_draft(id, note, &cause, None)
}

fn failure(d: &Draft) -> (String, Value) {
    let f = d.draft.failure.clone().unwrap();
    (f.code, f.params)
}

fn question() -> Question {
    Question {
        question: "Which delimiter does the file use?".into(),
        options: vec![
            DecisionOption {
                label: "Comma".into(),
                detail: None,
            },
            DecisionOption {
                label: "Semicolon".into(),
                detail: None,
            },
        ],
        context: None,
    }
}

#[test]
fn a_session_stops_after_its_turn_waiting_for_the_operator_and_commits_nothing() {
    let mut sim = sim(|_| {});
    let id = conversation(&sim, vec![write(good())]);
    let d = awaiting(&mut sim, id, 1);
    sim.run_until(300, |_| false);
    let after = sim.draft(id);
    assert_eq!(after.draft.state, SpecDraftState::AwaitingOperator);
    assert_eq!(after.draft.version, d.draft.version);
    assert!(sim.world.doc_commits().is_empty());
    assert_eq!(sim.world.prompts().len(), 1, "no feedback turn was sent");
    let rounds = sim.draft_rounds(id);
    assert_eq!(rounds.len(), 1);
    assert_eq!(rounds[0].1.outcome, LintOutcome::Passed);
    assert_eq!(
        after.session.as_ref().map(|s| s.state),
        Some(SessionState::Idle)
    );
    assert_eq!(sim.world.running_sessions().len(), 1);
}

#[test]
fn a_message_starts_a_turn_and_the_commit_makes_one_architect_spec() {
    let mut sim = sim(|_| {});
    let revised = [good(), b"\n- `tasks export --json` also works\n".to_vec()].concat();
    let id = conversation(&sim, vec![write(good()), write(revised.clone())]);
    awaiting(&mut sim, id, 1);

    message(&sim, id, "Also document the JSON output.").unwrap();
    let working = sim.draft(id);
    assert_eq!(working.draft.state, SpecDraftState::Drafting);
    awaiting(&mut sim, id, 2);
    let prompts = sim.world.prompts();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[1], "Also document the JSON output.");

    commit(&sim, id, Some("docs: add the export spec")).unwrap();
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded, "{:?}", d.draft);
    let commits = sim.world.doc_commits();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].bytes, revised);
    assert_eq!(
        commits[0].message.as_deref(),
        Some("docs: add the export spec")
    );
    let spec = sim.spec_of(d.draft.spec_id.unwrap());
    assert_eq!(spec.origin, SpecOrigin::Architect);
    assert_eq!(spec.state, SpecState::Ready);
    assert_eq!(spec.path, PATH);
    let ended = sim.draft_sessions(id)[0].end_reason.clone().unwrap();
    assert_eq!(ended.code, "operator_commit");
}

#[test]
fn commit_message_and_text_are_bounded() {
    let mut sim = sim(|_| {});
    let id = conversation(&sim, vec![write(good())]);
    awaiting(&mut sim, id, 1);
    let long = "x".repeat(201);
    let too_long = commit(&sim, id, Some(&long)).unwrap_err();
    assert!(matches!(
        too_long,
        EngineError::Invalid {
            field: "message",
            ..
        }
    ));
    let empty = message(&sim, id, "   ").unwrap_err();
    assert!(matches!(empty, EngineError::Invalid { field: "text", .. }));
    assert_eq!(sim.draft(id).draft.state, SpecDraftState::AwaitingOperator);
}

#[test]
fn the_operator_actions_are_refused_outside_their_state() {
    let mut sim = sim(|_| {});
    let id = conversation(&sim, vec![write(bad()), write(good())]);
    assert_eq!(
        rejected(commit(&sim, id, None)),
        Rejection::DraftNotWaiting,
        "queued"
    );
    awaiting(&mut sim, id, 1);
    assert_eq!(
        sim.draft_rounds(id)[0].1.outcome,
        LintOutcome::Failed,
        "the first file does not lint"
    );
    assert_eq!(rejected(commit(&sim, id, None)), Rejection::DraftLintFailed);
    message(&sim, id, "fix the step table").unwrap();
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    assert!(matches!(
        message(&sim, id, "and again").unwrap_err(),
        EngineError::Rejected(Rejection::DraftNotWaiting)
    ));
    assert_eq!(
        rejected(engine.discard_spec_draft(id, "no", &cause, None)),
        Rejection::DraftNotWaiting
    );
    awaiting(&mut sim, id, 2);
    commit(&sim, id, None).unwrap();
    assert_eq!(
        rejected(commit(&sim, id, None)),
        Rejection::DraftNotWaiting,
        "committing"
    );
}

#[test]
fn an_open_question_blocks_the_commit_until_the_operator_answers() {
    let mut sim = sim(|cfg| {
        cfg.agent = AgentScript {
            decisions: vec![vec![question()]].into(),
            ..AgentScript::default()
        };
    });
    let id = conversation(&sim, vec![write(good())]);
    let d = awaiting(&mut sim, id, 1);
    assert_eq!(d.decisions.len(), 1);
    assert_eq!(d.decisions[0].state, DecisionState::Asked);
    assert_eq!(
        rejected(commit(&sim, id, None)),
        Rejection::DraftDecisionOpen
    );
    assert!(matches!(
        message(&sim, id, "hello").unwrap_err(),
        EngineError::Rejected(Rejection::DraftDecisionOpen)
    ));
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .answer_decision(d.decisions[0].id, Some(1), None, &cause)
        .unwrap();
    awaiting(&mut sim, id, 2);
    assert_eq!(sim.draft(id).decisions[0].state, DecisionState::Delivered);
    commit(&sim, id, None).unwrap();
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
}

#[test]
fn discard_cancels_the_draft_and_cleans_up() {
    let mut sim = sim(|_| {});
    let id = conversation(&sim, vec![write(good())]);
    awaiting(&mut sim, id, 1);
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .discard_spec_draft(id, "wrong direction", &cause, None)
        .unwrap();
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Cancelled);
    assert!(sim.world.doc_commits().is_empty());
    assert!(sim.run_until(STEPS, |s| {
        s.draft(id)
            .environment
            .as_ref()
            .is_some_and(|e| e.state == saka_core::vocab::EnvironmentState::TornDown)
    }));
}

#[test]
fn a_draft_nobody_answers_expires_but_keeps_what_the_architect_wrote() {
    let mut sim = sim(|_| {});
    let id = conversation(&sim, vec![write(good())]);
    awaiting(&mut sim, id, 1);
    sim.clock.advance(71 * HOUR_MS);
    sim.run_until(200, |_| false);
    assert_eq!(sim.draft(id).draft.state, SpecDraftState::AwaitingOperator);
    sim.clock.advance(2 * HOUR_MS);
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Failed);
    let (code, params) = failure(&d);
    assert_eq!(code, "session_idle_expired");
    assert_eq!(params["hours"], 72);
    assert!(!saka_core::draft::can_retry(&d));
    let written = sim.engine().spec_draft_content(id).unwrap();
    assert_eq!(written.content.as_bytes(), good().as_slice());
    assert!(sim.world.doc_commits().is_empty());
}

#[test]
fn a_restart_while_waiting_keeps_the_conversation() {
    let mut sim = sim(|_| {});
    let id = conversation(&sim, vec![write(good())]);
    awaiting(&mut sim, id, 1);
    sim.restart();
    sim.run_until(300, |_| false);
    let d = sim.draft(id);
    assert_eq!(d.draft.state, SpecDraftState::AwaitingOperator);
    assert_eq!(sim.draft_rounds(id).len(), 1);
    commit(&sim, id, None).unwrap();
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    assert_eq!(sim.world.doc_commits().len(), 1);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn crashes_at_any_point_of_a_conversation_never_double_an_effect(points in proptest::collection::vec(0usize..80, 1..4)) {
        let mut sim = sim(|_| {});
        let id = conversation(&sim, vec![write(bad()), write(good())]);
        let mut points = points;
        points.sort_unstable();
        let mut step = 0;
        let mut messaged = false;
        for point in points {
            while step < point && !sim.draft(id).draft.state.is_finished() {
                let d = sim.draft(id);
                if d.draft.state == SpecDraftState::AwaitingOperator {
                    if !messaged {
                        messaged = message(&sim, id, "fix the step table").is_ok();
                    } else {
                        let _ = commit(&sim, id, None);
                    }
                }
                sim.step();
                step += 1;
            }
            sim.crash_mid_job();
        }
        while !sim.draft(id).draft.state.is_finished() {
            let d = sim.draft(id);
            if d.draft.state == SpecDraftState::AwaitingOperator {
                if !messaged {
                    messaged = message(&sim, id, "fix the step table").is_ok();
                } else {
                    let _ = commit(&sim, id, None);
                }
            }
            let moved = sim.run_until(STEPS, |s| {
                let state = s.draft(id).draft.state;
                state == SpecDraftState::AwaitingOperator || state.is_finished()
            });
            prop_assert!(moved);
        }
        let d = finished(&mut sim, id);
        prop_assert_eq!(d.draft.state, SpecDraftState::Succeeded);
        prop_assert_eq!(sim.world.doc(PATH), Some(good()));
        let mut turns: Vec<_> = sim.draft_rounds(id).iter().map(|(_, r)| (r.session_id, r.turn)).collect();
        turns.dedup();
        prop_assert_eq!(turns.len(), sim.draft_rounds(id).len());
        let live: Vec<_> = sim.draft_sessions(id).into_iter().filter(|s| s.state.is_live()).collect();
        prop_assert!(live.is_empty());
    }
}
