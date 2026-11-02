#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test support fails loudly on a broken setup"
)]

mod clock;
pub mod fake_claude;
pub mod fixtures;
mod sim;
mod world;

pub use clock::{FakeClock, FakeIds};
pub use fixtures::{
    AFTER_GRAPH_PATH, AFTER_GRAPH_SPEC, APPROVAL_PATH, approval_spec, one_step_spec,
};
pub use sim::{Operator, Sim, SimConfig, SimOutcome, Step};
pub use world::{
    AgentOutcome, AgentScript, CiScript, CommandScript, FakeWorld, RecipeRun, SIM_RUNNER_VERSION,
    Stats, TurnUsage,
};
