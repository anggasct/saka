#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::ids::{SpecId, SpecRevisionId};
use saka_core::rejection::Rejection;
use saka_core::vocab::{SpecState, WorkItemState};
use saka_engine::EngineError;
use saka_store::Batch;
use saka_testkit::{AFTER_GRAPH_PATH, AFTER_GRAPH_SPEC, Sim, SimConfig, one_step_spec};
use serde_json::{Value, json};

const STEPS: usize = 8_000;

fn sim(max_active_tasks: usize) -> Sim {
    Sim::new(SimConfig {
        max_active_tasks,
        ..SimConfig::default()
    })
}

fn count(sim: &Sim, sql: &str) -> i64 {
    sim.engine()
        .store()
        .read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

fn register(sim: &Sim, slug: &str, frontmatter: &str) -> (SpecId, SpecRevisionId) {
    let spec = sim.register(
        &format!("docs/specs/{slug}.md"),
        &one_step_spec(slug, frontmatter),
    );
    assert!(spec.lint_ok, "{slug}: {:?}", spec.issues);
    (spec.spec_id, spec.revision_id)
}

fn refused_issues(result: Result<Vec<saka_core::ids::WorkItemId>, EngineError>) -> Value {
    match result {
        Err(EngineError::Rejected(Rejection::SpecNotReady { issues })) => issues,
        other => panic!("expected spec_not_ready, got {other:?}"),
    }
}

#[test]
fn parallel_steps_run_together_and_the_join_builds_on_both() {
    let mut sim = sim(2);
    let spec = sim.register(AFTER_GRAPH_PATH, AFTER_GRAPH_SPEC);
    assert!(spec.lint_ok, "{:?}", spec.issues);
    assert_eq!(
        sim.approve_spec(spec.spec_id, spec.revision_id)
            .unwrap()
            .len(),
        4
    );
    assert_eq!(count(&sim, "SELECT COUNT(*) FROM work_item_dependency"), 4);

    let mut together = false;
    let mut early = false;
    let shipped = sim.run_until(STEPS, |s| {
        let tasks = s.tasks_of(spec.spec_id);
        let state = |n: usize| tasks[n - 1].item.state;
        if state(2) == WorkItemState::InStage && state(3) == WorkItemState::InStage {
            together = true;
        }
        let started = |n: usize| state(n) != WorkItemState::Queued;
        if (started(2) || started(3)) && state(1) != WorkItemState::Done {
            early = true;
        }
        if started(4) && (state(2) != WorkItemState::Done || state(3) != WorkItemState::Done) {
            early = true;
        }
        s.spec_of(spec.spec_id).state == SpecState::Shipped
    });
    assert!(shipped, "not shipped");
    assert!(together, "S2 and S3 never ran at the same time");
    assert!(
        !early,
        "a step started before the steps it waits for were done"
    );

    let tasks = sim.tasks_of(spec.spec_id);
    let merge = |n: usize| {
        tasks[n - 1]
            .change_request
            .as_ref()
            .unwrap()
            .merge_commit_sha
            .clone()
            .unwrap()
    };
    let s4_base = tasks[3]
        .environment
        .as_ref()
        .unwrap()
        .base_sha
        .clone()
        .unwrap();
    assert!(sim.world.is_ancestor(&merge(2), &s4_base));
    assert!(sim.world.is_ancestor(&merge(3), &s4_base));

    let prs = sim.world.prs();
    for n in 1..=4 {
        let branch = format!("saka/parallel/s{n}");
        assert_eq!(prs.iter().filter(|p| p.head_branch == branch).count(), 1);
    }
    assert_eq!(sim.stats().merges, 4);
}

#[test]
fn a_spec_waits_for_the_spec_it_depends_on_to_ship() {
    let mut sim = sim(1);
    let (a, a_rev) = register(&sim, "a", "");
    let (b, b_rev) = register(&sim, "b", "depends_on: [a]\npriority: 10\n");
    sim.approve_spec(b, b_rev).unwrap();
    sim.approve_spec(a, a_rev).unwrap();

    assert_eq!(
        count(&sim, "SELECT COUNT(*) FROM spec_dependency"),
        1,
        "approving b recorded its dependency"
    );
    assert_eq!(sim.spec_of(b).priority, 10);
    assert_eq!(sim.tasks_of(b)[0].item.priority, 10);

    sim.step();
    let waiting = sim.engine().metrics().unwrap().admission_waiting;
    assert_eq!(
        waiting,
        vec![("spec".to_string(), 1), ("step".to_string(), 0)]
    );

    let mut early = false;
    let shipped = sim.run_until(STEPS, |s| {
        let b_started = s.tasks_of(b)[0].item.state != WorkItemState::Queued;
        if b_started && s.spec_of(a).state != SpecState::Shipped {
            early = true;
        }
        s.spec_of(b).state == SpecState::Shipped
    });
    assert!(shipped, "b never shipped");
    assert!(!early, "b started before a shipped");
}

#[test]
fn the_more_important_spec_starts_first() {
    let mut sim = sim(1);
    let (low, low_rev) = register(&sim, "low", "");
    let (high, high_rev) = register(&sim, "high", "priority: 5\n");
    sim.approve_spec(low, low_rev).unwrap();
    sim.approve_spec(high, high_rev).unwrap();
    let started = sim.run_until(STEPS, |s| {
        s.tasks_of(high)[0].item.state != WorkItemState::Queued
    });
    assert!(started);
    assert_eq!(sim.tasks_of(low)[0].item.state, WorkItemState::Queued);
    assert!(sim.run_until(STEPS, |s| s.spec_of(low).state == SpecState::Shipped));
}

#[test]
fn missing_withdrawn_and_circular_dependencies_refuse_approve() {
    let sim = sim(1);
    let events = || count(&sim, "SELECT COUNT(*) FROM event");
    let graph = || count(&sim, "SELECT COUNT(*) FROM spec_dependency");

    let (lonely, lonely_rev) = register(&sim, "lonely", "depends_on: [nowhere]\n");
    let before = (events(), graph());
    assert_eq!(
        refused_issues(sim.approve_spec(lonely, lonely_rev)),
        json!([{"code": "depends_on_unknown", "severity": "error", "line": 3, "params": {"slug": "nowhere"}}])
    );
    assert_eq!((events(), graph()), before);

    let (gone, _) = register(&sim, "gone", "");
    let mut withdrawn = sim.spec_of(gone);
    withdrawn.state = SpecState::Withdrawn;
    let mut batch = Batch::new(withdrawn.updated_at);
    batch.specs.push(withdrawn);
    sim.engine().store().commit(&batch).unwrap();
    let (late, late_rev) = register(&sim, "late", "depends_on:\n  - gone\n");
    assert_eq!(
        refused_issues(sim.approve_spec(late, late_rev)),
        json!([{"code": "depends_on_withdrawn", "severity": "error", "line": 3, "params": {"slug": "gone"}}])
    );

    let (x, x_rev) = register(&sim, "x", "depends_on: [y]\n");
    let (y, y_rev) = register(&sim, "y", "depends_on: [x]\n");
    sim.approve_spec(y, y_rev).unwrap();
    let before = (events(), graph());
    assert_eq!(
        refused_issues(sim.approve_spec(x, x_rev)),
        json!([{"code": "depends_on_cycle", "severity": "error", "line": 3, "params": {"cycle": ["x", "y", "x"]}}])
    );
    assert_eq!((events(), graph()), before);

    let (p, p_rev) = register(&sim, "p", "depends_on: [q]\n");
    let (q, q_rev) = register(&sim, "q", "depends_on: [r]\n");
    let (r, r_rev) = register(&sim, "r", "depends_on: [p]\n");
    sim.approve_spec(q, q_rev).unwrap();
    sim.approve_spec(r, r_rev).unwrap();
    assert_eq!(
        refused_issues(sim.approve_spec(p, p_rev)),
        json!([{"code": "depends_on_cycle", "severity": "error", "line": 3, "params": {"cycle": ["p", "q", "r", "p"]}}])
    );
}

#[test]
fn a_revision_from_an_older_parser_is_read_again_and_cannot_be_approved() {
    let sim = sim(1);
    let text = one_step_spec("kept", "");
    let (kept, old) = sim.old_parser_spec("kept", &text);
    let events = count(&sim, "SELECT COUNT(*) FROM event");
    match sim.approve_spec(kept, old) {
        Err(EngineError::Rejected(r)) => {
            assert_eq!(r.code(), "spec_reparse_required");
            assert_eq!(
                r.params(),
                json!({"parser_version": 1, "current_parser_version": 2})
            );
        }
        other => panic!("expected spec_reparse_required, got {other:?}"),
    }
    assert_eq!(count(&sim, "SELECT COUNT(*) FROM event"), events);

    let again = sim.register("docs/specs/kept.md", &text);
    assert_eq!(again.spec_id, kept);
    assert_ne!(again.revision_id, old);
    let versions: Vec<i64> = sim
        .engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT parser_version FROM spec_revision WHERE spec_id = ?1 ORDER BY parser_version",
            )?;
            let rows = stmt.query_map([kept.0.to_string()], |r| r.get(0))?;
            Ok(rows.collect::<Result<Vec<i64>, _>>()?)
        })
        .unwrap();
    assert_eq!(versions, vec![1, 2]);
    assert_eq!(sim.spec_of(kept).latest_revision_id, again.revision_id);
    sim.approve_spec(kept, again.revision_id).unwrap();
}
