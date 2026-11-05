use async_trait::async_trait;
use saka_core::rejection::Rejection;
use saka_core::vocab::BlockerAction;
use saka_ports::{ChatAction, ChatActionKind, ChatActions, ChatOutcome};
use saka_store::read;

use crate::{Engine, EngineError};

impl Engine {
    pub fn chat_action(&self, action: &ChatAction) -> ChatOutcome {
        let outcome = self.run_chat_action(action);
        self.counters_chat(action.platform, outcome);
        outcome
    }

    fn run_chat_action(&self, action: &ChatAction) -> ChatOutcome {
        let item = match self
            .store
            .read(|c| read::blocker_work_item(c, action.blocker_id))
        {
            Ok(Some(item)) => item,
            Ok(None) => return ChatOutcome::Gone,
            Err(e) => {
                tracing::error!(error = %e, "chat action could not read the blocker");
                return ChatOutcome::Failed;
            }
        };
        let kind = match action.kind {
            ChatActionKind::Retry => BlockerAction::Retry,
            ChatActionKind::Cancel => BlockerAction::Cancel,
        };
        let cause = self.cause(&format!("operator:{}:{}", action.platform, action.user_id));
        let why = format!("{} chat", action.platform);
        match self.resolve_blocker_with(item, action.blocker_id, Some(kind), &why, &cause, None) {
            Ok(_) => ChatOutcome::Done,
            Err(EngineError::Rejected(Rejection::BlockerNotOpen { .. })) => ChatOutcome::Gone,
            Err(EngineError::Rejected(Rejection::BlockerActionChanged { .. })) => {
                ChatOutcome::Changed
            }
            Err(EngineError::Rejected(Rejection::ActionNeedsInput { .. })) => {
                ChatOutcome::NeedsInput
            }
            Err(e) => {
                tracing::warn!(error = %e, "chat action failed");
                ChatOutcome::Failed
            }
        }
    }

    fn counters_chat(&self, platform: &str, outcome: ChatOutcome) {
        let label = match outcome {
            ChatOutcome::Done => "done",
            ChatOutcome::Gone => "gone",
            ChatOutcome::Changed => "changed",
            ChatOutcome::NeedsInput => "needs_input",
            ChatOutcome::Failed => "failed",
        };
        self.counters.chat_action(platform, label);
    }
}

#[async_trait]
impl ChatActions for Engine {
    async fn act(&self, action: &ChatAction) -> ChatOutcome {
        self.chat_action(action)
    }
}
