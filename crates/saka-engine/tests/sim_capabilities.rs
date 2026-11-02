#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{SpecState, WorkItemState};
use saka_ports::{Scm, ScmCapabilities};
use saka_testkit::{FAKE_CAPABILITIES, Sim, SimConfig};

const STEPS: usize = 8_000;

fn shipped(keep_branches: bool) -> Sim {
    let mut cfg = SimConfig::default();
    cfg.loop_cfg.delete_branch_after_merge = !keep_branches;
    let mut sim = Sim::new(cfg);
    sim.world.set_capabilities(ScmCapabilities {
        delete_branch_after_merge: !keep_branches,
        ..FAKE_CAPABILITIES
    });
    sim.approve_spec(sim.spec_id, sim.revision_id).unwrap();
    assert!(sim.run_until(STEPS, |s| s.spec_of(s.spec_id).state == SpecState::Shipped));
    assert!(
        sim.tasks()
            .iter()
            .all(|t| t.item.state == WorkItemState::Done)
    );
    sim
}

fn delete_jobs(sim: &Sim) -> i64 {
    sim.engine()
        .store()
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM job WHERE kind = 'scm.delete_branch'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

#[test]
fn branches_are_deleted_only_when_the_provider_does_that() {
    let deleting = shipped(false);
    assert_eq!(delete_jobs(&deleting), 2);
    assert_eq!(deleting.stats().branches_deleted, 2);

    let keeping = shipped(true);
    assert_eq!(delete_jobs(&keeping), 0);
    assert_eq!(keeping.stats().branches_deleted, 0);
}

#[test]
fn an_unknown_integration_has_no_capabilities() {
    let sim = Sim::new(SimConfig::default());
    assert_eq!(sim.world.capabilities("github"), Some(FAKE_CAPABILITIES));
    assert_eq!(sim.world.capabilities("gitlab"), None);
}
