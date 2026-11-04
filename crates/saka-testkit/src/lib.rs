#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test support fails loudly on a broken setup"
)]

mod accounts;
mod clock;
pub mod diagnostics;
pub mod fake_claude;
pub mod fixtures;
pub mod logs;
mod releases;
mod sim;
mod world;

pub use accounts::{
    ACCESS_TOKEN_SECS, AccountCall, DEVICE_VERIFICATION_URI, DeviceAnswer, FakeAccounts,
    GitHubUser, REFRESH_TOKEN_SECS,
};
pub use clock::{FakeClock, FakeIds};
pub use fixtures::{
    AFTER_GRAPH_PATH, AFTER_GRAPH_SPEC, APPROVAL_PATH, approval_spec, one_step_spec, repo_entry,
    saka_toml,
};
pub use releases::FakeReleases;
pub use sim::{
    DOCS_REPO, Layout, Operator, SECOND_INTEGRATION, SECOND_WEB_URL, SHOP_REPO, Sim, SimConfig,
    SimOutcome, Step, WEB_REPO, ZERO_SHA256, draft_config,
};
pub use world::{
    APP_CREDENTIAL, AgentOutcome, AgentScript, CiScript, CommandScript, DocsCheckoutState,
    DocsTurn, DraftTurn, FAKE_CAPABILITIES, FAKE_INTEGRATION, FAKE_SIGNATURE_HEADER, FakeWorld,
    FileEdit, HOME_REPO, Launch, Pr, Prepared, RecipeRun, SIM_RUNNER_VERSION, ScmCall, ScopeCheck,
    Stats, ToolchainScript, TurnUsage, fake_signature, git_blob,
};
