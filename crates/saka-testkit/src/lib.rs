#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test support fails loudly on a broken setup"
)]

mod clock;
pub mod fake_claude;
pub mod fixtures;
pub mod logs;
mod sim;
mod world;

pub use clock::{FakeClock, FakeIds};
pub use fixtures::{
    AFTER_GRAPH_PATH, AFTER_GRAPH_SPEC, APPROVAL_PATH, approval_spec, one_step_spec, repo_entry,
    saka_toml,
};
pub use sim::{
    DOCS_REPO, Layout, Operator, SHOP_REPO, Sim, SimConfig, SimOutcome, Step, WEB_REPO,
    ZERO_SHA256, draft_config,
};
pub use world::{
    AgentOutcome, AgentScript, CiScript, CommandScript, DraftTurn, FAKE_CAPABILITIES,
    FAKE_INTEGRATION, FAKE_SIGNATURE_HEADER, FakeWorld, HOME_REPO, Launch, Prepared, RecipeRun,
    SIM_RUNNER_VERSION, Stats, ToolchainScript, TurnUsage, fake_signature, git_blob,
};
