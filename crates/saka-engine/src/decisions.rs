use async_trait::async_trait;
use saka_core::decision::{DecisionOption, Question};
use saka_core::ids::{DecisionRequestId, SessionId};
use saka_core::redact::Redactor;
use saka_core::rejection::Rejection;
use saka_core::session::SessionInput;
use saka_ports::{AskRefusal, Asked, Decisions};
use saka_store::read;

use crate::action::Action;
use crate::{Cause, Engine, EngineError, Extra, Result, Shows};

pub const AGENT_ACTOR: &str = "agent:decision";

fn redacted(question: Question, redactor: &Redactor) -> Question {
    let clean = |text: &str| redactor.text(text).into_owned();
    Question {
        question: clean(&question.question),
        options: question
            .options
            .iter()
            .map(|o| DecisionOption {
                label: clean(&o.label),
                detail: o.detail.as_deref().map(clean),
            })
            .collect(),
        context: question.context.as_deref().map(clean),
    }
}

fn refusal(error: &EngineError) -> AskRefusal {
    match error {
        EngineError::Rejected(Rejection::DecisionOnePerSession) => AskRefusal::OnePerSession,
        EngineError::Rejected(Rejection::DecisionLimitReached) => AskRefusal::LimitReached,
        EngineError::Rejected(Rejection::Invalid { .. }) => AskRefusal::InvalidInput,
        EngineError::Rejected(Rejection::InvalidState { .. } | Rejection::NotFound) => {
            AskRefusal::SessionNotRunning
        }
        _ => AskRefusal::Unavailable,
    }
}

impl Engine {
    pub fn answer_decision(
        &self,
        id: DecisionRequestId,
        option: Option<u32>,
        note: Option<String>,
        cause: &Cause,
    ) -> Result<()> {
        self.answer_decision_with(id, option, note, cause, None)
            .map(|_| ())
    }

    pub fn answer_decision_with(
        &self,
        id: DecisionRequestId,
        option: Option<u32>,
        note: Option<String>,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<saka_core::event::Event>> {
        let owner = self
            .store
            .read(|c| read::decision_owner(c, id))?
            .ok_or(EngineError::Rejected(Rejection::DecisionNotFound))?;
        let input = SessionInput::AnswerDecision {
            decision_id: id,
            option,
            note,
            actor: crate::OPERATOR.to_string(),
        };
        let extra = Extra {
            action: action.map(|a| (a, Shows::Decision(id))),
            ..Extra::default()
        };
        self.decide_owner(owner, input, cause, extra)
    }
}

#[async_trait]
impl Decisions for Engine {
    async fn ask(
        &self,
        session: SessionId,
        question: Question,
    ) -> std::result::Result<Asked, AskRefusal> {
        let owner = self
            .store
            .read(|c| read::session_owner(c, session))
            .map_err(|_| AskRefusal::Unavailable)?
            .ok_or(AskRefusal::SessionNotRunning)?;
        let redactor = self
            .store
            .read(|c| read::session_project(c, session))
            .map_err(|_| AskRefusal::Unavailable)?
            .ok_or(AskRefusal::SessionNotRunning)
            .and_then(|project| {
                self.project_redactor(project).map_err(|e| {
                    tracing::warn!(session_id = %session, code = %e.code, "decision request failed");
                    AskRefusal::Unavailable
                })
            })?;
        let decision_id = DecisionRequestId(self.new_id());
        let input = SessionInput::AskDecision {
            decision_id,
            question: redacted(question, &redactor),
        };
        let cause = self.cause(AGENT_ACTOR);
        match self.decide_owner(owner, input, &cause, Extra::default()) {
            Ok(_) => Ok(Asked { decision_id }),
            Err(error) => {
                let refusal = refusal(&error);
                if refusal == AskRefusal::Unavailable {
                    tracing::warn!(session_id = %session, error = %error, "decision request failed");
                }
                Err(refusal)
            }
        }
    }
}
