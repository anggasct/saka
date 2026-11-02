#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{BlockerCode, ContextOrigin, WorkItemState};
use saka_store::read;
use saka_testkit::{APPROVAL_PATH, CiScript, Operator, Sim, SimConfig, approval_spec};

const STEPS: usize = 8_000;

fn linked_spec() -> String {
    approval_spec(
        "",
        &[("Build the base", "AC-01"), ("Finish the rest", "AC-02")],
    )
    .replace(
        "# Approval\n",
        "# Approval\n\nSee [the architecture](../architecture.md).\n",
    )
}

fn s1_task(sim: &Sim, spec: saka_core::ids::SpecId) -> saka_core::model::Task {
    sim.tasks_of(spec).remove(0)
}

#[test]
fn the_first_turn_is_the_packet_of_the_pinned_revision_and_survives_doc_changes() {
    let mut sim = Sim::new(SimConfig {
        max_active_tasks: 1,
        ci: CiScript::Fail(vec!["test".into()]),
        operator: Operator {
            retry: vec![BlockerCode::SessionLost, BlockerCode::CiRed],
            ..Operator::default()
        },
        ..SimConfig::default()
    });
    sim.world
        .put_doc("docs/architecture.md", b"# Architecture v1\n");
    sim.world
        .put_doc("docs/glossary.md", b"---\ncontext: always\n---\n# Terms\n");
    let registered = sim.register(APPROVAL_PATH, &linked_spec());
    assert!(registered.lint_ok, "{:?}", registered.issues);
    let commit = sim
        .engine()
        .store()
        .read(|c| read::load_revision(c, registered.revision_id))
        .unwrap()
        .unwrap()
        .source_commit;
    sim.approve_spec(registered.spec_id, registered.revision_id)
        .unwrap();
    let spec = registered.spec_id;

    assert!(sim.run_until(STEPS, |s| !s.world.prompts().is_empty()));
    let first = sim.world.prompts()[0].clone();
    assert!(first.starts_with("<documents>\n<document index=\"1\">\n"));
    assert!(first.contains(&format!(
        "<source>docs/specs/approval.md @ {commit}</source>"
    )));
    assert!(first.contains(&format!(
        "<document index=\"2\">\n<source>docs/architecture.md @ {commit}</source>\n<document_content>\n# Architecture v1\n</document_content>"
    )));
    assert!(first.contains(&format!(
        "<document index=\"3\">\n<source>docs/glossary.md @ {commit}</source>"
    )));
    assert!(first.contains("Task: approval/s1 — Build the base\n"));

    let s1 = s1_task(&sim, spec).item.id;
    let (_, packet) = sim.engine().work_packet(s1).unwrap();
    assert_eq!(packet.text, first);
    let origins: Vec<Option<ContextOrigin>> = packet.documents.iter().map(|d| d.origin).collect();
    assert_eq!(
        origins,
        vec![None, Some(ContextOrigin::Link), Some(ContextOrigin::Always)]
    );

    sim.world
        .put_doc("docs/architecture.md", b"# Architecture v2\n");
    let changed = sim.register(APPROVAL_PATH, &linked_spec());
    assert_ne!(changed.revision_id, registered.revision_id);
    let (_, after_change) = sim.engine().work_packet(s1).unwrap();
    assert_eq!(after_change.text, packet.text);

    assert!(sim.run_until(STEPS, |s| s1_task(s, spec).open_blocker().is_some()));
    sim.world.set_ci(CiScript::Pass);
    assert!(sim.run_until(STEPS, |s| s1_task(s, spec).item.state
        == WorkItemState::Done));
    let prompts = sim.world.prompts();
    assert!(
        prompts.iter().any(|p| p.contains("CI failed on ")),
        "the retry names the failed checks"
    );
    for p in prompts.iter().filter(|p| p.contains("Task: approval/s1")) {
        assert!(p.contains("# Architecture v1\n"));
        assert!(!p.contains("# Architecture v2"));
    }
    assert_eq!(
        s1_task(&sim, spec).item.spec_revision_id,
        Some(registered.revision_id)
    );
}

#[test]
fn a_task_without_a_spec_revision_has_no_work_packet() {
    let sim = Sim::new(SimConfig::default());
    sim.approve_spec(sim.spec_id, sim.revision_id).unwrap();
    let mut external = sim.tasks()[1].clone();
    external.item.id = saka_core::ids::WorkItemId(uuid::Uuid::from_u128(0xe1));
    external.item.origin = saka_core::vocab::WorkItemOrigin::ExternalChange;
    external.item.spec_id = None;
    external.item.spec_revision_id = None;
    external.item.step_id = None;
    external.item.step_index = None;
    external.item.task_key = "external/1".into();
    external.item.branch = "feature/outside".into();
    let mut batch = saka_store::Batch::new(sim.engine().now());
    batch.tasks.push(external.clone());
    sim.engine().store().commit(&batch).unwrap();

    match sim.engine().work_packet(external.item.id) {
        Err(saka_engine::EngineError::Rejected(r)) => {
            assert_eq!(r.code(), "work_packet_unavailable");
            assert_eq!(
                r.params(),
                serde_json::json!({ "reason": "not_a_spec_task" })
            );
        }
        other => panic!("{other:?}"),
    }
}
