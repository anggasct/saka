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

    async fn stop(&self, session_id: SessionId, runner_unit: Option<&str>) -> PortResult<()>;
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
