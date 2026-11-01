//! Task decisions: one work item from admission through `dev → merge → sync` to done.
//!
//! Facts from the outside (job results, runner events, poll results) that arrive late or
//! twice are ignored with no events. Commands from people are rejected with a reason.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::Ctx;
use crate::event::{Event, Prompt};
use crate::gate::{self, Check};
use crate::ids::{
    BlockerId, ChangeRequestId, EnvironmentId, GateResultId, PermissionRequestId, SessionId,
    StageRunId,
};
use crate::job::JobKind;
use crate::model::{
    Blocker, ChangeRequest, Coded, Environment, GateResult, PermissionRequest, Session, StageRun,
    Task, WorkItem,
};
use crate::policy::{Decision, Effect};
use crate::rejection::Rejection;
use crate::time::Timestamp;
use crate::vocab::{
    BlockerCode, BlockerResolution, ChangeState, EnvironmentState, GateOutcome, GateSource,
    MergeMethod, PermissionState, SessionState, Stage, StagePhase, StageRunState, Trigger,
    WorkItemOrigin, WorkItemState,
};

/// Operator limits the decisions apply.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopConfig {
    pub max_followup_turns: u32,
    pub max_running_ms: i64,
    pub ci_appear_timeout_ms: i64,
    pub poll_interval_ms: i64,
    pub merge_method: MergeMethod,
}

/// Pending permission requests become `expired` with this source when their session ends.
pub const SESSION_ENDED_SOURCE: &str = "system:session_ended";
const DENIED_BY_OPERATOR: &str = "Denied by the operator.";

#[derive(Debug, Clone, PartialEq)]
pub enum TaskInput {
    /// The scheduler picked this task.
    Admit,
    EnvironmentPrepared {
        environment_id: EnvironmentId,
        root_path: String,
        worktree_path: String,
        base_sha: String,
    },
    SessionLaunched {
        session_id: SessionId,
        cli: String,
        cli_version: Option<String>,
        model: Option<String>,
        account: String,
        runner_unit: String,
        socket_path: String,
        log_path: String,
    },
    SessionInitialized {
        cli_session_id: String,
        model: Option<String>,
    },
    /// The runner accepted a requested turn.
    TurnAccepted {
        turn: u32,
    },
    PermissionRequested {
        permission_request_id: PermissionRequestId,
        cli_request_id: String,
        tool: String,
        summary: String,
        input: Value,
        decision: Decision,
    },
    AnswerPermission {
        permission_request_id: PermissionRequestId,
        allow: bool,
        message: Option<String>,
        actor: String,
    },
    PermissionDelivered {
        permission_request_id: PermissionRequestId,
    },
    TurnEnded {
        turn: u32,
        status: String,
        cost_usd_micros: Option<i64>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
    },
    WorktreeInspected {
        turn: u32,
        dirty: bool,
        commits_ahead: u32,
        head_sha: String,
    },
    CliExited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    /// Timers: running time and CI appearance.
    Tick,
    /// The server started; sessions it did not reattach to are lost.
    ServerRestarted,
    BranchPushed {
        sha: String,
    },
    PushRejected {
        remote_sha: String,
        local_sha: String,
    },
    ChangeOpened {
        change_request_id: ChangeRequestId,
        number: u64,
        url: String,
        head_sha: String,
        base_branch: String,
    },
    ChangeObserved {
        state: ChangeState,
        head_sha: String,
        merge_commit_sha: Option<String>,
        merged_at: Option<Timestamp>,
        checks: Vec<Check>,
    },
    RequestMerge {
        expected_version: u64,
        reason: Option<String>,
    },
    MergeSucceeded {
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
    },
    MergeRejected {
        reason: String,
    },
    MergeVerified {
        found: bool,
    },
    EnvironmentTornDown,
    Retry {
        reason: String,
    },
    Cancel {
        reason: String,
    },
    JobExhausted {
        kind: JobKind,
        error: Coded,
    },
}

pub fn decide(
    task: &Task,
    input: TaskInput,
    cfg: &LoopConfig,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<Event>, Rejection> {
    if task.item.origin != WorkItemOrigin::SpecStep {
        return Err(Rejection::Unsupported);
    }
    let d = Decider {
        task,
        cfg,
        now: ctx.now,
    };
    use TaskInput as I;
    Ok(match input {
        I::Admit => d.admit(ctx),
        I::EnvironmentPrepared {
            environment_id,
            root_path,
            worktree_path,
            base_sha,
        } => d.environment_prepared(environment_id, root_path, worktree_path, base_sha),
        I::SessionLaunched {
            session_id,
            cli,
            cli_version,
            model,
            account,
            runner_unit,
            socket_path,
            log_path,
        } => d.session_launched(
            session_id,
            cli,
            cli_version,
            model,
            account,
            runner_unit,
            socket_path,
            log_path,
        ),
        I::SessionInitialized {
            cli_session_id,
            model,
        } => d.with_live_session(|s| {
            vec![Event::SessionInitialized {
                work_item_id: task.item.id,
                session_id: s.id,
                cli_session_id,
                model,
            }]
        }),
        I::TurnAccepted { turn } => d.turn_accepted(turn),
        I::PermissionRequested {
            permission_request_id,
            cli_request_id,
            tool,
            summary,
            input,
            decision,
        } => d.permission_requested(
            permission_request_id,
            cli_request_id,
            tool,
            summary,
            input,
            decision,
        ),
        I::AnswerPermission {
            permission_request_id,
            allow,
            message,
            actor,
        } => {
            return d.answer_permission(permission_request_id, allow, message, &actor);
        }
        I::PermissionDelivered {
            permission_request_id,
        } => d.permission_delivered(permission_request_id),
        I::TurnEnded {
            turn,
            status,
            cost_usd_micros,
            input_tokens,
            output_tokens,
        } => d.turn_ended(
            turn,
            status,
            cost_usd_micros,
            input_tokens,
            output_tokens,
            ctx,
        ),
        I::WorktreeInspected {
            turn,
            dirty,
            commits_ahead,
            head_sha,
        } => d.worktree_inspected(turn, dirty, commits_ahead, head_sha, ctx),
        I::CliExited { code, signal } => d.session_gone(
            "runner_exited",
            json!({ "code": code, "signal": signal }),
            ctx,
        ),
        I::ServerRestarted => d.session_gone("server_restart", json!({}), ctx),
        I::Tick => d.tick(ctx),
        I::BranchPushed { sha } => d.branch_pushed(sha, ctx),
        I::PushRejected {
            remote_sha,
            local_sha,
        } => {
            if task.is_at(Stage::Dev, StagePhase::Pushing) {
                d.block(
                    BlockerCode::PushRejected,
                    json!({ "remote_sha": remote_sha, "local_sha": local_sha }),
                    ctx,
                )
            } else {
                vec![]
            }
        }
        I::ChangeOpened {
            change_request_id,
            number,
            url,
            head_sha,
            base_branch,
        } => d.change_opened(change_request_id, number, url, head_sha, base_branch, ctx),
        I::ChangeObserved {
            state,
            head_sha,
            merge_commit_sha,
            merged_at,
            checks,
        } => d.change_observed(state, head_sha, merge_commit_sha, merged_at, checks, ctx),
        I::RequestMerge {
            expected_version,
            reason,
        } => return d.request_merge(expected_version, reason),
        I::MergeSucceeded {
            merge_commit_sha,
            merged_at,
        } => {
            if task.is_at(Stage::Merge, StagePhase::Merging) {
                d.merged(merge_commit_sha, merged_at, true, ctx)
            } else {
                vec![]
            }
        }
        I::MergeRejected { reason } => {
            if task.is_at(Stage::Merge, StagePhase::Merging) {
                let head = task.change_request.as_ref().map(|c| c.head_sha.clone());
                d.block(
                    BlockerCode::MergeConflict,
                    json!({ "reason": reason, "head_sha": head }),
                    ctx,
                )
            } else {
                vec![]
            }
        }
        I::MergeVerified { found } => d.merge_verified(found, ctx),
        I::EnvironmentTornDown => d.environment_torn_down(),
        I::Retry { reason } => return d.retry(reason, ctx),
        I::Cancel { reason } => return d.cancel(reason),
        I::JobExhausted { kind, error } => d.job_exhausted(kind, error, ctx),
    })
}

struct Decider<'a> {
    task: &'a Task,
    cfg: &'a LoopConfig,
    now: Timestamp,
}

impl Decider<'_> {
    fn item(&self) -> &WorkItem {
        &self.task.item
    }

    fn run_id(&self) -> Option<StageRunId> {
        self.task.stage_run.as_ref().map(|r| r.id)
    }

    fn next_attempt(&self, stage: Stage) -> u32 {
        self.task.attempts.get(&stage).copied().unwrap_or(0) + 1
    }

    fn new_run(
        &self,
        stage: Stage,
        trigger: Trigger,
        entry: StagePhase,
        reason: Option<String>,
        retry_of: Option<Coded>,
        ctx: &mut Ctx<'_>,
    ) -> Event {
        Event::StageRunCreated {
            work_item_id: self.item().id,
            stage_run_id: StageRunId::new(ctx.ids),
            stage,
            attempt: self.next_attempt(stage),
            trigger,
            entry_phase: entry,
            reason,
            retry_of,
        }
    }

    fn phase(&self, phase: StagePhase, head_sha: Option<String>) -> Vec<Event> {
        match self.run_id() {
            Some(stage_run_id) => {
                vec![Event::StagePhaseChanged {
                    work_item_id: self.item().id,
                    stage_run_id,
                    phase,
                    head_sha,
                }]
            }
            None => vec![],
        }
    }

    fn with_live_session(&self, f: impl FnOnce(&Session) -> Vec<Event>) -> Vec<Event> {
        self.task.live_session().map(f).unwrap_or_default()
    }

    fn session_state(&self, session: &Session, state: SessionState) -> Event {
        Event::SessionStateChanged {
            work_item_id: self.item().id,
            session_id: session.id,
            state,
            running_ms: session.running_ms_at(self.now),
        }
    }

    /// Ends the live session (if any) and expires its pending permission requests.
    fn end_session(&self, state: SessionState, code: &str, params: Value) -> Vec<Event> {
        let Some(session) = self.task.live_session() else {
            return vec![];
        };
        let mut events = vec![
            Event::SessionEnded {
                work_item_id: self.item().id,
                session_id: session.id,
                end_reason: Coded {
                    code: code.to_string(),
                    params,
                },
            },
            self.session_state(session, state),
        ];
        for p in self
            .task
            .permissions
            .iter()
            .filter(|p| p.state == PermissionState::Pending)
        {
            events.push(Event::PermissionAnswered {
                work_item_id: self.item().id,
                permission_request_id: p.id,
                state: PermissionState::Expired,
                answer_source: SESSION_ENDED_SOURCE.to_string(),
                message: None,
            });
        }
        events
    }

    fn block(&self, code: BlockerCode, params: Value, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if self.task.open_blocker().is_some() || self.item().state.is_terminal() {
            return vec![];
        }
        vec![Event::WorkItemBlocked {
            work_item_id: self.item().id,
            stage_run_id: self.run_id(),
            blocker_id: BlockerId::new(ctx.ids),
            code,
            params,
            stage: self
                .item()
                .stage
                .or(self.task.stage_run.as_ref().map(|r| r.stage)),
            phase: self
                .item()
                .stage_phase
                .or(self.task.stage_run.as_ref().map(|r| r.entry_phase)),
        }]
    }

    fn admit(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if self.item().state != WorkItemState::Queued {
            return vec![];
        }
        vec![
            Event::WorkItemAdmitted {
                work_item_id: self.item().id,
            },
            self.new_run(
                Stage::Dev,
                Trigger::Auto,
                StagePhase::PreparingEnvironment,
                None,
                None,
                ctx,
            ),
        ]
    }

    fn environment_prepared(
        &self,
        environment_id: EnvironmentId,
        root_path: String,
        worktree_path: String,
        base_sha: String,
    ) -> Vec<Event> {
        let waiting = self.item().state == WorkItemState::Admitted
            && self
                .task
                .stage_run
                .as_ref()
                .is_some_and(|r| r.state == StageRunState::Pending);
        if !waiting {
            return vec![];
        }
        let mut events = vec![Event::EnvironmentPrepared {
            work_item_id: self.item().id,
            environment_id,
            repo_id: self.item().repo_id,
            root_path,
            worktree_path,
            branch: self.item().branch.clone(),
            base_sha,
        }];
        events.extend(self.phase(StagePhase::RunningSession, None));
        events
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the launch result fields one to one"
    )]
    fn session_launched(
        &self,
        session_id: SessionId,
        cli: String,
        cli_version: Option<String>,
        model: Option<String>,
        account: String,
        runner_unit: String,
        socket_path: String,
        log_path: String,
    ) -> Vec<Event> {
        let (Some(run), Some(env)) = (&self.task.stage_run, self.task.ready_environment()) else {
            return vec![];
        };
        if !self.task.is_at(Stage::Dev, StagePhase::RunningSession)
            || self.task.live_session().is_some()
        {
            return vec![];
        }
        let retry_of = self
            .task
            .blocker
            .as_ref()
            .filter(|_| run.trigger == Trigger::Manual)
            .map(|b| Coded {
                code: b.code.as_str().to_string(),
                params: b.params.clone(),
            });
        vec![
            Event::SessionStarted {
                work_item_id: self.item().id,
                session_id,
                stage_run_id: run.id,
                environment_id: env.id,
                cli,
                cli_version,
                model,
                account,
                runner_unit,
                socket_path,
                log_path,
            },
            Event::SessionTurnRequested {
                work_item_id: self.item().id,
                session_id,
                turn: 1,
                prompt: Prompt::WorkPacket { retry_of },
            },
        ]
    }

    fn turn_accepted(&self, turn: u32) -> Vec<Event> {
        self.with_live_session(|s| {
            if s.turns == turn && matches!(s.state, SessionState::Starting | SessionState::Idle) {
                vec![self.session_state(s, SessionState::Running)]
            } else {
                vec![]
            }
        })
    }

    fn permission_requested(
        &self,
        permission_request_id: PermissionRequestId,
        cli_request_id: String,
        tool: String,
        summary: String,
        input: Value,
        decision: Decision,
    ) -> Vec<Event> {
        let Some(session) = self.task.live_session() else {
            return vec![];
        };
        if self
            .task
            .permissions
            .iter()
            .any(|p| p.cli_request_id == cli_request_id)
        {
            return vec![];
        }
        let mut events = vec![Event::PermissionRequested {
            work_item_id: self.item().id,
            session_id: session.id,
            permission_request_id,
            cli_request_id,
            tool,
            summary,
            input,
        }];
        let answered = match (decision.effect, decision.source.answer_source()) {
            (Effect::Allow, Some(source)) => Some((PermissionState::Allowed, source, None)),
            (Effect::Deny, Some(source)) => {
                let message = format!(
                    "Denied by Saka policy ({}).",
                    source.trim_start_matches("policy:")
                );
                Some((PermissionState::Denied, source, Some(message)))
            }
            _ => None,
        };
        match answered {
            Some((state, answer_source, message)) => events.push(Event::PermissionAnswered {
                work_item_id: self.item().id,
                permission_request_id,
                state,
                answer_source,
                message,
            }),
            None if session.state == SessionState::Running => {
                events.push(self.session_state(session, SessionState::Waiting))
            }
            None => {}
        }
        events
    }

    fn answer_permission(
        &self,
        id: PermissionRequestId,
        allow: bool,
        message: Option<String>,
        actor: &str,
    ) -> Result<Vec<Event>, Rejection> {
        let Some(request) = self.task.permissions.iter().find(|p| p.id == id) else {
            return Err(Rejection::NotFound);
        };
        if request.state != PermissionState::Pending {
            return Err(Rejection::PermissionAlreadyAnswered);
        }
        let mut events = vec![Event::PermissionAnswered {
            work_item_id: self.item().id,
            permission_request_id: id,
            state: if allow {
                PermissionState::Allowed
            } else {
                PermissionState::Denied
            },
            answer_source: format!("human:{actor}"),
            message: if allow {
                None
            } else {
                Some(message.unwrap_or_else(|| DENIED_BY_OPERATOR.to_string()))
            },
        }];
        let others_pending = self
            .task
            .permissions
            .iter()
            .any(|p| p.id != id && p.state == PermissionState::Pending);
        if let Some(session) = self.task.live_session()
            && session.state == SessionState::Waiting
            && !others_pending
        {
            events.push(Event::SessionStateChanged {
                work_item_id: self.item().id,
                session_id: session.id,
                state: SessionState::Running,
                running_ms: session.running_ms,
            });
        }
        Ok(events)
    }

    fn permission_delivered(&self, id: PermissionRequestId) -> Vec<Event> {
        match self.task.permissions.iter().find(|p| p.id == id) {
            Some(p)
                if p.delivered_at.is_none()
                    && matches!(p.state, PermissionState::Allowed | PermissionState::Denied) =>
            {
                vec![Event::PermissionDelivered {
                    work_item_id: self.item().id,
                    permission_request_id: id,
                }]
            }
            _ => vec![],
        }
    }

    fn turn_ended(
        &self,
        turn: u32,
        status: String,
        cost_usd_micros: Option<i64>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Some(session) = self.task.live_session() else {
            return vec![];
        };
        if session.turns != turn
            || !matches!(session.state, SessionState::Running | SessionState::Waiting)
        {
            return vec![];
        }
        let mut events = vec![Event::SessionTurnEnded {
            work_item_id: self.item().id,
            session_id: session.id,
            turn,
            status: status.clone(),
            cost_usd_micros,
            input_tokens,
            output_tokens,
        }];
        if status == "success" {
            events.push(self.session_state(session, SessionState::Idle));
        } else {
            events.extend(self.end_session(
                SessionState::Failed,
                "session_failed",
                json!({ "subtype": status }),
            ));
            events.extend(self.block(
                BlockerCode::SessionFailed,
                json!({ "subtype": status }),
                ctx,
            ));
        }
        events
    }

    fn worktree_inspected(
        &self,
        turn: u32,
        dirty: bool,
        commits_ahead: u32,
        head_sha: String,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let Some(session) = self.task.live_session() else {
            return vec![];
        };
        if !self.task.is_at(Stage::Dev, StagePhase::RunningSession)
            || session.turns != turn
            || session.state != SessionState::Idle
        {
            return vec![];
        }
        if !dirty && commits_ahead > 0 {
            let mut events = self.end_session(SessionState::Ended, "completed", json!({}));
            events.extend(self.phase(StagePhase::Pushing, Some(head_sha)));
            return events;
        }
        if session.followups < self.cfg.max_followup_turns {
            return vec![Event::SessionTurnRequested {
                work_item_id: self.item().id,
                session_id: session.id,
                turn: turn + 1,
                prompt: Prompt::Uncommitted {
                    dirty,
                    commits_ahead,
                },
            }];
        }
        let params = json!({ "dirty": dirty, "commits": commits_ahead });
        let mut events = self.end_session(SessionState::Ended, "no_changes", params.clone());
        events.extend(self.block(BlockerCode::NoChanges, params, ctx));
        events
    }

    fn session_gone(&self, cause: &str, params: Value, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Some(session) = self.task.live_session() else {
            return vec![];
        };
        let mut events = vec![Event::SessionLost {
            work_item_id: self.item().id,
            session_id: session.id,
            cause: cause.to_string(),
        }];
        let mut end = self.end_session(
            SessionState::Lost,
            "session_lost",
            json!({ "cause": cause, "detail": params }),
        );
        // `end_session` starts with `SessionEnded`; keep it, after the loss itself.
        events.append(&mut end);
        if self.task.is_at(Stage::Dev, StagePhase::RunningSession) {
            events.extend(self.block(BlockerCode::SessionLost, json!({ "cause": cause }), ctx));
        }
        events
    }

    fn tick(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if let Some(session) = self.task.live_session()
            && matches!(
                session.state,
                SessionState::Running | SessionState::Waiting | SessionState::Idle
            )
            && session.running_ms_at(self.now) > self.cfg.max_running_ms
        {
            let minutes = self.cfg.max_running_ms / 60_000;
            let mut events = self.end_session(
                SessionState::Interrupted,
                "session_timeout",
                json!({ "minutes": minutes }),
            );
            events.extend(self.block(
                BlockerCode::SessionTimeout,
                json!({ "minutes": minutes }),
                ctx,
            ));
            return events;
        }
        if self.task.is_at(Stage::Merge, StagePhase::AwaitingCi)
            && self.task.gate.as_ref().is_none_or(|g| g.checks.is_empty())
            && let Some(started) = self.task.stage_run.as_ref().and_then(|r| r.started_at)
            && self.now.since(started) > self.cfg.ci_appear_timeout_ms
        {
            let head = self
                .task
                .change_request
                .as_ref()
                .map(|c| c.head_sha.clone());
            return self.block(
                BlockerCode::CiRed,
                json!({ "reason": "no_checks", "head_sha": head }),
                ctx,
            );
        }
        vec![]
    }

    fn branch_pushed(&self, sha: String, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if !self.task.is_at(Stage::Dev, StagePhase::Pushing) {
            return vec![];
        }
        let mut events = vec![Event::BranchPushed {
            work_item_id: self.item().id,
            branch: self.item().branch.clone(),
            sha: sha.clone(),
        }];
        match &self.task.change_request {
            // The change already exists (a retried dev stage): it now shows the new head.
            Some(cr) if cr.state == ChangeState::Open => {
                if cr.head_sha != sha {
                    events.push(Event::ChangeRequestUpdated {
                        work_item_id: self.item().id,
                        change_request_id: cr.id,
                        head_sha: sha.clone(),
                    });
                }
                events.extend(self.dev_done(sha, ctx));
            }
            _ => events.extend(self.phase(StagePhase::OpeningChange, Some(sha))),
        }
        events
    }

    fn dev_done(&self, head_sha: String, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Some(run) = &self.task.stage_run else {
            return vec![];
        };
        vec![
            Event::StageRunSucceeded {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                head_sha: Some(head_sha),
            },
            self.new_run(
                Stage::Merge,
                Trigger::Auto,
                StagePhase::AwaitingCi,
                None,
                None,
                ctx,
            ),
        ]
    }

    fn change_opened(
        &self,
        id: ChangeRequestId,
        number: u64,
        url: String,
        head_sha: String,
        base_branch: String,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        if !self.task.is_at(Stage::Dev, StagePhase::OpeningChange) {
            return vec![];
        }
        let mut events = vec![Event::ChangeRequestOpened {
            work_item_id: self.item().id,
            change_request_id: id,
            repo_id: self.item().repo_id,
            number,
            url,
            head_branch: self.item().branch.clone(),
            base_branch,
            head_sha: head_sha.clone(),
        }];
        events.extend(self.dev_done(head_sha, ctx));
        events
    }

    fn change_observed(
        &self,
        state: ChangeState,
        head_sha: String,
        merge_commit_sha: Option<String>,
        merged_at: Option<Timestamp>,
        checks: Vec<Check>,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let in_merge = [
            StagePhase::AwaitingCi,
            StagePhase::AwaitingMerge,
            StagePhase::Merging,
        ]
        .iter()
        .any(|p| self.task.is_at(Stage::Merge, *p));
        let (Some(cr), Some(run)) = (&self.task.change_request, &self.task.stage_run) else {
            return vec![];
        };
        if !in_merge || cr.state != ChangeState::Open {
            return vec![];
        }
        match state {
            ChangeState::Merged => {
                let Some(sha) = merge_commit_sha else {
                    return vec![];
                };
                return self.merged(sha, merged_at, false, ctx);
            }
            ChangeState::Closed => {
                let mut events = vec![Event::ChangeRequestClosed {
                    work_item_id: self.item().id,
                    change_request_id: cr.id,
                }];
                events.extend(self.block(
                    BlockerCode::ChangeClosed,
                    json!({ "number": cr.number }),
                    ctx,
                ));
                return events;
            }
            ChangeState::Open => {}
        }
        let mut events = Vec::new();
        let head_changed = cr.head_sha != head_sha;
        if head_changed {
            events.push(Event::ChangeRequestUpdated {
                work_item_id: self.item().id,
                change_request_id: cr.id,
                head_sha: head_sha.clone(),
            });
        }
        let outcome = gate::outcome(&checks);
        let unchanged =
            self.task.gate.as_ref().is_some_and(|g| {
                g.head_sha == head_sha && g.outcome == outcome && g.checks == checks
            });
        if !unchanged {
            events.push(Event::GateObserved {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                gate_result_id: GateResultId::new(ctx.ids),
                head_sha: head_sha.clone(),
                outcome,
                checks: checks.clone(),
            });
        }
        if self.task.is_at(Stage::Merge, StagePhase::Merging) {
            return events;
        }
        let awaiting_merge = self.task.is_at(Stage::Merge, StagePhase::AwaitingMerge);
        match outcome {
            GateOutcome::Passed if !awaiting_merge || head_changed => {
                events.extend(self.phase(StagePhase::AwaitingMerge, Some(head_sha)));
            }
            GateOutcome::Passed => {}
            GateOutcome::Failed => {
                let failing: Vec<&str> = checks
                    .iter()
                    .filter(|c| c.is_failure())
                    .map(|c| c.name.as_str())
                    .collect();
                events.extend(self.block(
                    BlockerCode::CiRed,
                    json!({ "checks": failing, "head_sha": head_sha }),
                    ctx,
                ));
            }
            GateOutcome::Pending => {
                if awaiting_merge {
                    events.extend(self.phase(StagePhase::AwaitingCi, Some(head_sha)));
                }
            }
        }
        events
    }

    fn request_merge(
        &self,
        expected_version: u64,
        reason: Option<String>,
    ) -> Result<Vec<Event>, Rejection> {
        if expected_version != self.item().version {
            return Err(Rejection::VersionStale);
        }
        let in_merge =
            self.item().state == WorkItemState::InStage && self.item().stage == Some(Stage::Merge);
        if !in_merge || self.task.is_at(Stage::Merge, StagePhase::Merging) {
            return Err(Rejection::InvalidState {
                state: self.state_name(),
            });
        }
        let cr = self.task.change_request.as_ref();
        let gate = self.task.gate.as_ref();
        let ready = self.task.is_at(Stage::Merge, StagePhase::AwaitingMerge)
            && gate.is_some_and(|g| {
                g.outcome == GateOutcome::Passed && cr.is_some_and(|c| c.head_sha == g.head_sha)
            });
        let (Some(cr), true) = (cr, ready) else {
            return Err(Rejection::MergeNotReady {
                gate_outcome: gate.map(|g| g.outcome.as_str().to_string()),
                head_sha: cr.map(|c| c.head_sha.clone()),
            });
        };
        let mut events = vec![Event::MergeRequested {
            work_item_id: self.item().id,
            head_sha: cr.head_sha.clone(),
            merge_method: self.cfg.merge_method,
            reason,
        }];
        events.extend(self.phase(StagePhase::Merging, Some(cr.head_sha.clone())));
        Ok(events)
    }

    fn merged(
        &self,
        merge_commit_sha: String,
        merged_at: Option<Timestamp>,
        by_saka: bool,
        ctx: &mut Ctx<'_>,
    ) -> Vec<Event> {
        let (Some(cr), Some(run)) = (&self.task.change_request, &self.task.stage_run) else {
            return vec![];
        };
        vec![
            Event::ChangeRequestMerged {
                work_item_id: self.item().id,
                change_request_id: cr.id,
                merge_commit_sha: merge_commit_sha.clone(),
                merged_at,
                merged_by_saka: by_saka,
            },
            Event::StageRunSucceeded {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                head_sha: Some(cr.head_sha.clone()),
            },
            self.new_run(
                Stage::Sync,
                Trigger::Auto,
                StagePhase::Verifying,
                None,
                None,
                ctx,
            ),
        ]
    }

    fn merge_verified(&self, found: bool, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if !self.task.is_at(Stage::Sync, StagePhase::Verifying) {
            return vec![];
        }
        if found {
            return self.phase(StagePhase::TearingDown, None);
        }
        let cr = self.task.change_request.as_ref();
        self.block(
            BlockerCode::DodFailed,
            json!({
                "merge_commit_sha": cr.and_then(|c| c.merge_commit_sha.clone()),
                "base_branch": cr.map(|c| c.base_branch.clone()),
            }),
            ctx,
        )
    }

    fn environment_torn_down(&self) -> Vec<Event> {
        let Some(env) = self
            .task
            .environment
            .as_ref()
            .filter(|e| e.state != EnvironmentState::TornDown)
        else {
            return vec![];
        };
        let mut events = vec![Event::EnvironmentTornDown {
            work_item_id: self.item().id,
            environment_id: env.id,
        }];
        if self.task.is_at(Stage::Sync, StagePhase::TearingDown)
            && let (Some(run), Some(sha)) = (
                &self.task.stage_run,
                self.task
                    .change_request
                    .as_ref()
                    .and_then(|c| c.merge_commit_sha.clone()),
            )
        {
            events.push(Event::StageRunSucceeded {
                work_item_id: self.item().id,
                stage_run_id: run.id,
                head_sha: None,
            });
            events.push(Event::WorkItemDone {
                work_item_id: self.item().id,
                merge_commit_sha: sha,
            });
        }
        events
    }

    fn retry(&self, reason: String, ctx: &mut Ctx<'_>) -> Result<Vec<Event>, Rejection> {
        let blocked = self.item().state == WorkItemState::Blocked;
        let Some(blocker) = self.task.open_blocker().filter(|_| blocked) else {
            return Err(Rejection::InvalidState {
                state: self.state_name(),
            });
        };
        let Some((stage, mut entry)) = retry_entry(blocker.code, &blocker.params) else {
            return Err(Rejection::InvalidState {
                state: self.state_name(),
            });
        };
        // A new session needs a ready environment; a ready one is reused.
        let env_ready = self.task.ready_environment().is_some();
        if entry == StagePhase::RunningSession && !env_ready {
            entry = StagePhase::PreparingEnvironment;
        } else if entry == StagePhase::PreparingEnvironment && env_ready {
            entry = StagePhase::RunningSession;
        }
        let retry_of = Coded {
            code: blocker.code.as_str().to_string(),
            params: blocker.params.clone(),
        };
        Ok(vec![
            Event::BlockerResolved {
                work_item_id: self.item().id,
                blocker_id: blocker.id,
                resolution: BlockerResolution::Retried,
                reason: Some(reason.clone()),
            },
            self.new_run(
                stage,
                Trigger::Manual,
                entry,
                Some(reason),
                Some(retry_of),
                ctx,
            ),
        ])
    }

    fn cancel(&self, reason: String) -> Result<Vec<Event>, Rejection> {
        if !matches!(
            self.item().state,
            WorkItemState::Queued
                | WorkItemState::Admitted
                | WorkItemState::InStage
                | WorkItemState::Blocked
        ) {
            return Err(Rejection::InvalidState {
                state: self.state_name(),
            });
        }
        let mut events = Vec::new();
        if let Some(b) = self.task.open_blocker() {
            events.push(Event::BlockerResolved {
                work_item_id: self.item().id,
                blocker_id: b.id,
                resolution: BlockerResolution::Cancelled,
                reason: Some(reason.clone()),
            });
        }
        events.extend(self.end_session(SessionState::Ended, "cancelled", json!({})));
        events.push(Event::WorkItemCancelled {
            work_item_id: self.item().id,
            reason,
        });
        Ok(events)
    }

    fn job_exhausted(&self, kind: JobKind, error: Coded, ctx: &mut Ctx<'_>) -> Vec<Event> {
        let Some((code, mut params)) = kind.exhausted_blocker() else {
            return vec![];
        };
        if let Value::Object(map) = &mut params {
            map.insert(
                "error".to_string(),
                json!({ "code": error.code, "params": error.params }),
            );
        }
        let mut events = Vec::new();
        if matches!(
            kind,
            JobKind::SessionSendTurn
                | JobKind::SessionDeliverPermissionAnswer
                | JobKind::SessionInterrupt
        ) {
            if let Some(session) = self.task.live_session() {
                events.push(Event::SessionLost {
                    work_item_id: self.item().id,
                    session_id: session.id,
                    cause: "runner_unreachable".to_string(),
                });
            }
            events.extend(self.end_session(
                SessionState::Lost,
                "session_lost",
                json!({ "cause": "runner_unreachable" }),
            ));
        }
        events.extend(self.block(code, params, ctx));
        events
    }

    fn state_name(&self) -> String {
        self.item().state.as_str().to_string()
    }
}

/// Where a retry starts for each blocker code; `None` when only cancel is possible.
pub fn retry_entry(code: BlockerCode, params: &Value) -> Option<(Stage, StagePhase)> {
    use BlockerCode as B;
    Some(match code {
        B::EnvSetupFailed => (Stage::Dev, StagePhase::PreparingEnvironment),
        B::SessionFailed | B::SessionLost | B::SessionTimeout | B::NoChanges | B::CiRed => {
            (Stage::Dev, StagePhase::RunningSession)
        }
        B::PushRejected => (Stage::Dev, StagePhase::Pushing),
        B::MergeConflict => (Stage::Merge, StagePhase::AwaitingCi),
        B::DodFailed => (Stage::Sync, StagePhase::Verifying),
        B::ScmUnavailable => {
            let phase: StagePhase = params.get("phase").and_then(Value::as_str)?.parse().ok()?;
            let stage = match phase {
                StagePhase::PreparingEnvironment
                | StagePhase::RunningSession
                | StagePhase::Pushing
                | StagePhase::OpeningChange => Stage::Dev,
                StagePhase::AwaitingCi | StagePhase::AwaitingMerge | StagePhase::Merging => {
                    Stage::Merge
                }
                StagePhase::Verifying | StagePhase::TearingDown => Stage::Sync,
            };
            // Merging again needs a fresh look at the change and its checks.
            let phase = if phase == StagePhase::Merging {
                StagePhase::AwaitingCi
            } else {
                phase
            };
            (stage, phase)
        }
        _ => return None,
    })
}

/// Applies one event to a task. `None` + `WorkItemCreated` creates it.
pub fn evolve(task: Option<Task>, event: &Event, now: Timestamp) -> Option<Task> {
    if let Event::WorkItemCreated {
        work_item_id,
        project_id,
        spec_id,
        spec_revision_id,
        step_id,
        step_index,
        task_key,
        work,
        branch,
        repo_id,
        priority,
    } = event
    {
        return Some(Task {
            item: WorkItem {
                id: *work_item_id,
                project_id: *project_id,
                origin: WorkItemOrigin::SpecStep,
                spec_id: Some(*spec_id),
                spec_revision_id: Some(*spec_revision_id),
                step_id: Some(step_id.clone()),
                step_index: Some(*step_index),
                task_key: task_key.clone(),
                work: work.clone(),
                repo_id: *repo_id,
                branch: branch.clone(),
                state: WorkItemState::Queued,
                stage: None,
                stage_phase: None,
                priority: *priority,
                version: 1,
                admitted_at: None,
                done_at: None,
                created_at: now,
                updated_at: now,
            },
            stage_run: None,
            gate: None,
            blocker: None,
            change_request: None,
            environment: None,
            session: None,
            permissions: Vec::new(),
            attempts: BTreeMap::new(),
            retired_runs: Vec::new(),
        });
    }
    let mut t = task?;
    if event.work_item_id() != Some(t.item.id) {
        return Some(t);
    }
    t.item.version += 1;
    t.item.updated_at = now;
    apply(&mut t, event, now);
    Some(t)
}

fn set_place(item: &mut WorkItem, state: WorkItemState, place: Option<(Stage, StagePhase)>) {
    item.state = state;
    item.stage = place.map(|p| p.0);
    item.stage_phase = place.map(|p| p.1);
}

fn apply(t: &mut Task, event: &Event, now: Timestamp) {
    use Event as E;
    match event {
        E::WorkItemAdmitted { .. } => {
            t.item.state = WorkItemState::Admitted;
            t.item.admitted_at = Some(now);
        }
        E::StageRunCreated {
            stage_run_id,
            stage,
            attempt,
            trigger,
            entry_phase,
            reason,
            ..
        } => {
            if let Some(old) = t.stage_run.take() {
                t.retired_runs.push(old);
            }
            let waiting = *entry_phase == StagePhase::PreparingEnvironment;
            t.stage_run = Some(StageRun {
                id: *stage_run_id,
                work_item_id: t.item.id,
                stage: *stage,
                attempt: *attempt,
                state: if waiting {
                    StageRunState::Pending
                } else {
                    StageRunState::Running
                },
                trigger: *trigger,
                reason: reason.clone(),
                entry_phase: *entry_phase,
                head_sha: None,
                failure: None,
                started_at: if waiting { None } else { Some(now) },
                ended_at: None,
                created_at: now,
                updated_at: now,
            });
            t.attempts.insert(*stage, *attempt);
            t.gate = None;
            if waiting {
                set_place(&mut t.item, WorkItemState::Admitted, None);
            } else {
                set_place(
                    &mut t.item,
                    WorkItemState::InStage,
                    Some((*stage, *entry_phase)),
                );
            }
        }
        E::StagePhaseChanged {
            phase, head_sha, ..
        } => {
            if let Some(run) = &mut t.stage_run {
                run.state = StageRunState::Running;
                run.started_at.get_or_insert(now);
                if head_sha.is_some() {
                    run.head_sha.clone_from(head_sha);
                }
                run.updated_at = now;
                let stage = run.stage;
                set_place(&mut t.item, WorkItemState::InStage, Some((stage, *phase)));
            }
        }
        E::StageRunSucceeded {
            stage_run_id,
            head_sha,
            ..
        } => {
            if let Some(run) = t.stage_run.as_mut().filter(|r| r.id == *stage_run_id) {
                run.state = StageRunState::Succeeded;
                if head_sha.is_some() {
                    run.head_sha.clone_from(head_sha);
                }
                run.ended_at = Some(now);
                run.updated_at = now;
            }
        }
        E::GateObserved {
            stage_run_id,
            gate_result_id,
            head_sha,
            outcome,
            checks,
            ..
        } => {
            t.gate = Some(GateResult {
                id: *gate_result_id,
                stage_run_id: *stage_run_id,
                work_item_id: t.item.id,
                source: GateSource::Scm,
                head_sha: head_sha.clone(),
                outcome: *outcome,
                checks: checks.clone(),
                observed_at: now,
            });
        }
        E::MergeRequested { .. } => {}
        E::WorkItemBlocked {
            stage_run_id,
            blocker_id,
            code,
            params,
            ..
        } => {
            t.blocker = Some(Blocker {
                id: *blocker_id,
                work_item_id: t.item.id,
                stage_run_id: *stage_run_id,
                code: *code,
                params: params.clone(),
                resolution: None,
                opened_at: now,
                resolved_at: None,
            });
            if let Some(run) = &mut t.stage_run {
                run.state = StageRunState::Blocked;
                run.failure = Some(Coded {
                    code: code.as_str().to_string(),
                    params: params.clone(),
                });
                run.ended_at = Some(now);
                run.updated_at = now;
            }
            set_place(&mut t.item, WorkItemState::Blocked, None);
        }
        E::BlockerResolved {
            blocker_id,
            resolution,
            ..
        } => {
            if let Some(b) = t.blocker.as_mut().filter(|b| b.id == *blocker_id) {
                b.resolution = Some(*resolution);
                b.resolved_at = Some(now);
            }
        }
        E::WorkItemCancelled { .. } => {
            if let Some(run) = t
                .stage_run
                .as_mut()
                .filter(|r| matches!(r.state, StageRunState::Pending | StageRunState::Running))
            {
                run.state = StageRunState::Cancelled;
                run.ended_at = Some(now);
                run.updated_at = now;
            }
            set_place(&mut t.item, WorkItemState::Cancelled, None);
        }
        E::WorkItemDone { .. } => {
            set_place(&mut t.item, WorkItemState::Done, None);
            t.item.done_at = Some(now);
        }
        E::EnvironmentPrepared {
            environment_id,
            repo_id,
            root_path,
            worktree_path,
            branch,
            base_sha,
            ..
        } => {
            t.environment = Some(Environment {
                id: *environment_id,
                work_item_id: t.item.id,
                repo_id: *repo_id,
                root_path: root_path.clone(),
                worktree_path: worktree_path.clone(),
                branch: branch.clone(),
                base_sha: Some(base_sha.clone()),
                state: EnvironmentState::Ready,
                created_at: now,
                updated_at: now,
            });
        }
        E::EnvironmentTornDown { environment_id, .. } => {
            if let Some(env) = t.environment.as_mut().filter(|e| e.id == *environment_id) {
                env.state = EnvironmentState::TornDown;
                env.updated_at = now;
            }
        }
        E::SessionStarted {
            session_id,
            stage_run_id,
            environment_id,
            cli,
            cli_version,
            model,
            account,
            runner_unit,
            socket_path,
            log_path,
            ..
        } => {
            t.permissions.clear();
            t.session = Some(Session {
                id: *session_id,
                work_item_id: t.item.id,
                stage_run_id: *stage_run_id,
                environment_id: *environment_id,
                cli: cli.clone(),
                cli_version: cli_version.clone(),
                cli_session_id: None,
                model: model.clone(),
                account: account.clone(),
                state: SessionState::Starting,
                runner_unit: runner_unit.clone(),
                socket_path: socket_path.clone(),
                log_path: log_path.clone(),
                turns: 0,
                followups: 0,
                running_ms: 0,
                running_since: None,
                cost_usd_micros: None,
                input_tokens: None,
                output_tokens: None,
                end_reason: None,
                started_at: Some(now),
                ended_at: None,
                created_at: now,
                updated_at: now,
            });
        }
        E::SessionInitialized {
            session_id,
            cli_session_id,
            model,
            ..
        } => {
            if let Some(s) = session_mut(t, *session_id, now) {
                s.cli_session_id = Some(cli_session_id.clone());
                if model.is_some() {
                    s.model.clone_from(model);
                }
            }
        }
        E::SessionStateChanged {
            session_id,
            state,
            running_ms,
            ..
        } => {
            if let Some(s) = session_mut(t, *session_id, now) {
                s.state = *state;
                s.running_ms = *running_ms;
                s.running_since = (*state == SessionState::Running).then_some(now);
                if !state.is_live() {
                    s.ended_at = Some(now);
                }
            }
        }
        E::SessionTurnRequested {
            session_id,
            turn,
            prompt,
            ..
        } => {
            if let Some(s) = session_mut(t, *session_id, now) {
                s.turns = *turn;
                if matches!(prompt, Prompt::Uncommitted { .. }) {
                    s.followups += 1;
                }
            }
        }
        E::SessionTurnEnded {
            session_id,
            cost_usd_micros,
            input_tokens,
            output_tokens,
            ..
        } => {
            if let Some(s) = session_mut(t, *session_id, now) {
                let add = |acc: &mut Option<i64>, v: &Option<i64>| {
                    if let Some(v) = v {
                        *acc = Some(acc.unwrap_or(0) + v);
                    }
                };
                add(&mut s.cost_usd_micros, cost_usd_micros);
                add(&mut s.input_tokens, input_tokens);
                add(&mut s.output_tokens, output_tokens);
            }
        }
        E::SessionEnded {
            session_id,
            end_reason,
            ..
        } => {
            if let Some(s) = session_mut(t, *session_id, now) {
                s.end_reason = Some(end_reason.clone());
                s.ended_at = Some(now);
            }
        }
        E::SessionLost { .. } => {}
        E::PermissionRequested {
            session_id,
            permission_request_id,
            cli_request_id,
            tool,
            summary,
            input,
            ..
        } => {
            t.permissions.push(PermissionRequest {
                id: *permission_request_id,
                work_item_id: t.item.id,
                session_id: *session_id,
                cli_request_id: cli_request_id.clone(),
                tool: tool.clone(),
                input: input.clone(),
                summary: summary.clone(),
                state: PermissionState::Pending,
                answer_source: None,
                message: None,
                requested_at: now,
                answered_at: None,
                delivered_at: None,
            });
        }
        E::PermissionAnswered {
            permission_request_id,
            state,
            answer_source,
            message,
            ..
        } => {
            if let Some(p) = t
                .permissions
                .iter_mut()
                .find(|p| p.id == *permission_request_id)
            {
                p.state = *state;
                p.answer_source = Some(answer_source.clone());
                p.message.clone_from(message);
                p.answered_at = Some(now);
            }
        }
        E::PermissionDelivered {
            permission_request_id,
            ..
        } => {
            if let Some(p) = t
                .permissions
                .iter_mut()
                .find(|p| p.id == *permission_request_id)
            {
                p.delivered_at = Some(now);
            }
        }
        E::BranchPushed { .. } => {}
        E::ChangeRequestOpened {
            change_request_id,
            repo_id,
            number,
            url,
            head_branch,
            base_branch,
            head_sha,
            ..
        } => {
            t.change_request = Some(ChangeRequest {
                id: *change_request_id,
                work_item_id: t.item.id,
                repo_id: *repo_id,
                number: *number,
                url: url.clone(),
                head_branch: head_branch.clone(),
                base_branch: base_branch.clone(),
                head_sha: head_sha.clone(),
                state: ChangeState::Open,
                merge_commit_sha: None,
                merged_at: None,
                last_polled_at: None,
                created_at: now,
                updated_at: now,
            });
        }
        E::ChangeRequestUpdated { head_sha, .. } => {
            if let Some(cr) = &mut t.change_request {
                cr.head_sha.clone_from(head_sha);
                cr.updated_at = now;
            }
        }
        E::ChangeRequestMerged {
            merge_commit_sha,
            merged_at,
            ..
        } => {
            if let Some(cr) = &mut t.change_request {
                cr.state = ChangeState::Merged;
                cr.merge_commit_sha = Some(merge_commit_sha.clone());
                cr.merged_at = merged_at.or(Some(now));
                cr.updated_at = now;
            }
        }
        E::ChangeRequestClosed { .. } => {
            if let Some(cr) = &mut t.change_request {
                cr.state = ChangeState::Closed;
                cr.updated_at = now;
            }
        }
        E::WorkItemCreated { .. }
        | E::SpecRegistered { .. }
        | E::SpecRevisionRecorded { .. }
        | E::SpecApproved { .. }
        | E::SpecShipped { .. } => {}
    }
}

fn session_mut(t: &mut Task, id: SessionId, now: Timestamp) -> Option<&mut Session> {
    let s = t.session.as_mut().filter(|s| s.id == id)?;
    s.updated_at = now;
    Some(s)
}
