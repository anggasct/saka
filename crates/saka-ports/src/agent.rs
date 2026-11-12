use async_trait::async_trait;
use saka_core::authorship::GitIdentity;
use saka_core::diagnostics::{AgentCli, AgentLogin};
use saka_core::ids::SessionId;
use saka_core::model::RunOwner;
use saka_core::time::Timestamp;
use serde_json::Value;

use crate::PortResult;
use crate::env::EnvRef;

pub const MAX_RESULT_TEXT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchProfile {
    Task,
    Architect { cwd: String },
    Reviewer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLaunch {
    pub session_id: SessionId,
    pub owner: RunOwner,
    pub environment: EnvRef,
    pub resume_cli_session_id: Option<String>,
    pub profile: LaunchProfile,
    pub model: Option<String>,
    pub author: GitIdentity,
    pub regression_check: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchedSession {
    pub cli: String,
    pub cli_version: Option<String>,
    pub model: Option<String>,
    pub account: String,
    pub runner_unit: String,
    pub socket_path: String,
    pub log_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    pub session_id: SessionId,
    pub runner_unit: String,
    pub socket_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reattached {
    pub runner_version: String,
    pub next_offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Open { silent_ms: i64 },
    Closed { generation: u32 },
}

#[async_trait]
pub trait Agents: Send + Sync {
    async fn start(&self, launch: &SessionLaunch) -> PortResult<LaunchedSession>;

    async fn send_turn(&self, session: &SessionRef, turn: u32, text: &str) -> PortResult<()>;

    async fn answer_permission(
        &self,
        session: &SessionRef,
        request_id: &str,
        allow: bool,
        message: Option<&str>,
    ) -> PortResult<()>;

    async fn interrupt(&self, session: &SessionRef) -> PortResult<()>;

    async fn steer(&self, session: &SessionRef, n: u32, text: &str) -> PortResult<()>;

    async fn reattach(&self, session: &SessionRef, from_offset: u64) -> PortResult<Reattached>;

    async fn unit_active(&self, session: &SessionRef) -> PortResult<bool>;

    fn link(&self, session: SessionId) -> Link;

    fn disconnect(&self, session: SessionId);

    async fn stop(&self, session_id: SessionId, runner_unit: Option<&str>) -> PortResult<()>;

    async fn agent_cli(&self) -> PortResult<AgentCli>;

    async fn agent_login(&self) -> PortResult<AgentLogin>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunnerEvent {
    pub session_id: SessionId,
    pub offset: u64,
    pub at: Timestamp,
    pub kind: RunnerEventKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunnerEventKind {
    CliStarted {
        pid: Option<u32>,
        cli_version: Option<String>,
    },
    SessionInit {
        cli_session_id: String,
        model: Option<String>,
    },
    Activity {
        category: String,
        summary: String,
        tool: Option<String>,
        is_error: Option<bool>,
    },
    Steer {
        n: u32,
        turn: u32,
        text: String,
    },
    PermissionRequest {
        request_id: String,
        tool: String,
        input: Value,
    },
    PermissionDelivered {
        request_id: String,
        behavior: String,
    },
    RateLimit {
        info: Value,
    },
    TurnEnded {
        turn: u32,
        status: String,
        cost_usd_micros: Option<i64>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        result_summary: Option<String>,
        result_text: Option<String>,
    },
    CliExited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    RunnerError {
        code: String,
        params: Value,
    },
}
