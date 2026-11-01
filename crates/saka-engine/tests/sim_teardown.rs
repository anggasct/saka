#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{EnvironmentState, SpecState, WorkItemState};
use saka_testkit::{Sim, SimConfig};

const STEPS: usize = 8_000;
const HOUR_MS: i64 = 3_600_000;

fn environment_state(sim: &Sim, step: u32) -> Option<EnvironmentState> {
    sim.task(step).environment.map(|e| e.state)
}

#[test]
fn a_teardown_that_keeps_failing_never_holds_the_spec_and_is_cleaned_up_later() {
    let mut sim = Sim::new(SimConfig::default());
    sim.world.refuse_teardowns(true);
    sim.approve();

    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.spec().state, SpecState::Shipped);
    for step in [1, 2] {
        let task = sim.task(step);
        assert_eq!(task.item.state, WorkItemState::Done, "step {step}");
        assert!(task.open_blocker().is_none(), "step {step}");
        assert_eq!(
            environment_state(&sim, step),
            Some(EnvironmentState::TeardownFailed),
            "step {step}"
        );
    }
    assert_eq!(sim.stats().teardowns, 0);

    sim.world.refuse_teardowns(false);
    sim.clock.advance(HOUR_MS);
    assert!(sim.run_until(STEPS, |s| {
        [1, 2]
            .iter()
            .all(|n| environment_state(s, *n) == Some(EnvironmentState::TornDown))
    }));
    assert_eq!(sim.stats().teardowns, 2);
    assert_eq!(sim.task(1).item.state, WorkItemState::Done);
    assert_eq!(sim.task(2).item.state, WorkItemState::Done);
}
