#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::SpecState;
use saka_engine::{DocWriteView, EngineError, Precondition};
use saka_testkit::fixtures::{TWO_STEP_PATH, TWO_STEP_SPEC};
use saka_testkit::{Sim, SimConfig, one_step_spec};

fn save(sim: &Sim, path: &str, content: &str, precondition: Precondition) -> DocWriteView {
    try_save(sim, path, content, precondition).unwrap()
}

fn try_save(
    sim: &Sim,
    path: &str,
    content: &str,
    precondition: Precondition,
) -> Result<DocWriteView, EngineError> {
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    sim.block_on(engine.write_doc(
        sim.project_id,
        path,
        content.to_string(),
        precondition,
        &cause,
        None,
    ))
}

fn revisions(sim: &Sim) -> i64 {
    sim.engine()
        .store()
        .read(|c| Ok(c.query_row("SELECT count(*) FROM spec_revision", [], |r| r.get(0))?))
        .unwrap()
}

fn issue_codes(view: &DocWriteView) -> Vec<String> {
    let issues = &view.spec.as_ref().unwrap().revision.issues;
    issues
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn saving_a_spec_records_a_revision_of_exactly_the_saved_bytes() {
    let sim = Sim::new(SimConfig::default());
    let text = one_step_spec("Saved from the editor", "");
    let before = revisions(&sim);
    let saved = save(&sim, "docs/specs/saved.md", &text, Precondition::Absent);
    assert!(saved.committed && saved.created);
    let spec = saved.spec.as_ref().unwrap();
    assert_eq!(spec.revision.source_commit, saved.commit);
    assert_eq!(
        spec.revision.content_hash,
        saka_spec::content_hash(text.as_bytes())
    );
    assert!(spec.revision.lint_ok, "{}", spec.revision.issues);
    assert_eq!(spec.spec.state, SpecState::Ready);
    assert_eq!(saved.spec_problem, None);
    assert_eq!(revisions(&sim), before + 1);

    let again = save(&sim, "docs/specs/saved.md", &text, Precondition::Exists);
    assert!(!again.committed);
    assert_eq!(again.commit, saved.commit);
    assert_eq!(again.spec.as_ref().unwrap().revision.id, spec.revision.id);
    assert_eq!(revisions(&sim), before + 1);
}

#[test]
fn a_plain_doc_is_committed_without_a_spec() {
    let sim = Sim::new(SimConfig::default());
    let before = revisions(&sim);
    let saved = save(&sim, "docs/notes.md", "# Notes\n", Precondition::Absent);
    assert!(saved.committed);
    assert_eq!((saved.spec, saved.spec_problem), (None, None));
    assert_eq!(revisions(&sim), before);
}

#[test]
fn a_registered_spec_saved_as_plain_text_gets_a_revision_that_says_so() {
    let sim = Sim::new(SimConfig::default());
    sim.world.put_doc(TWO_STEP_PATH, TWO_STEP_SPEC.as_bytes());
    let saved = save(
        &sim,
        TWO_STEP_PATH,
        "# Just notes now\n",
        Precondition::Exists,
    );
    assert!(saved.committed);
    assert_ne!(saved.spec.as_ref().unwrap().revision.id, sim.revision_id);
    assert_eq!(issue_codes(&saved), ["not_a_spec"]);
}

#[test]
fn a_slug_taken_by_another_spec_keeps_the_commit_and_explains_why() {
    let sim = Sim::new(SimConfig::default());
    let saved = save(
        &sim,
        "docs/elsewhere/tasks-done.md",
        TWO_STEP_SPEC,
        Precondition::Absent,
    );
    assert!(saved.committed);
    assert_eq!(saved.spec, None);
    assert_eq!(saved.spec_problem.unwrap().code, "spec_slug_conflict");
    assert_eq!(
        sim.world.doc("docs/elsewhere/tasks-done.md").unwrap(),
        TWO_STEP_SPEC.as_bytes()
    );
}

#[test]
fn changing_an_approved_spec_shows_the_plan_for_the_new_revision() {
    let mut sim = Sim::new(SimConfig::default());
    sim.approve();
    sim.world.put_doc(TWO_STEP_PATH, TWO_STEP_SPEC.as_bytes());
    let changed = TWO_STEP_SPEC.replace("# Mark tasks as done", "# Mark tasks as finished");
    assert_ne!(changed, TWO_STEP_SPEC);
    let saved = save(&sim, TWO_STEP_PATH, &changed, Precondition::Exists);
    let spec = saved.spec.unwrap();
    assert_eq!(spec.spec.state, SpecState::Approved);
    assert_eq!(spec.approved_revision.unwrap().id, sim.revision_id);
    assert!(spec.revision_plan.is_some());
}

#[test]
fn a_save_sent_again_after_a_restart_is_unchanged_and_still_registers() {
    let mut sim = Sim::new(SimConfig::default());
    let text = one_step_spec("Survives a restart", "");
    let first = save(&sim, "docs/specs/restart.md", &text, Precondition::Absent);
    sim.restart();
    let again = save(&sim, "docs/specs/restart.md", &text, Precondition::Absent);
    assert!(!again.committed);
    assert_eq!(again.commit, first.commit);
    assert_eq!(
        again.spec.unwrap().revision.id,
        first.spec.unwrap().revision.id
    );
    assert!(matches!(
        try_save(&sim, "docs/specs/restart.md", "x", Precondition::Absent),
        Err(EngineError::Rejected(_))
    ));
}
