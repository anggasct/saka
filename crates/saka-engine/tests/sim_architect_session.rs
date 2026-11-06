#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use proptest::prelude::*;
use saka_core::decision::{DecisionOption, Question};
use saka_core::ids::SpecDraftId;
use saka_core::model::Draft;
use saka_core::rejection::Rejection;
use saka_core::vocab::{
    DecisionState, LintOutcome, SessionState, SpecDraftState, SpecOrigin, SpecState, WorkItemState,
};
use saka_engine::EngineError;
use saka_store::{StoredEvent, read};
use saka_testkit::{
    APPROVAL_PATH, AgentScript, DraftTurn, Operator, Sim, SimConfig, approval_spec,
};
use serde_json::{Value, json};

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
    sim.block_on(engine.commit_spec_draft(id, note, &cause, None))
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

fn events(sim: &Sim, kind: &str) -> Vec<StoredEvent> {
    sim.engine()
        .store()
        .read(|c| read::events_after(c, 0, 100_000))
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

fn discard(sim: &Sim, id: SpecDraftId) {
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .discard_spec_draft(id, "enough", &cause, None)
        .unwrap();
}

#[test]
fn two_changed_files_become_one_commit() {
    let mut sim = sim(|_| {});
    sim.world.put_doc("docs/prd.md", b"# PRD\n");
    let notes = b"# Notes\n\nThe export keeps the column order.\n".to_vec();
    let id = conversation(
        &sim,
        vec![DraftTurn::WriteAll(vec![
            (PATH.into(), good()),
            ("docs/notes/export.md".into(), notes.clone()),
        ])],
    );
    awaiting(&mut sim, id, 1);
    let changes = sim.block_on(sim.engine().spec_draft_changes(id)).unwrap();
    let listed: Vec<(&str, bool)> = changes
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.added))
        .collect();
    assert_eq!(listed, [("docs/notes/export.md", true), (PATH, true)]);
    assert!(changes.passed);

    commit(&sim, id, Some("docs: add the export spec and its notes")).unwrap();
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded, "{:?}", d.draft);
    let committed = events(&sim, "workspace.doc.committed");
    assert_eq!(committed.len(), 2);
    assert_eq!(
        committed[0].payload["commit"],
        committed[1].payload["commit"]
    );
    let mut paths: Vec<_> = committed
        .iter()
        .map(|e| e.payload["path"].as_str().unwrap().to_string())
        .collect();
    paths.sort();
    assert_eq!(paths, ["docs/notes/export.md", PATH]);
    assert_eq!(sim.world.doc("docs/notes/export.md").unwrap(), notes);
    assert_eq!(sim.world.doc(PATH).unwrap(), good());
    assert_eq!(
        d.draft.commit_sha.as_deref(),
        Some(sim.world.base_head().as_str())
    );
    let spec = sim.spec_of(d.draft.spec_id.unwrap());
    assert_eq!(spec.path, PATH);
    assert_eq!(spec.origin, SpecOrigin::Architect);
}

#[test]
fn a_commit_without_a_change_is_refused() {
    let mut sim = sim(|_| {});
    sim.world.put_doc(PATH, &good());
    let id = {
        sim.world.script_draft(vec![DraftTurn::Nothing]);
        sim.revise_session(PATH, IDEA).unwrap().draft.id
    };
    awaiting(&mut sim, id, 1);
    assert_eq!(rejected(commit(&sim, id, None)), Rejection::DraftNoChanges);
    assert_eq!(sim.draft(id).draft.state, SpecDraftState::AwaitingOperator);
}

#[test]
fn a_file_that_moved_on_the_base_blocks_the_commit_and_a_file_that_did_not_does_not() {
    let mut sim = sim(|_| {});
    sim.world.put_doc("docs/prd.md", b"# PRD v1\n");
    sim.world.put_doc("docs/other.md", b"# Other v1\n");
    let turn = DraftTurn::WriteAll(vec![
        (PATH.into(), good()),
        ("docs/prd.md".into(), b"# PRD v1\n\nMine.\n".to_vec()),
    ]);
    let id = conversation(&sim, vec![turn.clone()]);
    awaiting(&mut sim, id, 1);
    sim.world.put_doc("docs/other.md", b"# Other v2\n");
    let before = sim.world.doc_commits().len();
    commit(&sim, id, None).unwrap();
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    assert_eq!(sim.world.doc_commits().len(), before + 2);
    assert_eq!(sim.world.doc("docs/other.md").unwrap(), b"# Other v2\n");
    assert_eq!(
        sim.world.doc("docs/prd.md").unwrap(),
        b"# PRD v1\n\nMine.\n"
    );

    let mut sim = sim_with_prd();
    let id = conversation(&sim, vec![turn]);
    awaiting(&mut sim, id, 1);
    sim.world
        .put_doc("docs/prd.md", b"# PRD v2 by someone else\n");
    let refused = rejected(commit(&sim, id, None));
    assert_eq!(
        refused,
        Rejection::DraftBaseMoved {
            paths: vec!["docs/prd.md".into()]
        }
    );
    assert_eq!(refused.params(), json!({ "paths": ["docs/prd.md"] }));
    assert_eq!(sim.draft(id).draft.state, SpecDraftState::AwaitingOperator);
    assert!(sim.world.doc(PATH).is_none());
    discard(&sim, id);
    assert_eq!(
        finished(&mut sim, id).draft.state,
        SpecDraftState::Cancelled
    );
}

fn sim_with_prd() -> Sim {
    let sim = sim(|_| {});
    sim.world.put_doc("docs/prd.md", b"# PRD v1\n");
    sim
}

#[test]
fn a_file_that_moves_between_the_check_and_the_push_fails_the_draft_without_overwriting() {
    let mut sim = sim_with_prd();
    let id = conversation(
        &sim,
        vec![DraftTurn::WriteAll(vec![
            (PATH.into(), good()),
            ("docs/prd.md".into(), b"# PRD v1\n\nMine.\n".to_vec()),
        ])],
    );
    awaiting(&mut sim, id, 1);
    commit(&sim, id, None).unwrap();
    sim.world
        .put_doc("docs/prd.md", b"# PRD v2 by someone else\n");
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Failed);
    let (code, params) = failure(&d);
    assert_eq!(code, "base_moved");
    assert_eq!(params["paths"], json!(["docs/prd.md"]));
    assert_eq!(
        sim.world.doc("docs/prd.md").unwrap(),
        b"# PRD v2 by someone else\n"
    );
    assert!(sim.world.doc(PATH).is_none());
    assert!(!saka_core::draft::can_retry(&d));
}

#[test]
fn revising_an_approved_spec_records_a_new_revision_and_leaves_the_running_step_alone() {
    let mut sim = sim(|cfg| {
        cfg.max_active_tasks = 1;
        cfg.operator = Operator {
            start: true,
            ..Operator::default()
        };
    });
    let first_text = approval_spec(
        "",
        &[("Build the base", "AC-01"), ("Build the rest", "AC-02")],
    );
    sim.world.put_doc(APPROVAL_PATH, first_text.as_bytes());
    let first = sim.register(APPROVAL_PATH, &first_text);
    assert!(first.lint_ok, "{:?}", first.issues);
    sim.approve_spec(first.spec_id, first.revision_id).unwrap();
    let state = |s: &Sim| s.tasks_of(first.spec_id)[0].item.state;
    assert!(sim.run_until(STEPS, |s| state(s) == WorkItemState::InStage));
    let tasks_before = sim.tasks_of(first.spec_id).len();

    let revised = approval_spec(
        "",
        &[
            ("Build the base", "AC-01"),
            ("Build the rest, differently", "AC-02"),
            ("Document it", "AC-01"),
        ],
    );
    sim.world.script_draft(vec![DraftTurn::Write {
        path: APPROVAL_PATH.into(),
        bytes: revised.clone().into_bytes(),
    }]);
    let id = sim
        .revise_session(APPROVAL_PATH, "Add a documentation step.")
        .unwrap()
        .draft
        .id;
    run_to(&mut sim, id, |d| {
        d.draft.state == SpecDraftState::AwaitingOperator
    });
    let packet = sim
        .world
        .prompts()
        .into_iter()
        .find(|p| p.contains("Revise the existing spec"))
        .unwrap();
    assert!(packet.contains(&format!(
        "Target file: {APPROVAL_PATH} (it already exists in your checkout"
    )));
    commit(&sim, id, None).unwrap();
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded, "{:?}", d.draft);

    let spec = sim.spec_of(first.spec_id);
    assert_eq!(d.draft.spec_id, Some(first.spec_id));
    assert_ne!(spec.latest_revision_id, first.revision_id);
    assert_eq!(spec.approved_revision_id, Some(first.revision_id));
    let revision = sim
        .engine()
        .store()
        .read(|c| read::load_revision(c, spec.latest_revision_id))
        .unwrap()
        .unwrap();
    assert_eq!(revision.content, revised.as_bytes());
    assert_eq!(sim.tasks_of(first.spec_id).len(), tasks_before);
    assert_eq!(
        sim.tasks_of(first.spec_id)[0].item.spec_revision_id,
        Some(first.revision_id)
    );
    assert_eq!(state(&sim), WorkItemState::InStage);
}

#[test]
fn a_revision_needs_an_existing_target() {
    let sim = sim(|_| {});
    let missing = match sim.revise_session(PATH, IDEA).unwrap_err() {
        EngineError::Rejected(r) => r,
        other => panic!("{other:?}"),
    };
    assert_eq!(missing, Rejection::FileNotFound { path: PATH.into() });
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

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn crashes_around_a_multi_file_commit_leave_one_commit_with_both_files(points in proptest::collection::vec(0usize..60, 1..4)) {
        let mut sim = sim(|_| {});
        let notes = b"# Notes\n".to_vec();
        let id = conversation(
            &sim,
            vec![DraftTurn::WriteAll(vec![
                (PATH.into(), good()),
                ("docs/notes/export.md".into(), notes.clone()),
            ])],
        );
        awaiting(&mut sim, id, 1);
        let head = sim.world.base_head();
        commit(&sim, id, None).unwrap();
        let mut points = points;
        points.sort_unstable();
        let mut step = 0;
        for point in points {
            while step < point && !sim.draft(id).draft.state.is_finished() {
                sim.step();
                step += 1;
            }
            sim.crash_mid_job();
        }
        let d = finished(&mut sim, id);
        prop_assert_eq!(d.draft.state, SpecDraftState::Succeeded);
        prop_assert_eq!(sim.world.doc(PATH), Some(good()));
        prop_assert_eq!(sim.world.doc("docs/notes/export.md"), Some(notes));
        prop_assert!(events(&sim, "workspace.doc.committed").len() <= 2);
        let commits: std::collections::BTreeSet<_> = events(&sim, "workspace.doc.committed")
            .iter()
            .map(|e| e.payload["commit"].clone().to_string())
            .collect();
        prop_assert!(commits.len() <= 1);
        prop_assert_ne!(sim.world.base_head(), head);
        let live: Vec<_> = sim.draft_sessions(id).into_iter().filter(|s| s.state.is_live()).collect();
        prop_assert!(live.is_empty());
    }
}
