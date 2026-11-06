use async_trait::async_trait;
use saka_core::decision::Question;
use saka_core::ids::{DecisionRequestId, SessionId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asked {
    pub decision_id: DecisionRequestId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AskRefusal {
    #[error("one_open_per_session")]
    OnePerSession,
    #[error("limit_reached")]
    LimitReached,
    #[error("invalid_input")]
    InvalidInput,
    #[error("session_not_running")]
    SessionNotRunning,
    #[error("unavailable")]
    Unavailable,
}

impl AskRefusal {
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            AskRefusal::OnePerSession => "one_open_per_session",
            AskRefusal::LimitReached => "limit_reached",
            AskRefusal::InvalidInput => "invalid_input",
            AskRefusal::SessionNotRunning => "session_not_running",
            AskRefusal::Unavailable => "unavailable",
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<AskRefusal> {
        [
            AskRefusal::OnePerSession,
            AskRefusal::LimitReached,
            AskRefusal::InvalidInput,
            AskRefusal::SessionNotRunning,
            AskRefusal::Unavailable,
        ]
        .into_iter()
        .find(|r| r.code() == code)
    }
}

#[async_trait]
pub trait Decisions: Send + Sync {
    async fn ask(&self, session: SessionId, question: Question) -> Result<Asked, AskRefusal>;
}
