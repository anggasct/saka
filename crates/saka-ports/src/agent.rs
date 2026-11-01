//! Agent sessions: a runner per session drives the agent CLI. The engine starts and stops
//! sessions and sends them turns and permission answers; what the runner reports comes
//! back as [`RunnerEvent`]s, which the binary hands to the engine in order.

use async_trait::async_trait;
use saka_core::ids::{SessionId, WorkItemId};
use saka_core::time::Timestamp;
use serde_json::Value;

use crate::PortResult;
use crate::env::EnvRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLaunch {
    pub session_id: SessionId,
    pub work_item_id: WorkItemId,
    pub environment: EnvRef,
}

/// What a started session runs under.
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

/// A started session, as the calls that talk to its runner need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    pub session_id: SessionId,
    pub runner_unit: String,
    pub socket_path: String,
}

#[async_trait]
pub trait Agents: Send + Sync {
    /// Starts the runner for the session. A runner left over from an earlier attempt with
    /// the same session id is stopped first.
    async fn start(&self, launch: &SessionLaunch) -> PortResult<LaunchedSession>;

    /// Sends a user turn. The runner ignores a turn number it already has.
    async fn send_turn(&self, session: &SessionRef, turn: u32, text: &str) -> PortResult<()>;

    /// Delivers a permission answer. The runner answers the CLI once per request id.
    async fn answer_permission(
        &self,
        session: &SessionRef,
        request_id: &str,
        allow: bool,
        message: Option<&str>,
    ) -> PortResult<()>;

    async fn interrupt(&self, session: &SessionRef) -> PortResult<()>;

    /// Shuts the session down and stops its runner unit; a unit that is gone is a success.
    async fn stop(&self, session_id: SessionId, runner_unit: Option<&str>) -> PortResult<()>;
}

/// One line of a session's `events.ndjson`, already redacted.
#[derive(Debug, Clone, PartialEq)]
pub struct RunnerEvent {
    pub session_id: SessionId,
    /// 0-based line number in `events.ndjson`.
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
