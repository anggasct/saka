use serde_json::Value;

use crate::ids::SessionId;
use crate::model::{Coded, Task};
use crate::vocab::{BlockerCode, ResumeCause, ResumeReason, SessionState, Trigger};

#[derive(Debug, Clone, PartialEq)]
pub struct ResumePlan {
    pub from: SessionId,
    pub cause: ResumeCause,
    pub reason: ResumeReason,
    pub cli_session_id: Option<String>,
    pub retry_of: Option<Coded>,
}

pub fn plan(task: &Task) -> Option<ResumePlan> {
    let run = task.stage_run.as_ref()?;
    let previous = task.session.as_ref().filter(|s| !s.state.is_live())?;
    let env = task.ready_environment()?;
    if previous.stage_run_id == run.id {
        return (previous.state == SessionState::Lost && !run.resumed_lost).then(|| ResumePlan {
            from: previous.id,
            cause: ResumeCause::Lost,
            reason: ResumeReason::RunnerLost,
            cli_session_id: previous.cli_session_id.clone(),
            retry_of: None,
        });
    }
    let blocker = task.blocker.as_ref().filter(|b| b.resolution.is_some());
    if run.trigger != Trigger::Manual || previous.environment_id != env.id {
        return None;
    }
    let cli_session_id = previous.cli_session_id.clone()?;
    Some(ResumePlan {
        from: previous.id,
        cause: ResumeCause::Retry,
        reason: blocker.map_or(ResumeReason::Retried, |b| reason_of(b.code, &b.params)),
        cli_session_id: Some(cli_session_id),
        retry_of: blocker.map(|b| Coded {
            code: b.code.as_str().to_string(),
            params: b.params.clone(),
        }),
    })
}

pub fn wants_session(task: &Task) -> bool {
    let (Some(run), None) = (&task.stage_run, task.live_session()) else {
        return false;
    };
    match &task.session {
        Some(s) if s.stage_run_id == run.id => {
            plan(task).is_some_and(|p| p.cause == ResumeCause::Lost)
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
