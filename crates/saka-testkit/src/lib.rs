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
pub use sim::{Operator, Sim, SimConfig, SimOutcome, Step};
pub use world::{AgentOutcome, AgentScript, CiScript, FakeWorld, Stats};
