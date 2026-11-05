use async_trait::async_trait;
use saka_core::ids::BlockerId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatActionKind {
    Retry,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatAction {
    pub platform: &'static str,
    pub user_id: String,
    pub kind: ChatActionKind,
    pub blocker_id: BlockerId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatOutcome {
    Done,
    Gone,
    Changed,
    NeedsInput,
    Failed,
}

#[async_trait]
pub trait ChatActions: Send + Sync {
    async fn act(&self, action: &ChatAction) -> ChatOutcome;
}
