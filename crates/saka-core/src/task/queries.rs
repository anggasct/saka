use std::collections::BTreeMap;

use serde_json::Value;

use super::recovery::retry_target;
use super::{
    AUTO_RETRY_DELAYS_MS, AUTO_RETRY_PREFIX, LoopConfig, MANUAL_RETRY_LIMIT, TaskInput,
    can_takeover, decide,
};
use crate::Ctx;
use crate::autonomy::Ceilings;
use crate::event::Prompt;
use crate::external;
use crate::gate::GatePolicy;
use crate::model::{Blocker, Coded, GateResult, Retries, RunOwner, Session, Task};
use crate::session::{RunRetry, RunView};
use crate::time::Timestamp;
use crate::vocab::{
    Autonomy, BlockerCode, ChangeState, GateOutcome, Stage, StagePhase, Trigger, WorkItemOrigin,
    WorkItemState,
};

pub fn merge_ready(task: &Task) -> bool {
    let reviewed = task
        .change_request
        .as_ref()
        .is_some_and(|cr| !task.has_review_stage() || task.approved_at(&cr.head_sha));
    task.is_at(Stage::Merge, StagePhase::AwaitingMerge)
        && gates_clear(task)
        && reviewed
        && !task.is_queued()
}

pub fn merge_allowed(task: &Task, head_sha: &str) -> bool {
    task.change_request.as_ref().is_some_and(|cr| {
        cr.head_sha == head_sha
            && gates_clear(task)
            && (!task.has_review_stage() || task.approved_at(head_sha))
    })
}

pub fn auto_merge_enabled(task: &Task, ceilings: &Ceilings) -> bool {
    task.item.origin == WorkItemOrigin::SpecStep
        && task.autonomy(ceilings) == Autonomy::Autonomous
        && task.workflow.stage_trigger(Stage::Merge) == Some(Trigger::Auto)
        && task.has_review_stage()
}

pub fn update_merge(task: &Task) -> bool {
    task.external.is_some()
        && task
            .change_request
            .as_ref()
            .is_some_and(|cr| cr.update_head_sha.as_ref() == Some(&cr.head_sha))
        && merge_ready(task)
}

pub fn auto_merge(task: &Task, ceilings: &Ceilings) -> bool {
    auto_merge_enabled(task, ceilings) && merge_ready(task)
}

pub fn gate_policy(task: &Task) -> GatePolicy {
    let policy = GatePolicy::from_recipe(task.environment.as_ref().and_then(|e| e.recipe.as_ref()));
    if task.external.is_some() {
        policy.without_saka()
    } else {
        policy
    }
}

pub fn gates_clear(task: &Task) -> bool {
    let Some(cr) = &task.change_request else {
        return false;
    };
    let at_head: Vec<&GateResult> = task
        .gates
        .iter()
        .filter(|g| g.head_sha == cr.head_sha)
        .collect();
    !at_head.is_empty()
        && at_head.iter().all(|g| g.outcome != GateOutcome::Failed)
        && at_head.iter().any(|g| g.outcome == GateOutcome::Passed)
}

pub fn can_retry(task: &Task) -> bool {
    task.item.state == WorkItemState::Blocked
        && task.open_blocker().is_some_and(|b| {
            retry_target(task, b.code, &b.params).is_some_and(|(stage, _)| {
                task.retries.get(&stage).map_or(0, |r| r.manual) < MANUAL_RETRY_LIMIT
            })
        })
}

pub fn available_actions(task: &Task) -> Vec<&'static str> {
    let mut actions = Vec::new();
    if task.item.state == WorkItemState::AwaitingStart {
        actions.push("start");
    }
    if merge_ready(task) {
        actions.push("merge");
    }
    if can_retry(task) {
        actions.push("retry");
    }
    if matches!(
        task.item.state,
        WorkItemState::Queued
            | WorkItemState::AwaitingStart
            | WorkItemState::Admitted
            | WorkItemState::InStage
            | WorkItemState::Blocked
            | WorkItemState::TakenOver
    ) {
        actions.push("cancel");
    }
    if can_takeover(task) {
        actions.push("takeover");
    }
    if task.item.state == WorkItemState::TakenOver {
        actions.push("return");
    }
    if external::awaiting_request(task) {
        actions.push("review");
    }
    if external::fix_available(task) {
        actions.push("fix");
    }
    actions
}

pub fn count_retry(
    retries: &mut BTreeMap<Stage, Retries>,
    stage: Stage,
    trigger: Trigger,
    reason: Option<&str>,
) {
    let entry = retries.entry(stage).or_default();
    match trigger {
        Trigger::Manual => {
            entry.manual += 1;
            entry.auto = 0;
        }
        Trigger::Auto if reason.is_some_and(|r| r.starts_with(AUTO_RETRY_PREFIX)) => {
            entry.auto += 1;
        }
        Trigger::Auto => {}
    }
}

pub fn auto_retryable(code: BlockerCode, params: &Value) -> bool {
    let text = |key: &str| params.get(key).and_then(Value::as_str);
    match code {
        BlockerCode::SessionLost | BlockerCode::ScmUnavailable => true,
        BlockerCode::EnvSetupFailed => !matches!(
            params.pointer("/error/code").and_then(Value::as_str),
            Some("setup_command_failed" | "docs_commit_missing" | "docs_checkout_too_large")
        ),
        BlockerCode::MergeConflict | BlockerCode::DocsMergeFailed => {
            text("reason") == Some("head_changed")
        }
        BlockerCode::CiRed => {
            text("source") == Some("saka") && text("reason") == Some("runner_failed")
        }
        BlockerCode::DodFailed => text("reason") == Some("runner_failed"),
        _ => false,
    }
}

pub fn auto_retry_due(task: &Task, now: Timestamp) -> bool {
    let Some(blocker) = task.open_blocker() else {
        return false;
    };
    if task.item.state != WorkItemState::Blocked || !auto_retryable(blocker.code, &blocker.params) {
        return false;
    }
    let Some((stage, _)) = retry_target(task, blocker.code, &blocker.params) else {
        return false;
    };
    let done = task.retries.get(&stage).map_or(0, |r| r.auto);
    let Some(delay) = usize::try_from(done)
        .ok()
        .and_then(|i| AUTO_RETRY_DELAYS_MS.get(i))
    else {
        return false;
    };
    now.since(blocker.opened_at) >= *delay
}

pub fn docs_pending(task: &Task) -> Option<String> {
    let pushed = task
        .environment
        .as_ref()
        .and_then(|e| e.docs.as_ref())
        .and_then(|d| d.pushed_sha.as_ref());
    task.stage_run
        .as_ref()
        .and_then(|r| r.docs_head_sha.clone())
        .filter(|h| Some(h) != pushed)
}

pub fn needs_docs_change(task: &Task) -> bool {
    task.docs_change.is_none()
        && task
            .environment
            .as_ref()
            .and_then(|e| e.docs.as_ref())
            .is_some_and(|d| d.pushed_sha.is_some())
}

pub fn docs_open(task: &Task) -> bool {
    task.docs_change
        .as_ref()
        .is_some_and(|c| c.state == ChangeState::Open && c.skipped_at.is_none())
}

pub fn watches_docs_change(task: &Task) -> bool {
    docs_open(task)
        && matches!(
            task.item.state,
            WorkItemState::Blocked | WorkItemState::InStage
        )
}

pub fn watches_change(task: &Task) -> bool {
    let open = task
        .change_request
        .as_ref()
        .is_some_and(|c| c.state == ChangeState::Open);
    open && match task.item.state {
        WorkItemState::InStage if task.item.origin == WorkItemOrigin::ExternalChange => true,
        WorkItemState::Blocked => true,
        WorkItemState::InStage => {
            task.item.stage == Some(Stage::Dev)
                || task.item.stage == Some(Stage::Fix)
                || task.item.stage == Some(Stage::Review)
                || task.is_at(Stage::Merge, StagePhase::AwaitingCi)
                || task.is_at(Stage::Merge, StagePhase::AwaitingMerge)
                || task.is_at(Stage::Merge, StagePhase::Queued)
                || task.is_at(Stage::Merge, StagePhase::Integrating)
                || task.is_at(Stage::Merge, StagePhase::Merging)
        }
        _ => false,
    }
}

pub fn needs_tick(task: &Task, cfg: &LoopConfig, now: Timestamp) -> bool {
    struct Probe;
    impl crate::ids::IdSource for Probe {
        fn next_id(&mut self) -> uuid::Uuid {
            uuid::Uuid::nil()
        }
    }
    let mut ids = Probe;
    let mut ctx = Ctx { now, ids: &mut ids };
    decide(task, TaskInput::Tick, cfg, &mut ctx).is_ok_and(|events| !events.is_empty())
}

pub fn run_view(task: &Task) -> Option<RunView<'_>> {
    let run = task.stage_run.as_ref()?;
    let blocker = task.blocker.as_ref();
    let coded = |b: &Blocker| Coded {
        code: b.code.as_str().to_string(),
        params: b.params.clone(),
    };
    let owner = RunOwner::StageRun {
        work_item_id: task.item.id,
        stage_run_id: run.id,
    };
    let reviewing = run.stage == Stage::Review;
    // A reviewer never inherits another run's conversation: only its own session is visible.
    let session = if reviewing {
        task.session.as_ref().filter(|s| s.owner == owner)
    } else {
        // And the agent that wrote the change never resumes the reviewer's conversation.
        std::iter::once(task.session.as_ref())
            .flatten()
            .chain(task.retired_sessions.iter().rev())
            .find(|s| !reviewer_session(task, s))
    };
    let fixing = run.fix.as_ref();
    let fix_cause = fixing.map(|f| {
        (
            f.cause.code.parse::<BlockerCode>().ok(),
            f.cause.params.clone(),
        )
    });
    Some(RunView {
        owner,
        project_id: task.item.project_id,
        session,
        permissions: &task.permissions,
        decisions: &task.decisions,
        prior_usage: task.prior_usage,
        budget: run.budget,
        budget_warnings: &run.budget_warnings,
        resumed_lost: run.resumed_lost,
        ready_environment: task.ready_environment().map(|e| e.id),
        retry: RunRetry {
            trigger: run.trigger,
            retried: match fix_cause {
                Some((Some(code), params)) => Some((code, params)),
                _ => blocker
                    .filter(|b| b.resolution.is_some())
                    .map(|b| (b.code, b.params.clone())),
            },
            follows_up: fixing.is_some(),
        },
        packet: if reviewing {
            Prompt::ReviewPacket
        } else {
            Prompt::WorkPacket {
                retry_of: fixing.map(|f| f.cause.clone()).or_else(|| {
                    blocker
                        .filter(|_| run.trigger == Trigger::Manual)
                        .map(coded)
                }),
            }
        },
    })
}

pub(super) fn reviewer_session(task: &Task, session: &Session) -> bool {
    let Some(run_id) = session.owner.stage_run_id() else {
        return false;
    };
    task.stage_run
        .iter()
        .chain(task.retired_runs.iter())
        .any(|r| r.id == run_id && r.stage == Stage::Review)
}

pub fn merge_status(reason: &str) -> u16 {
    match reason {
        "head_changed" => 409,
        "rejected" => 422,
        _ => 405,
    }
}
