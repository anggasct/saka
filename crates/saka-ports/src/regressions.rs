use async_trait::async_trait;
use saka_core::ids::SessionId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requested {
    Recorded { attempt: u32, commit_sha: String },
    DirtyWorktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegressionRefusal {
    #[error("tool_unavailable")]
    ToolUnavailable,
    #[error("limit_reached")]
    LimitReached,
    #[error("already_reproduced")]
    AlreadyReproduced,
    #[error("check_pending")]
    Pending,
    #[error("session_not_running")]
    SessionNotRunning,
    #[error("unavailable")]
    Unavailable,
}

impl RegressionRefusal {
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            RegressionRefusal::ToolUnavailable => "tool_unavailable",
            RegressionRefusal::LimitReached => "limit_reached",
            RegressionRefusal::AlreadyReproduced => "already_reproduced",
            RegressionRefusal::Pending => "check_pending",
            RegressionRefusal::SessionNotRunning => "session_not_running",
            RegressionRefusal::Unavailable => "unavailable",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<RegressionRefusal> {
        [
            RegressionRefusal::ToolUnavailable,
            RegressionRefusal::LimitReached,
            RegressionRefusal::AlreadyReproduced,
            RegressionRefusal::Pending,
            RegressionRefusal::SessionNotRunning,
            RegressionRefusal::Unavailable,
        ]
        .into_iter()
        .find(|r| r.code() == code)
    }
}

#[async_trait]
pub trait Regressions: Send + Sync {
    async fn request(&self, session: SessionId) -> Result<Requested, RegressionRefusal>;
}
