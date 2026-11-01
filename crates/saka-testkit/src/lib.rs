//! Fake ports, a virtual clock, and the loop simulation harness for tests.
//!
//! [`FakeWorld`] is one in-memory world behind every adapter port: a git remote with a
//! commit graph, worktrees, change requests with CI, and scripted agent sessions. It
//! counts every effect, so tests can assert that nothing happened twice. [`Sim`] drives a
//! real engine on a real SQLite file against that world, one step at a time.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test support fails loudly on a broken setup"
)]

mod clock;
pub mod fixtures;
mod sim;
mod world;

pub use clock::{FakeClock, FakeIds};
pub use sim::{Operator, Sim, SimConfig, SimOutcome, Step};
pub use world::{AgentOutcome, AgentScript, CiScript, FakeWorld, Stats};
