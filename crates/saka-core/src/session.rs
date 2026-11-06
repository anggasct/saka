use serde_json::{Value, json};

use crate::Ctx;
use crate::authorship::SessionAuthor;
use crate::event::{Event, Prompt};
use crate::ids::{
    EnvironmentId, PermissionRequestId, PermissionRuleId, ProjectId, SessionId, UsageRecordId,
};
use crate::job::{Job, JobKind};
use crate::model::{
    AccountQuota, Budget, Coded, Environment, PermissionRequest, PermissionRule, RunOwner, Session,
    ToolchainUse, Usage, UsageRecord,
};
use crate::policy::{Decision, Effect};
use crate::rejection::Rejection;
use crate::resume;
use crate::time::Timestamp;
use crate::vocab::{
    BlockerCode, BudgetDimension, EnvironmentState, InterruptBy, PermissionState, ResumeCause,
    SessionState, Trigger,
};

pub const SESSION_ENDED_SOURCE: &str = "system:session_ended";
pub const WAIT_LIMIT_SOURCE: &str = "system:wait_limit";
const DENIED_BY_OPERATOR: &str = "Denied by the operator.";
const PERMISSION_EXPIRED_MESSAGE: &str = "The permission request expired.";
const SAKA_INTERRUPT_GRACE_MS: i64 = 5_000;
const BUDGET_DIMENSIONS: [BudgetDimension; 4] = [
    BudgetDimension::RunningMinutes,
    BudgetDimension::Turns,
    BudgetDimension::Tokens,
    BudgetDimension::CostUsd,
];

#[derive(Debug, Clone, PartialEq)]
pub struct SessionConfig {
    pub permission_wait_ms: i64,
    pub idle_ms: i64,
    pub budget_warn_percent: u32,
    pub rate_limit_pause: f64,
    pub account: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunRetry {
    pub trigger: Trigger,
    pub retried: Option<(BlockerCode, Value)>,
    pub follows_up: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunView<'a> {
    pub owner: RunOwner,
    pub project_id: ProjectId,
    pub session: Option<&'a Session>,
    pub permissions: &'a [PermissionRequest],
    pub prior_usage: Usage,
    pub budget: Option<Budget>,
    pub budget_warnings: &'a [BudgetDimension],
    pub resumed_lost: bool,
    pub ready_environment: Option<EnvironmentId>,
    pub retry: RunRetry,
    pub packet: Prompt,
}

impl RunView<'_> {
    pub fn live_session(&self) -> Option<&Session> {
        self.session.filter(|s| s.state.is_live())
    }

    pub fn usage(&self, now: Timestamp) -> Usage {
        let mut usage = self.prior_usage;
        if let Some(s) = self.session.filter(|s| s.owner == self.owner) {
            usage.add(s.usage_at(now));
        }
        usage
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionInput {
    Launched {
        session_id: SessionId,
        cli: String,
        cli_version: Option<String>,
        model: Option<String>,
        account: String,
        runner_unit: String,
        socket_path: String,
        log_path: String,
        author: SessionAuthor,
    },
    Initialized {
        cli_session_id: String,
        model: Option<String>,
    },
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
        remember: bool,
        active_rules: Vec<PermissionRule>,
        allowed_parts: Vec<String>,
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
    CliExited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Gone {
        cause: String,
    },
    ServerRestarted,
    Reattached {
        runner_version: String,
        from_offset: u64,
    },
    Steer {
        session_id: SessionId,
        text: String,
    },
    Interrupt {
        session_id: SessionId,
    },
    Stop {
        session_id: SessionId,
        reason: String,
    },
    QuotaObserved {
        window: String,
        utilization: f64,
        status: String,
        resets_at: Option<Timestamp>,
        refused: bool,
    },
    Tick,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionSignal {
    TurnCompleted { session_id: SessionId, turn: u32 },
    Stopped { code: BlockerCode, params: Value },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionOutcome {
    pub events: Vec<Event>,
    pub signal: Option<SessionSignal>,
}

impl SessionOutcome {
    fn events(events: Vec<Event>) -> SessionOutcome {
        SessionOutcome {
            events,
            signal: None,
        }
    }

    fn stopped(events: Vec<Event>, code: BlockerCode, params: Value) -> SessionOutcome {
        SessionOutcome {
            events,
            signal: Some(SessionSignal::Stopped { code, params }),
        }
    }
}

pub fn decide(
    view: &RunView<'_>,
    input: SessionInput,
    cfg: &SessionConfig,
    ctx: &mut Ctx<'_>,
) -> Result<SessionOutcome, Rejection> {
    let d = Decider {
        view,
        cfg,
        now: ctx.now,
    };
    use SessionInput as I;
    Ok(match input {
        I::Launched {
            session_id,
            cli,
            cli_version,
            model,
            account,
            runner_unit,
            socket_path,
            log_path,
            author,
        } => SessionOutcome::events(d.launched(Launch {
            session_id,
            cli,
            cli_version,
            model,
            account,
            runner_unit,
            socket_path,
            log_path,
            author,
        })),
        I::Initialized {
            cli_session_id,
            model,
        } => SessionOutcome::events(d.with_live_session(|s| {
            vec![Event::SessionInitialized {
                owner: s.owner,
                session_id: s.id,
                cli_session_id,
                model,
            }]
        })),
        I::TurnAccepted { turn } => SessionOutcome::events(d.turn_accepted(turn)),
        I::PermissionRequested {
            permission_request_id,
            cli_request_id,
            tool,
            summary,
            input,
            decision,
        } => SessionOutcome::events(d.permission_requested(
            permission_request_id,
            cli_request_id,
            tool,
            summary,
            input,
            decision,
        )),
        I::AnswerPermission {
            permission_request_id,
            allow,
            message,
            actor,
            remember,
            active_rules,
            allowed_parts,
        } => SessionOutcome::events(d.answer_permission(
            permission_request_id,
            allow,
            message,
            &actor,
            remember.then_some((active_rules.as_slice(), allowed_parts.as_slice())),
            ctx,
        )?),
        I::PermissionDelivered {
            permission_request_id,
        } => SessionOutcome::events(d.permission_delivered(permission_request_id)),
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
        I::CliExited { code, signal } => {
            d.gone("runner_exited", json!({ "code": code, "signal": signal }))
        }
        I::Gone { cause } => d.gone(&cause, json!({})),
        I::ServerRestarted => d.gone("server_restart", json!({})),
        I::Reattached {
            runner_version,
            from_offset,
        } => SessionOutcome::events(d.with_live_session(|s| {
            vec![Event::SessionReattached {
                owner: s.owner,
                session_id: s.id,
                runner_version,
                from_offset,
            }]
        })),
        I::Steer { session_id, text } => d.steer(session_id, text)?,
        I::Interrupt { session_id } => SessionOutcome::events(d.interrupt(session_id)?),
        I::Stop { session_id, reason } => d.stop(session_id, reason)?,
        I::QuotaObserved {
            window,
            utilization,
            status,
            resets_at,
            refused,
        } => d.quota_observed(window, utilization, status, resets_at, refused),
        I::Tick => d.tick(),
    })
}

pub fn end(
    view: &RunView<'_>,
    state: SessionState,
    code: &str,
    params: Value,
    now: Timestamp,
) -> Vec<Event> {
    end_except(view, state, code, params, None, now)
}

pub fn lose(view: &RunView<'_>, cause: &str, now: Timestamp) -> Vec<Event> {
    let mut events = Vec::new();
    if let Some(session) = view.live_session() {
        events.push(Event::SessionLost {
            owner: session.owner,
            session_id: session.id,
            cause: cause.to_string(),
        });
    }
    events.extend(end(
        view,
        SessionState::Lost,
        "session_lost",
        json!({ "cause": cause }),
        now,
    ));
    events
}

pub fn next_turn(
    view: &RunView<'_>,
    turn: u32,
    prompt: Prompt,
    cfg: &SessionConfig,
    now: Timestamp,
) -> SessionOutcome {
    let Some(session) = view.live_session() else {
        return SessionOutcome::default();
    };
    let d = Decider { view, cfg, now };
    let usage = view.usage(now);
    if let Some(dim) = d.exhausted(&usage, &[BudgetDimension::Turns]) {
        return d.budget_stop(session, dim, &usage);
    }
    SessionOutcome::events(vec![Event::SessionTurnRequested {
        owner: session.owner,
        session_id: session.id,
        turn,
        prompt,
    }])
}

fn end_except(
    view: &RunView<'_>,
    state: SessionState,
    code: &str,
    params: Value,
    answered: Option<PermissionRequestId>,
    now: Timestamp,
) -> Vec<Event> {
    let Some(session) = view.live_session() else {
        return vec![];
    };
    let mut events = vec![
        Event::SessionEnded {
            owner: session.owner,
            session_id: session.id,
            end_reason: Coded {
                code: code.to_string(),
                params,
            },
        },
        state_changed(session, state, now),
    ];
    for p in view
        .permissions
        .iter()
        .filter(|p| p.state == PermissionState::Pending && Some(p.id) != answered)
    {
        events.push(Event::PermissionAnswered {
            owner: p.owner,
            permission_request_id: p.id,
            state: PermissionState::Expired,
            answer_source: SESSION_ENDED_SOURCE.to_string(),
            message: None,
        });
    }
    events
}

fn state_changed(session: &Session, state: SessionState, now: Timestamp) -> Event {
    Event::SessionStateChanged {
        owner: session.owner,
        session_id: session.id,
        state,
        running_ms: session.running_ms_at(now),
    }
}

struct Launch {
    session_id: SessionId,
    cli: String,
    cli_version: Option<String>,
    model: Option<String>,
    account: String,
    runner_unit: String,
    socket_path: String,
    log_path: String,
    author: SessionAuthor,
}

struct Decider<'a, 'v> {
    view: &'a RunView<'v>,
    cfg: &'a SessionConfig,
    now: Timestamp,
}

impl Decider<'_, '_> {
    fn with_live_session(&self, f: impl FnOnce(&Session) -> Vec<Event>) -> Vec<Event> {
        self.view.live_session().map(f).unwrap_or_default()
    }

    fn saka_stop(
        &self,
        session: &Session,
        end_code: &str,
        code: BlockerCode,
        params: Value,
        answered: Option<PermissionRequestId>,
    ) -> SessionOutcome {
        let mut events = Vec::new();
        if matches!(session.state, SessionState::Running | SessionState::Waiting) {
            events.push(Event::SessionInterruptRequested {
                owner: session.owner,
                session_id: session.id,
                actor: InterruptBy::Saka,
                turn: session.turns,
            });
        }
        events.extend(end_except(
            self.view,
            SessionState::Failed,
            end_code,
            params.clone(),
            answered,
            self.now,
        ));
        SessionOutcome::stopped(events, code, params)
    }

    fn exhausted(&self, usage: &Usage, dimensions: &[BudgetDimension]) -> Option<BudgetDimension> {
        let budget = self.view.budget?;
        dimensions.iter().copied().find(|d| {
            let limit = budget.limit(*d);
            limit > 0 && usage.used(*d) >= limit
        })
    }

    fn budget_stop(
        &self,
        session: &Session,
        dimension: BudgetDimension,
        usage: &Usage,
    ) -> SessionOutcome {
        let Some(budget) = self.view.budget else {
            return SessionOutcome::default();
        };
        let params = json!({
            "dimension": dimension,
            "used": budget_value(dimension, usage.used(dimension)),
            "limit": budget_value(dimension, budget.limit(dimension)),
        });
        self.saka_stop(
            session,
            "budget_exhausted",
            BlockerCode::BudgetExhausted,
            params,
            None,
        )
    }

    fn budget_warnings(&self, usage: &Usage, dimensions: &[BudgetDimension]) -> Vec<Event> {
        let Some(budget) = self.view.budget else {
            return vec![];
        };
        let percent = i128::from(self.cfg.budget_warn_percent);
        dimensions
            .iter()
            .copied()
            .filter(|d| !self.view.budget_warnings.contains(d))
            .filter_map(|d| {
                let (used, limit) = (usage.used(d), budget.limit(d));
                let over = i128::from(used) * 100 >= i128::from(limit) * percent;
                (limit > 0 && used < limit && over).then(|| Event::BudgetWarned {
                    owner: self.view.owner,
                    dimension: d,
                    used: budget_value(d, used),
                    limit: budget_value(d, limit),
                })
            })
            .collect()
    }

    fn session_for(&self, id: SessionId) -> Result<&Session, Rejection> {
        match self.view.session {
            Some(s) if s.id == id && s.state.is_live() => Ok(s),
            Some(s) if s.id == id => Err(Rejection::InvalidState {
                state: s.state.as_str().to_string(),
            }),
            _ => Err(Rejection::NotFound),
        }
    }

    fn launched(&self, launch: Launch) -> Vec<Event> {
        let Some(environment_id) = self.view.ready_environment else {
            return vec![];
        };
        if !resume::wants_session(self.view) {
            return vec![];
        }
        let plan = resume::plan(self.view);
        let mut events = Vec::new();
        if let (Some(p), Some(previous)) = (&plan, self.view.session)
            && p.cause == ResumeCause::Lost
        {
            events.push(Event::SessionStateChanged {
                owner: previous.owner,
                session_id: previous.id,
                state: SessionState::Resumed,
                running_ms: previous.running_ms,
            });
        }
        let prompt = match &plan {
            Some(p) if p.cli_session_id.is_some() => Prompt::Resume {
                cause: p.reason,
                retry_of: p.retry_of.clone(),
            },
            _ => self.view.packet.clone(),
        };
        let owner = self.view.owner;
        events.extend([
            Event::SessionStarted {
                owner,
                session_id: launch.session_id,
                environment_id,
                cli: launch.cli,
                cli_version: launch.cli_version,
                model: launch.model,
                account: launch.account,
                runner_unit: launch.runner_unit,
                socket_path: launch.socket_path,
                log_path: launch.log_path,
                resumed_from_session_id: plan.as_ref().map(|p| p.from),
                resume_cause: plan.as_ref().map(|p| p.cause),
                author: launch.author,
            },
            Event::SessionTurnRequested {
                owner,
                session_id: launch.session_id,
                turn: 1,
                prompt,
            },
        ]);
        events
    }

    fn turn_accepted(&self, turn: u32) -> Vec<Event> {
        self.with_live_session(|s| {
            if s.turns == turn
                && turn > s.recorded_turn
                && matches!(s.state, SessionState::Starting | SessionState::Idle)
            {
                vec![state_changed(s, SessionState::Running, self.now)]
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
        let Some(session) = self.view.live_session() else {
            return vec![];
        };
        if self
            .view
            .permissions
            .iter()
            .any(|p| p.cli_request_id == cli_request_id)
        {
            return vec![];
        }
        let owner = session.owner;
        let mut events = vec![Event::PermissionRequested {
            owner,
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
                let message = decision.message.clone().unwrap_or_else(|| {
                    format!(
                        "Denied by Saka policy ({}).",
                        source.trim_start_matches("policy:")
                    )
                });
                Some((PermissionState::Denied, source, Some(message)))
            }
            _ => None,
        };
        match answered {
            Some((state, answer_source, message)) => events.push(Event::PermissionAnswered {
                owner,
                permission_request_id,
                state,
                answer_source,
                message,
            }),
            None if session.state == SessionState::Running => {
                events.push(state_changed(session, SessionState::Waiting, self.now))
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
        remember: Option<(&[PermissionRule], &[String])>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Vec<Event>, Rejection> {
        let Some(request) = self.view.permissions.iter().find(|p| p.id == id) else {
            return Err(Rejection::NotFound);
        };
        if request.state != PermissionState::Pending {
            return Err(Rejection::PermissionAlreadyAnswered);
        }
        let project_id = self.view.project_id;
        let rules: Vec<Event> = match remember {
            Some(_) if !allow => {
                return Err(Rejection::Invalid {
                    field: "remember",
                    code: "allow_only",
                });
            }
            Some((active, allowed_parts)) => crate::rule::remembered(request, allowed_parts)?
                .into_iter()
                .filter(|(tool, command)| {
                    !active.iter().any(|r| {
                        r.revoked_at.is_none()
                            && r.project_id == project_id
                            && &r.tool == tool
                            && &r.command == command
                    })
                })
                .map(|(tool, command)| Event::PermissionRuleAdded {
                    rule_id: PermissionRuleId::new(ctx.ids),
                    project_id,
                    owner: request.owner,
                    tool,
                    command,
                    permission_request_id: id,
                    created_by: format!("human:{actor}"),
                })
                .collect(),
            None => Vec::new(),
        };
        let mut events = vec![Event::PermissionAnswered {
            owner: request.owner,
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
            .view
            .permissions
            .iter()
            .any(|p| p.id != id && p.state == PermissionState::Pending);
        if let Some(session) = self.view.live_session()
            && session.state == SessionState::Waiting
            && !others_pending
        {
            events.push(Event::SessionStateChanged {
                owner: session.owner,
                session_id: session.id,
                state: SessionState::Running,
                running_ms: session.running_ms,
            });
        }
        events.extend(rules);
        Ok(events)
    }

    fn permission_delivered(&self, id: PermissionRequestId) -> Vec<Event> {
        match self.view.permissions.iter().find(|p| p.id == id) {
            Some(p) if p.delivered_at.is_none() && awaits_delivery(p) => {
                vec![Event::PermissionDelivered {
                    owner: p.owner,
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
    ) -> SessionOutcome {
        let Some(session) = self.view.session else {
            return SessionOutcome::default();
        };
        let live = session.state.is_live();
        if session.turns != turn || turn <= session.recorded_turn {
            return SessionOutcome::default();
        }
        let running = if live {
            session.running_ms_at(self.now)
        } else {
            session.running_ms
        };
        let mut events = vec![Event::SessionTurnEnded {
            owner: session.owner,
            session_id: session.id,
            turn,
            status: status.clone(),
            cost_usd_micros,
            input_tokens,
            output_tokens,
            running_ms: (running - session.recorded_running_ms).max(0),
            account: session.qualified_account(),
            usage_record_id: UsageRecordId::new(ctx.ids),
        }];
        if !live {
            return SessionOutcome::events(events);
        }
        let mut usage = self.view.usage(self.now);
        usage.tokens += input_tokens.unwrap_or(0) + output_tokens.unwrap_or(0);
        usage.cost_usd_micros += cost_usd_micros.unwrap_or(0);
        let interrupted = session.interrupt_requested == Some(InterruptBy::Operator);
        if !interrupted && status != "success" {
            events.extend(end(
                self.view,
                SessionState::Failed,
                "session_failed",
                json!({ "subtype": status }),
                self.now,
            ));
            return SessionOutcome::stopped(
                events,
                BlockerCode::SessionFailed,
                json!({ "subtype": status }),
            );
        }
        let spend = [BudgetDimension::Tokens, BudgetDimension::CostUsd];
        if let Some(d) = self.exhausted(&usage, &spend) {
            let idle = Session {
                state: SessionState::Idle,
                ..session.clone()
            };
            let mut stop = self.budget_stop(&idle, d, &usage);
            events.append(&mut stop.events);
            return SessionOutcome {
                events,
                signal: stop.signal,
            };
        }
        events.push(state_changed(session, SessionState::Idle, self.now));
        events.extend(self.budget_warnings(&usage, &spend));
        SessionOutcome {
            events,
            signal: session
                .interrupt_requested
                .is_none()
                .then_some(SessionSignal::TurnCompleted {
                    session_id: session.id,
                    turn,
                }),
        }
    }

    fn gone(&self, cause: &str, params: Value) -> SessionOutcome {
        let Some(session) = self.view.live_session() else {
            return SessionOutcome::default();
        };
        let mut events = vec![Event::SessionLost {
            owner: session.owner,
            session_id: session.id,
            cause: cause.to_string(),
        }];
        events.extend(end(
            self.view,
            SessionState::Lost,
            "session_lost",
            json!({ "cause": cause, "detail": params }),
            self.now,
        ));
        let resumable = self.view.ready_environment.is_some()
            && session.owner == self.view.owner
            && !self.view.resumed_lost;
        if resumable {
            return SessionOutcome::events(events);
        }
        SessionOutcome::stopped(events, BlockerCode::SessionLost, json!({ "cause": cause }))
    }

    fn steer(&self, id: SessionId, text: String) -> Result<SessionOutcome, Rejection> {
        let session = self.session_for(id)?;
        let steered = |turn| Event::SessionSteered {
            owner: session.owner,
            session_id: session.id,
            text: text.clone(),
            turn,
            n: session.steers + 1,
        };
        match session.state {
            SessionState::Running => Ok(SessionOutcome::events(vec![steered(session.turns)])),
            SessionState::Idle => {
                let usage = self.view.usage(self.now);
                if let Some(d) = self.exhausted(&usage, &[BudgetDimension::Turns]) {
                    return Ok(self.budget_stop(session, d, &usage));
                }
                Ok(SessionOutcome::events(vec![
                    steered(session.turns + 1),
                    Event::SessionTurnRequested {
                        owner: session.owner,
                        session_id: session.id,
                        turn: session.turns + 1,
                        prompt: Prompt::Steer { text: text.clone() },
                    },
                ]))
            }
            other => Err(Rejection::InvalidState {
                state: other.as_str().to_string(),
            }),
        }
    }

    fn interrupt(&self, id: SessionId) -> Result<Vec<Event>, Rejection> {
        let session = self.session_for(id)?;
        if session.state != SessionState::Running {
            return Err(Rejection::InvalidState {
                state: session.state.as_str().to_string(),
            });
        }
        Ok(vec![
            Event::SessionInterruptRequested {
                owner: session.owner,
                session_id: session.id,
                actor: InterruptBy::Operator,
                turn: session.turns,
            },
            state_changed(session, SessionState::Interrupted, self.now),
        ])
    }

    fn stop(&self, id: SessionId, reason: String) -> Result<SessionOutcome, Rejection> {
        let session = self.session_for(id)?;
        let mut events = vec![Event::SessionStopRequested {
            owner: session.owner,
            session_id: session.id,
            reason: reason.clone(),
        }];
        events.extend(end(
            self.view,
            SessionState::Failed,
            "operator_stop",
            json!({ "reason": reason }),
            self.now,
        ));
        Ok(SessionOutcome::stopped(
            events,
            BlockerCode::SessionFailed,
            json!({ "cause": "operator_stop", "reason": reason }),
        ))
    }

    fn quota_observed(
        &self,
        window: String,
        utilization: f64,
        status: String,
        resets_at: Option<Timestamp>,
        refused: bool,
    ) -> SessionOutcome {
        let Some(session) = self.view.live_session() else {
            return SessionOutcome::default();
        };
        let quota = AccountQuota {
            account: session.qualified_account(),
            window,
            utilization,
            status,
            resets_at,
            observed_at: self.now,
        };
        let observed = Event::QuotaObserved {
            owner: session.owner,
            session_id: session.id,
            account: quota.account.clone(),
            window: quota.window.clone(),
            utilization,
            status: quota.status.clone(),
            resets_at,
        };
        if !refused {
            return SessionOutcome::events(vec![observed]);
        }
        let mut stop = self.saka_stop(
            session,
            "quota_exhausted",
            BlockerCode::QuotaExhausted,
            json!({ "account": quota.account, "window": quota.window, "resets_at": resets_at }),
            None,
        );
        stop.events.insert(0, observed);
        stop
    }

    fn tick(&self) -> SessionOutcome {
        let Some(session) = self.view.live_session() else {
            return SessionOutcome::default();
        };
        let wait = self.cfg.permission_wait_ms;
        if let Some(p) =
            self.view.permissions.iter().find(|p| {
                p.state == PermissionState::Pending && self.now.since(p.requested_at) > wait
            })
        {
            let mut stop = self.saka_stop(
                session,
                "permission_expired",
                BlockerCode::PermissionExpired,
                json!({ "tool": p.tool, "summary": p.summary, "hours": wait / 3_600_000 }),
                Some(p.id),
            );
            stop.events.insert(
                0,
                Event::PermissionAnswered {
                    owner: p.owner,
                    permission_request_id: p.id,
                    state: PermissionState::Expired,
                    answer_source: WAIT_LIMIT_SOURCE.to_string(),
                    message: Some(PERMISSION_EXPIRED_MESSAGE.to_string()),
                },
            );
            return stop;
        }
        if session.state == SessionState::Idle
            && session
                .idle_since
                .is_some_and(|since| self.now.since(since) > self.cfg.idle_ms)
        {
            let params = json!({ "cause": "idle", "minutes": self.cfg.idle_ms / 60_000 });
            let events = end(
                self.view,
                SessionState::Failed,
                "session_timeout",
                params.clone(),
                self.now,
            );
            return SessionOutcome::stopped(events, BlockerCode::SessionTimeout, params);
        }
        if matches!(
            session.state,
            SessionState::Running | SessionState::Waiting | SessionState::Interrupted
        ) {
            let usage = self.view.usage(self.now);
            let running = [BudgetDimension::RunningMinutes];
            if let Some(d) = self.exhausted(&usage, &running) {
                return self.budget_stop(session, d, &usage);
            }
            return SessionOutcome::events(self.budget_warnings(&usage, &running));
        }
        SessionOutcome::default()
    }
}

pub struct RunParts<'a> {
    pub environment: &'a mut Option<Environment>,
    pub session: &'a mut Option<Session>,
    pub retired_sessions: &'a mut Vec<Session>,
    pub permissions: &'a mut Vec<PermissionRequest>,
    pub prior_usage: &'a mut Usage,
    pub usage_records: &'a mut Vec<UsageRecord>,
    pub quota_observations: &'a mut Vec<AccountQuota>,
}

fn session_mut(
    session: &mut Option<Session>,
    id: SessionId,
    now: Timestamp,
) -> Option<&mut Session> {
    let s = session.as_mut().filter(|s| s.id == id)?;
    s.updated_at = now;
    Some(s)
}

pub fn apply(run: RunParts<'_>, event: &Event, now: Timestamp) -> bool {
    use Event as E;
    match event {
        E::EnvironmentPrepared {
            owner,
            environment_id,
            repo_id,
            root_path,
            worktree_path,
            branch,
            base_sha,
            port_start,
            port_count,
            recipe,
            recipe_sha256,
            toolchains,
            path_scope,
            docs,
            ..
        } => {
            *run.environment = Some(Environment {
                id: *environment_id,
                owner: *owner,
                repo_id: *repo_id,
                root_path: root_path.clone(),
                worktree_path: worktree_path.clone(),
                branch: branch.clone(),
                base_sha: Some(base_sha.clone()),
                state: EnvironmentState::Ready,
                created_at: now,
                updated_at: now,
                port_start: *port_start,
                port_count: *port_count,
                recipe: recipe.clone(),
                recipe_sha256: recipe_sha256.clone(),
                toolchains: toolchains
                    .iter()
                    .map(|t| ToolchainUse {
                        downloaded: false,
                        ..t.clone()
                    })
                    .collect(),
                path_scope: path_scope.clone(),
                docs: docs.as_deref().cloned(),
            });
        }
        E::EnvironmentTornDown { environment_id, .. } => {
            if let Some(env) = run.environment.as_mut().filter(|e| e.id == *environment_id) {
                env.state = EnvironmentState::TornDown;
                env.updated_at = now;
            }
        }
        E::EnvironmentTeardownFailed { environment_id, .. } => {
            if let Some(env) = run.environment.as_mut().filter(|e| e.id == *environment_id) {
                env.state = EnvironmentState::TeardownFailed;
                env.updated_at = now;
            }
        }
        E::SessionStarted {
            owner,
            session_id,
            environment_id,
            cli,
            cli_version,
            model,
            account,
            runner_unit,
            socket_path,
            log_path,
            resumed_from_session_id,
            resume_cause,
            ..
        } => {
            run.permissions.clear();
            if let Some(old) = run.session.take() {
                if old.owner == *owner {
                    run.prior_usage.add(old.usage_at(now));
                }
                run.retired_sessions.push(old);
            }
            *run.session = Some(Session {
                id: *session_id,
                owner: *owner,
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
                resumed_from_session_id: *resumed_from_session_id,
                resume_cause: *resume_cause,
                idle_since: None,
                interrupt_requested: None,
                steers: 0,
                last_offset: 0,
                recorded_turn: 0,
                recorded_running_ms: 0,
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
            if let Some(s) = session_mut(run.session, *session_id, now) {
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
            if let Some(s) = session_mut(run.session, *session_id, now) {
                s.idle_since = match (*state, s.state) {
                    (SessionState::Idle, SessionState::Idle) => s.idle_since.or(Some(now)),
                    (SessionState::Idle, _) => Some(now),
                    _ => None,
                };
                s.state = *state;
                s.running_ms = *running_ms;
                s.running_since =
                    matches!(*state, SessionState::Running | SessionState::Interrupted)
                        .then_some(now);
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
            if let Some(s) = session_mut(run.session, *session_id, now) {
                s.turns = *turn;
                if matches!(
                    prompt,
                    Prompt::Uncommitted { .. }
                        | Prompt::OutOfScope { .. }
                        | Prompt::DocsUncommitted { .. }
                        | Prompt::DocsOutOfScope { .. }
                        | Prompt::VerdictFormat { .. }
                ) {
                    s.followups += 1;
                }
                if s.interrupt_requested == Some(InterruptBy::Operator) {
                    s.interrupt_requested = None;
                }
            }
        }
        E::SessionTurnEnded {
            session_id,
            turn,
            status,
            cost_usd_micros,
            input_tokens,
            output_tokens,
            running_ms,
            account,
            usage_record_id,
            ..
        } => {
            if let Some(s) = session_mut(run.session, *session_id, now) {
                s.recorded_turn = *turn;
                s.recorded_running_ms += running_ms;
                let record = UsageRecord {
                    id: *usage_record_id,
                    owner: s.owner,
                    session_id: s.id,
                    account: account.clone(),
                    turn: *turn,
                    status: status.clone(),
                    input_tokens: *input_tokens,
                    output_tokens: *output_tokens,
                    cost_usd_micros: *cost_usd_micros,
                    running_ms: *running_ms,
                    ended_at: now,
                };
                let add = |acc: &mut Option<i64>, v: &Option<i64>| {
                    if let Some(v) = v {
                        *acc = Some(acc.unwrap_or(0) + v);
                    }
                };
                add(&mut s.cost_usd_micros, cost_usd_micros);
                add(&mut s.input_tokens, input_tokens);
                add(&mut s.output_tokens, output_tokens);
                run.usage_records.push(record);
            }
        }
        E::SessionSteered { session_id, n, .. } => {
            if let Some(s) = session_mut(run.session, *session_id, now) {
                s.steers = *n;
            }
        }
        E::SessionInterruptRequested {
            session_id, actor, ..
        } => {
            if let Some(s) = session_mut(run.session, *session_id, now) {
                s.interrupt_requested = Some(*actor);
            }
        }
        E::QuotaObserved {
            account,
            window,
            utilization,
            status,
            resets_at,
            ..
        } => run.quota_observations.push(AccountQuota {
            account: account.clone(),
            window: window.clone(),
            utilization: *utilization,
            status: status.clone(),
            resets_at: *resets_at,
            observed_at: now,
        }),
        E::SessionReattached { .. } | E::SessionStopRequested { .. } | E::SessionLost { .. } => {}
        E::SessionEnded {
            session_id,
            end_reason,
            ..
        } => {
            if let Some(s) = session_mut(run.session, *session_id, now) {
                s.end_reason = Some(end_reason.clone());
                s.ended_at = Some(now);
            }
        }
        E::PermissionRequested {
            owner,
            session_id,
            permission_request_id,
            cli_request_id,
            tool,
            summary,
            input,
            ..
        } => {
            run.permissions.push(PermissionRequest {
                id: *permission_request_id,
                owner: *owner,
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
            if let Some(p) = run
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
            if let Some(p) = run
                .permissions
                .iter_mut()
                .find(|p| p.id == *permission_request_id)
            {
                p.delivered_at = Some(now);
            }
        }
        _ => return false,
    }
    true
}

pub fn react(view: &RunView<'_>, event: &Event, now: Timestamp) -> Vec<Job> {
    use Event as E;
    match event {
        E::SessionTurnRequested {
            owner,
            session_id,
            turn,
            prompt,
        } => vec![Job::new(
            JobKind::SessionSendTurn,
            format!("{session_id}:{turn}"),
            json!({ "owner": owner, "session_id": session_id, "turn": turn, "prompt": prompt }),
            now,
        )],
        E::SessionSteered {
            owner,
            session_id,
            text,
            n,
            ..
        } if view
            .live_session()
            .is_some_and(|s| s.id == *session_id && s.state == SessionState::Running) =>
        {
            vec![Job::new(
                JobKind::SessionSteer,
                format!("{session_id}:{n}"),
                json!({ "owner": owner, "session_id": session_id, "n": n, "text": text }),
                now,
            )]
        }
        E::SessionInterruptRequested {
            owner,
            session_id,
            turn,
            ..
        } => vec![Job::new(
            JobKind::SessionInterrupt,
            format!("{session_id}:{turn}"),
            json!({ "owner": owner, "session_id": session_id }),
            now,
        )],
        E::SessionEnded { session_id, .. } => {
            let session = view.session.filter(|s| s.id == *session_id);
            let interrupted =
                session.is_some_and(|s| s.interrupt_requested == Some(InterruptBy::Saka));
            let at = if interrupted {
                now.plus_ms(SAKA_INTERRUPT_GRACE_MS)
            } else {
                now
            };
            session.map(|s| stop_job(s, at)).into_iter().collect()
        }
        E::PermissionAnswered {
            permission_request_id,
            state: PermissionState::Expired,
            answer_source,
            ..
        } if answer_source == WAIT_LIMIT_SOURCE => deliver(view, *permission_request_id, now),
        E::PermissionAnswered {
            permission_request_id,
            state: PermissionState::Allowed | PermissionState::Denied,
            ..
        } => deliver(view, *permission_request_id, now),
        _ => vec![],
    }
}

pub fn stop_job(session: &Session, now: Timestamp) -> Job {
    Job::new(
        JobKind::SessionStop,
        session.id,
        json!({ "owner": session.owner, "session_id": session.id, "runner_unit": session.runner_unit }),
        now,
    )
}

pub fn deliver_job(p: &PermissionRequest, now: Timestamp) -> Job {
    Job::new(
        JobKind::SessionDeliverPermissionAnswer,
        p.id,
        json!({
            "owner": p.owner,
            "session_id": p.session_id,
            "permission_request_id": p.id,
            "cli_request_id": p.cli_request_id,
            "behavior": if p.state == PermissionState::Allowed { "allow" } else { "deny" },
            "message": p.message,
        }),
        now,
    )
}

fn deliver(view: &RunView<'_>, id: PermissionRequestId, now: Timestamp) -> Vec<Job> {
    view.permissions
        .iter()
        .filter(|p| p.id == id)
        .map(|p| deliver_job(p, now))
        .collect()
}

pub fn session_actions(session: &Session) -> Vec<&'static str> {
    match session.state {
        SessionState::Running => vec!["steer", "interrupt", "stop"],
        SessionState::Idle => vec!["steer", "stop"],
        SessionState::Starting | SessionState::Waiting | SessionState::Interrupted => vec!["stop"],
        _ => vec![],
    }
}

pub fn budget_value(dimension: BudgetDimension, value: i64) -> Value {
    match dimension {
        BudgetDimension::RunningMinutes => json!(value / 60_000),
        BudgetDimension::CostUsd => json!(format!(
            "{}.{:06}",
            value.div_euclid(1_000_000),
            value.rem_euclid(1_000_000)
        )),
        BudgetDimension::Turns | BudgetDimension::Tokens => json!(value),
    }
}

pub fn budget_dimensions() -> &'static [BudgetDimension] {
    &BUDGET_DIMENSIONS
}

pub fn awaits_delivery(p: &PermissionRequest) -> bool {
    match p.state {
        PermissionState::Allowed | PermissionState::Denied => true,
        PermissionState::Expired => p.answer_source.as_deref() == Some(WAIT_LIMIT_SOURCE),
        PermissionState::Pending => false,
    }
}
