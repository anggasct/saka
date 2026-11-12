use serde_json::Value;

use crate::ids::SessionId;
use crate::model::Coded;
use crate::session::RunView;
use crate::vocab::{BlockerCode, ResumeCause, ResumeReason, SessionState, Trigger};

#[derive(Debug, Clone, PartialEq)]
pub struct ResumePlan {
    pub from: SessionId,
    pub cause: ResumeCause,
    pub reason: ResumeReason,
    pub cli_session_id: Option<String>,
    pub retry_of: Option<Coded>,
}

pub fn plan(view: &RunView<'_>) -> Option<ResumePlan> {
    let previous = view.session.filter(|s| !s.state.is_live())?;
    let env = view.ready_environment?;
    if previous.owner == view.owner {
        return (previous.state == SessionState::Lost && !view.resumed_lost).then(|| ResumePlan {
            from: previous.id,
            cause: ResumeCause::Lost,
            reason: ResumeReason::RunnerLost,
            cli_session_id: previous.cli_session_id.clone(),
            retry_of: None,
        });
    }
    if !(view.retry.trigger == Trigger::Manual || view.retry.follows_up)
        || previous.environment_id != env
    {
        return None;
    }
    let cli_session_id = previous.cli_session_id.clone()?;
    let retried = view.retry.retried.as_ref();
    Some(ResumePlan {
        from: previous.id,
        cause: ResumeCause::Retry,
        reason: retried.map_or(ResumeReason::Retried, |(code, params)| {
            reason_of(*code, params)
        }),
        cli_session_id: Some(cli_session_id),
        retry_of: retried.map(|(code, params)| Coded {
            code: code.as_str().to_string(),
            params: params.clone(),
        }),
    })
}

pub fn wants_session(view: &RunView<'_>) -> bool {
    if view.live_session().is_some()
        || view.asked_decision().is_some()
        || view.pending_regression().is_some()
    {
        return false;
    }
    match view.session {
        Some(s) if s.owner == view.owner => {
            plan(view).is_some_and(|p| p.cause == ResumeCause::Lost)
        }
        _ => true,
    }
}

pub fn reason_of(code: BlockerCode, params: &Value) -> ResumeReason {
    match code {
        BlockerCode::SessionLost => ResumeReason::RunnerLost,
        BlockerCode::SessionFailed
            if params.get("cause").and_then(Value::as_str) == Some("operator_stop") =>
        {
            ResumeReason::OperatorStopped
        }
        BlockerCode::SessionTimeout => ResumeReason::IdleTimeout,
        BlockerCode::PermissionExpired => ResumeReason::PermissionExpired,
        BlockerCode::DecisionExpired => ResumeReason::DecisionExpired,
        BlockerCode::BudgetExhausted => ResumeReason::BudgetExhausted,
        BlockerCode::QuotaExhausted => ResumeReason::QuotaExhausted,
        _ => ResumeReason::Retried,
    }
}

pub fn text(reason: ResumeReason) -> String {
    let cause = match reason {
        ResumeReason::RunnerLost => "the runner was lost",
        ResumeReason::OperatorStopped => "the operator stopped it",
        ResumeReason::IdleTimeout => "the operator interrupted it and did not answer in time",
        ResumeReason::PermissionExpired => "a permission request expired",
        ResumeReason::DecisionExpired => "a question you asked was not answered in time",
        ResumeReason::BudgetExhausted => "its budget ran out",
        ResumeReason::QuotaExhausted => "the agent account hit its rate limit",
        ResumeReason::Retried => "the operator sent the step back for another attempt",
    };
    format!(
        "Your previous session stopped ({cause}). Whatever you were running when it stopped did not finish and will not finish on its own.\n\
         Check `git status` and the last commits, then continue the step from where the work stands. The step, its acceptance criteria, and its Done when commands are unchanged.\n\
         Commit all your changes with a Conventional Commits message before you finish. Do not push."
    )
}
