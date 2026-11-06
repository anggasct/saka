use serde_json::{Value, json};

use crate::Ctx;
use crate::admission::{self, Waiting};
use crate::event::{Event, Prompt};
use crate::ids::{
    EnvironmentId, ProjectId, RepoId, SpecDraftAttemptId, SpecDraftId, SpecId, SpecRevisionId,
};
use crate::job::{Job, JobKind, exhausted_code};
use crate::model::{
    AccountQuota, Budget, Coded, Draft, DraftDocument, LintRound, RetryRequest, RoundMark,
    RunOwner, SpecDraft, SpecDraftAttempt,
};
use crate::rejection::Rejection;
use crate::resume;
use crate::session::{self, RunRetry, RunView, SessionConfig, SessionInput, SessionOutcome};
use crate::time::Timestamp;
use crate::vocab::{
    DraftAttemptState, DraftFailureCode, DraftLocale, EnvironmentState, LintOutcome,
    PermissionState, PrimaryAction, ResumeCause, SessionState, SpecDraftState, Trigger,
};

pub const MANUAL_RETRY_LIMIT: u32 = 3;
pub const LINT_PASSED: &str = "lint_passed";
const TEARDOWN_RETRY_INTERVAL_MS: i64 = 3_600_000;

#[derive(Debug, Clone, PartialEq)]
pub struct DraftConfig {
    pub max_active: usize,
    pub lint_rounds: u32,
    pub budget: Budget,
    pub session: SessionConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketFacts {
    pub text: String,
    pub sha256: String,
    pub documents: Vec<DraftDocument>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DraftInput {
    Admit {
        attempt_id: SpecDraftAttemptId,
        budget: Budget,
    },
    EnvironmentPrepared {
        environment_id: EnvironmentId,
        repo_id: RepoId,
        root_path: String,
        worktree_path: String,
        base_sha: String,
        packet: PacketFacts,
    },
    EnvironmentFailed {
        code: DraftFailureCode,
        params: Value,
    },
    Session(SessionInput),
    Linted(LintRound),
    Committed {
        commit: String,
        spec_id: SpecId,
        revision_id: SpecRevisionId,
    },
    CommitFailed {
        code: DraftFailureCode,
        params: Value,
        commit: Option<String>,
    },
    EnvironmentTornDown {
        environment_id: EnvironmentId,
    },
    TeardownFailed {
        environment_id: EnvironmentId,
        error: Coded,
    },
    Retry {
        reason: String,
        note: Option<String>,
    },
    Cancel {
        reason: String,
    },
    JobExhausted {
        kind: JobKind,
        error: Coded,
    },
    Tick,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDraft {
    pub spec_draft_id: SpecDraftId,
    pub project_id: ProjectId,
    pub path: String,
    pub slug: String,
    pub idea: String,
    pub locale: DraftLocale,
    pub context_paths: Vec<String>,
}

#[must_use]
pub fn request(new: NewDraft) -> Event {
    Event::SpecDraftRequested {
        spec_draft_id: new.spec_draft_id,
        project_id: new.project_id,
        path: new.path,
        slug: new.slug,
        idea: new.idea,
        locale: new.locale,
        context_paths: new.context_paths,
    }
}

pub fn run_view(d: &Draft) -> Option<RunView<'_>> {
    let attempt = d.attempt.as_ref()?;
    let owner = RunOwner::DraftAttempt {
        spec_draft_id: d.draft.id,
        attempt_id: attempt.id,
    };
    Some(RunView {
        owner,
        project_id: d.draft.project_id,
        session: d.session.as_ref().filter(|s| s.owner == owner),
        permissions: &d.permissions,
        prior_usage: d.prior_usage,
        budget: Some(attempt.budget),
        budget_warnings: &attempt.budget_warnings,
        resumed_lost: attempt.resumed_lost,
        ready_environment: d
            .ready_environment()
            .filter(|e| e.owner == owner)
            .map(|e| e.id),
        retry: RunRetry {
            trigger: attempt.trigger,
            retried: None,
            follows_up: false,
        },
        packet: Prompt::DraftPacket,
    })
}

pub fn decide(
    draft: &Draft,
    input: DraftInput,
    cfg: &DraftConfig,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<Event>, Rejection> {
    let d = Decider {
        d: draft,
        cfg,
        now: ctx.now,
    };
    use DraftInput as I;
    Ok(match input {
        I::Admit { attempt_id, budget } => d.admit(attempt_id, budget),
        I::EnvironmentPrepared {
            environment_id,
            repo_id,
            root_path,
            worktree_path,
            base_sha,
            packet,
        } => d.environment_prepared(
            environment_id,
            repo_id,
            root_path,
            worktree_path,
            base_sha,
            packet,
        ),
        I::EnvironmentFailed { code, params } if d.is(SpecDraftState::Preparing) => {
            d.fail(code, params, None)
        }
        I::EnvironmentFailed { .. } => vec![],
        I::Session(input) => return d.session(input, ctx),
        I::Linted(round) => d.linted(round),
        I::Committed {
            commit,
            spec_id,
            revision_id,
        } => d.committed(commit, spec_id, revision_id),
        I::CommitFailed {
            code,
            params,
            commit,
        } if d.is(SpecDraftState::Committing) => d.fail(code, params, commit),
        I::CommitFailed { .. } => vec![],
        I::EnvironmentTornDown { environment_id } => d.environment_torn_down(environment_id),
        I::TeardownFailed {
            environment_id,
            error,
        } => d.teardown_failed(environment_id, error),
        I::Retry { reason, note } => return d.retry(reason, note),
        I::Cancel { reason } => return d.cancel(reason),
        I::JobExhausted { kind, error } => d.job_exhausted(kind, error, ctx),
        I::Tick => d.tick(ctx),
    })
}

struct Decider<'a> {
    d: &'a Draft,
    cfg: &'a DraftConfig,
    now: Timestamp,
}

impl Decider<'_> {
    fn id(&self) -> SpecDraftId {
        self.d.draft.id
    }

    fn is(&self, state: SpecDraftState) -> bool {
        self.d.draft.state == state
    }

    fn state_rejection(&self) -> Rejection {
        Rejection::InvalidState {
            state: self.d.draft.state.as_str().to_string(),
        }
    }

    fn admit(&self, attempt_id: SpecDraftAttemptId, budget: Budget) -> Vec<Event> {
        let next = self.d.draft.attempt + 1;
        // The previous attempt's worktree goes first: a draft holds one live environment.
        let retiring = self.d.live_environment().is_some();
        if !self.is(SpecDraftState::Queued) || next > MANUAL_RETRY_LIMIT + 1 || retiring {
            return vec![];
        }
        let (trigger, reason, note) = match (&self.d.retry_request, next) {
            (_, 1) => (Trigger::Auto, None, None),
            (Some(r), _) => (Trigger::Manual, Some(r.reason.clone()), r.note.clone()),
            (None, _) => (Trigger::Manual, Some("retry".to_string()), None),
        };
        vec![Event::SpecDraftAttemptStarted {
            spec_draft_id: self.id(),
            attempt_id,
            attempt: next,
            trigger,
            reason,
            note,
            budget,
        }]
    }

    fn environment_prepared(
        &self,
        environment_id: EnvironmentId,
        repo_id: RepoId,
        root_path: String,
        worktree_path: String,
        base_sha: String,
        packet: PacketFacts,
    ) -> Vec<Event> {
        let (Some(attempt), Some(owner)) = (self.d.running_attempt(), self.d.run_owner()) else {
            return vec![];
        };
        let ready = self.d.ready_environment().is_some_and(|e| e.owner == owner);
        if !self.is(SpecDraftState::Preparing) || ready {
            return vec![];
        }
        vec![
            Event::EnvironmentPrepared {
                owner,
                environment_id,
                repo_id,
                root_path,
                worktree_path,
                branch: None,
                base_sha: base_sha.clone(),
                port_start: None,
                port_count: None,
                recipe: None,
                recipe_sha256: None,
                setup: vec![],
                toolchains: vec![],
                path_scope: vec![crate::scope::EVERYTHING.to_string()],
                docs: None,
            },
            Event::SpecDraftPacketRendered {
                spec_draft_id: self.id(),
                attempt_id: attempt.id,
                base_sha,
                packet_sha256: packet.sha256,
                documents: packet.documents,
            },
        ]
    }

    fn end_session(&self, code: &str, params: Value) -> Vec<Event> {
        run_view(self.d)
            .map(|view| session::end(&view, SessionState::Ended, code, params, self.now))
            .unwrap_or_default()
    }

    fn failed(&self, code: DraftFailureCode, params: Value, commit: Option<String>) -> Vec<Event> {
        let Some(attempt) = self.d.running_attempt() else {
            return vec![];
        };
        vec![Event::SpecDraftFailed {
            spec_draft_id: self.id(),
            attempt_id: attempt.id,
            code,
            params,
            commit,
        }]
    }

    fn fail(&self, code: DraftFailureCode, params: Value, commit: Option<String>) -> Vec<Event> {
        if self.d.running_attempt().is_none() {
            return vec![];
        }
        let mut events = self.end_session(code.as_str(), params.clone());
        events.extend(self.failed(code, params, commit));
        events
    }

    fn session(&self, input: SessionInput, ctx: &mut Ctx<'_>) -> Result<Vec<Event>, Rejection> {
        if let SessionInput::AnswerPermission { remember: true, .. } = &input {
            return Err(Rejection::Invalid {
                field: "remember",
                code: "not_applicable",
            });
        }
        let Some(view) = run_view(self.d) else {
            return match input {
                SessionInput::Steer { .. }
                | SessionInput::Interrupt { .. }
                | SessionInput::Stop { .. }
                | SessionInput::AnswerPermission { .. } => Err(Rejection::NotFound),
                _ => Ok(vec![]),
            };
        };
        if matches!(input, SessionInput::Launched { .. })
            && (!self.is(SpecDraftState::Drafting) || self.d.running_attempt().is_none())
        {
            return Ok(vec![]);
        }
        let outcome = session::decide(&view, input, &self.cfg.session, ctx)?;
        Ok(self.settle(outcome))
    }

    fn settle(&self, outcome: SessionOutcome) -> Vec<Event> {
        let mut events = outcome.events;
        if let Some(session::SessionSignal::Stopped { code, params }) = outcome.signal
            && self.is(SpecDraftState::Drafting)
        {
            let code =
                DraftFailureCode::from_blocker(code).unwrap_or(DraftFailureCode::SessionFailed);
            events.extend(self.failed(code, params, None));
        }
        events
    }

    fn linted(&self, round: LintRound) -> Vec<Event> {
        let (Some(attempt), Some(view)) = (self.d.running_attempt(), run_view(self.d)) else {
            return vec![];
        };
        let Some(session) = view.live_session() else {
            return vec![];
        };
        let fresh = session.id == round.session_id
            && session.turns == round.turn
            && session.recorded_turn == round.turn
            && session.state == SessionState::Idle
            && session.interrupt_requested.is_none();
        let seen = self
            .d
            .last_round
            .is_some_and(|m| m.session_id == round.session_id && m.turn >= round.turn);
        if !self.is(SpecDraftState::Drafting) || attempt.id != round.attempt_id || !fresh || seen {
            return vec![];
        }
        let number = attempt.rounds + 1;
        let max_rounds = self.cfg.lint_rounds;
        if number > max_rounds {
            return vec![];
        }
        let mut events = vec![Event::SpecDraftLinted {
            spec_draft_id: self.id(),
            attempt_id: attempt.id,
            round: number,
            session_id: round.session_id,
            turn: round.turn,
            outcome: round.outcome,
            bytes: round.bytes,
            content_hash: round.content_hash.clone(),
            issue_codes: issue_codes(&round.issues),
            decisions_open: round.decisions_open,
        }];
        match round.outcome {
            LintOutcome::Passed => {
                events.push(Event::SpecDraftCommitting {
                    spec_draft_id: self.id(),
                    attempt_id: attempt.id,
                    round: number,
                });
                events.extend(self.end_session(LINT_PASSED, json!({ "round": number })));
            }
            outcome if number < max_rounds => {
                let prompt = if outcome == LintOutcome::Missing {
                    Prompt::DraftMissing {
                        round: number,
                        max_rounds,
                    }
                } else {
                    Prompt::DraftLint {
                        round: number,
                        max_rounds,
                    }
                };
                let next = session::next_turn(
                    &view,
                    session.turns + 1,
                    prompt,
                    &self.cfg.session,
                    self.now,
                );
                events.extend(self.settle(next));
            }
            _ => events.extend(self.fail(
                DraftFailureCode::LintRoundsExhausted,
                json!({ "rounds": number }),
                None,
            )),
        }
        events
    }

    fn committed(
        &self,
        commit: String,
        spec_id: SpecId,
        revision_id: SpecRevisionId,
    ) -> Vec<Event> {
        let Some(attempt) = self.d.running_attempt() else {
            return vec![];
        };
        if !self.is(SpecDraftState::Committing) {
            return vec![];
        }
        vec![Event::SpecDraftSucceeded {
            spec_draft_id: self.id(),
            attempt_id: attempt.id,
            spec_id,
            revision_id,
            commit,
        }]
    }

    fn environment_torn_down(&self, id: EnvironmentId) -> Vec<Event> {
        let Some(env) = self
            .d
            .environment_by_id(id)
            .filter(|e| e.state != EnvironmentState::TornDown)
        else {
            return vec![];
        };
        vec![Event::EnvironmentTornDown {
            owner: env.owner,
            environment_id: env.id,
            teardown_failures: vec![],
        }]
    }

    fn teardown_failed(&self, id: EnvironmentId, error: Coded) -> Vec<Event> {
        let Some(env) = self.d.environment_by_id(id).filter(|e| {
            !matches!(
                e.state,
                EnvironmentState::TeardownFailed | EnvironmentState::TornDown
            )
        }) else {
            return vec![];
        };
        vec![Event::EnvironmentTeardownFailed {
            owner: env.owner,
            environment_id: env.id,
            error,
        }]
    }

    fn retry(&self, reason: String, note: Option<String>) -> Result<Vec<Event>, Rejection> {
        if !self.is(SpecDraftState::Failed)
            || !failure_code(self.d).is_some_and(|c| c.is_retryable())
        {
            return Err(self.state_rejection());
        }
        if self.d.draft.retries >= MANUAL_RETRY_LIMIT {
            return Err(Rejection::RetryLimitReached {
                stage: None,
                limit: MANUAL_RETRY_LIMIT,
            });
        }
        Ok(vec![Event::SpecDraftRetried {
            spec_draft_id: self.id(),
            reason,
            note,
        }])
    }

    fn cancel(&self, reason: String) -> Result<Vec<Event>, Rejection> {
        if !cancellable(self.d.draft.state) {
            return Err(self.state_rejection());
        }
        let mut events = self.end_session("cancelled", json!({}));
        events.push(Event::SpecDraftCancelled {
            spec_draft_id: self.id(),
            reason,
        });
        Ok(events)
    }

    fn job_exhausted(&self, kind: JobKind, error: Coded, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if kind == JobKind::EnvTeardown {
            // A teardown job names its environment; the worker reports it as `TeardownFailed`.
            return vec![];
        }
        if kind == JobKind::SessionReattach {
            let gone = SessionInput::Gone {
                cause: "runner_silent".to_string(),
            };
            return self.session(gone, ctx).unwrap_or_default();
        }
        let detail = json!({ "code": error.code, "params": error.params });
        match kind {
            JobKind::DraftLint if self.is(SpecDraftState::Drafting) => {
                return self.fail(
                    DraftFailureCode::WorktreeUnreadable,
                    json!({ "error": detail }),
                    None,
                );
            }
            JobKind::DraftCommit if self.is(SpecDraftState::Committing) => {
                return self.fail(
                    DraftFailureCode::ScmUnavailable,
                    json!({ "phase": "committing" }),
                    None,
                );
            }
            JobKind::EnvPrepare if self.is(SpecDraftState::Preparing) => {}
            JobKind::SessionStart
            | JobKind::SessionSendTurn
            | JobKind::SessionDeliverPermissionAnswer
            | JobKind::SessionInterrupt
            | JobKind::SessionSteer
                if self.is(SpecDraftState::Drafting) => {}
            _ => return vec![],
        }
        let Some((code, mut params)) = exhausted_code(kind) else {
            return vec![];
        };
        let Some(code) = DraftFailureCode::from_blocker(code) else {
            return vec![];
        };
        if let Value::Object(map) = &mut params {
            map.insert("error".to_string(), detail);
        }
        let mut events = Vec::new();
        if matches!(
            kind,
            JobKind::SessionSendTurn
                | JobKind::SessionDeliverPermissionAnswer
                | JobKind::SessionInterrupt
                | JobKind::SessionSteer
        ) && let Some(view) = run_view(self.d)
        {
            events.extend(session::lose(&view, "runner_unreachable", self.now));
            events.extend(self.failed(code, params, None));
            return events;
        }
        self.fail(code, params, None)
    }

    fn tick(&self, ctx: &mut Ctx<'_>) -> Vec<Event> {
        if self.d.live_session().is_none() {
            return vec![];
        }
        self.session(SessionInput::Tick, ctx).unwrap_or_default()
    }
}

fn issue_codes(issues: &[Value]) -> Vec<String> {
    issues
        .iter()
        .filter_map(|i| i.get("code").and_then(Value::as_str).map(String::from))
        .collect()
}

pub fn failure_code(d: &Draft) -> Option<DraftFailureCode> {
    d.draft.failure.as_ref()?.code.parse().ok()
}

fn cancellable(state: SpecDraftState) -> bool {
    matches!(
        state,
        SpecDraftState::Queued
            | SpecDraftState::Preparing
            | SpecDraftState::Drafting
            | SpecDraftState::Failed
    )
}

pub fn can_retry(d: &Draft) -> bool {
    d.draft.state == SpecDraftState::Failed
        && failure_code(d).is_some_and(DraftFailureCode::is_retryable)
        && d.draft.retries < MANUAL_RETRY_LIMIT
}

pub fn available_actions(d: &Draft) -> Vec<&'static str> {
    let mut actions = Vec::new();
    if can_retry(d) {
        actions.push("retry");
    }
    if cancellable(d.draft.state) {
        actions.push("cancel");
    }
    actions
}

pub fn primary_action(d: &Draft) -> Option<PrimaryAction> {
    let code = failure_code(d).filter(|_| d.draft.state == SpecDraftState::Failed)?;
    let retry = can_retry(d).then_some(PrimaryAction::Retry);
    let written = d.has_content.then_some(PrimaryAction::OpenWritten);
    match code {
        DraftFailureCode::RegisterFailed => Some(PrimaryAction::OpenFile),
        DraftFailureCode::LintRoundsExhausted
        | DraftFailureCode::PushRejected
        | DraftFailureCode::TargetExists
        | DraftFailureCode::TargetPathConflict => written.or(retry),
        _ => retry.or(written),
    }
}

pub fn evolve(draft: Option<Draft>, event: &Event, now: Timestamp) -> Option<Draft> {
    if let Event::SpecDraftRequested {
        spec_draft_id,
        project_id,
        path,
        slug,
        idea,
        locale,
        context_paths,
    } = event
    {
        return Some(Draft::new(SpecDraft {
            id: *spec_draft_id,
            project_id: *project_id,
            path: path.clone(),
            slug: slug.clone(),
            idea: idea.clone(),
            locale: *locale,
            context_paths: context_paths.clone(),
            state: SpecDraftState::Queued,
            attempt: 0,
            current_attempt_id: None,
            retries: 0,
            queued_at: now,
            failure: None,
            spec_id: None,
            commit_sha: None,
            version: 1,
            created_by: String::new(),
            created_at: now,
            updated_at: now,
            finished_at: None,
        }));
    }
    let mut d = draft?;
    if event.spec_draft_id() != Some(d.draft.id) {
        return Some(d);
    }
    if event.stream() == crate::event::Stream::SpecDraft(d.draft.id) {
        d.draft.version += 1;
    }
    d.draft.updated_at = now;
    apply(&mut d, event, now);
    Some(d)
}

fn finish(d: &mut Draft, state: SpecDraftState, attempt: DraftAttemptState, now: Timestamp) {
    d.draft.state = state;
    d.draft.finished_at = Some(now);
    if let Some(a) = d
        .attempt
        .as_mut()
        .filter(|a| a.state == DraftAttemptState::Running)
    {
        a.state = attempt;
        a.ended_at = Some(now);
    }
}

fn current<'a>(d: &'a mut Draft, owner: &RunOwner) -> Option<&'a mut SpecDraftAttempt> {
    d.attempt
        .as_mut()
        .filter(|a| owner.attempt_id() == Some(a.id))
}

fn apply(d: &mut Draft, event: &Event, now: Timestamp) {
    use Event as E;
    match event {
        E::SpecDraftAttemptStarted {
            attempt_id,
            attempt,
            trigger,
            reason,
            note,
            budget,
            ..
        } => {
            d.draft.state = SpecDraftState::Preparing;
            d.draft.attempt = *attempt;
            d.draft.current_attempt_id = Some(*attempt_id);
            d.draft.failure = None;
            d.draft.finished_at = None;
            d.retry_request = None;
            d.prior_usage = Default::default();
            d.attempt = Some(SpecDraftAttempt {
                id: *attempt_id,
                spec_draft_id: d.draft.id,
                attempt: *attempt,
                trigger: *trigger,
                reason: reason.clone(),
                note: note.clone(),
                state: DraftAttemptState::Running,
                base_sha: None,
                budget: *budget,
                budget_warnings: Vec::new(),
                resumed_lost: false,
                rounds: 0,
                packet_sha256: None,
                failure: None,
                started_at: now,
                ended_at: None,
            });
        }
        E::SpecDraftPacketRendered {
            attempt_id,
            base_sha,
            packet_sha256,
            ..
        } => {
            if let Some(a) = d.attempt.as_mut().filter(|a| a.id == *attempt_id) {
                a.base_sha = Some(base_sha.clone());
                a.packet_sha256 = Some(packet_sha256.clone());
                d.draft.state = SpecDraftState::Drafting;
            }
        }
        E::SpecDraftLinted {
            attempt_id,
            round,
            session_id,
            turn,
            outcome,
            content_hash,
            ..
        } => {
            if let Some(a) = d.attempt.as_mut().filter(|a| a.id == *attempt_id) {
                a.rounds = *round;
            }
            d.last_round = Some(RoundMark {
                attempt_id: *attempt_id,
                round: *round,
                session_id: *session_id,
                turn: *turn,
                outcome: *outcome,
            });
            d.has_content |= content_hash.is_some();
        }
        E::SpecDraftCommitting { .. } => d.draft.state = SpecDraftState::Committing,
        E::SpecDraftSucceeded {
            spec_id, commit, ..
        } => {
            d.draft.spec_id = Some(*spec_id);
            d.draft.commit_sha = Some(commit.clone());
            finish(
                d,
                SpecDraftState::Succeeded,
                DraftAttemptState::Succeeded,
                now,
            );
        }
        E::SpecDraftFailed {
            code,
            params,
            commit,
            ..
        } => {
            let failure = Coded {
                code: code.as_str().to_string(),
                params: params.clone(),
            };
            if commit.is_some() {
                d.draft.commit_sha.clone_from(commit);
            }
            d.draft.failure = Some(failure.clone());
            if let Some(a) = d
                .attempt
                .as_mut()
                .filter(|a| a.state == DraftAttemptState::Running)
            {
                a.failure = Some(failure);
            }
            finish(d, SpecDraftState::Failed, DraftAttemptState::Failed, now);
        }
        E::SpecDraftRetried { reason, note, .. } => {
            d.draft.state = SpecDraftState::Queued;
            d.draft.retries += 1;
            d.draft.queued_at = now;
            d.draft.failure = None;
            d.draft.finished_at = None;
            d.retry_request = Some(RetryRequest {
                reason: reason.clone(),
                note: note.clone(),
            });
        }
        E::SpecDraftCancelled { .. } => {
            d.draft.failure = None;
            finish(
                d,
                SpecDraftState::Cancelled,
                DraftAttemptState::Cancelled,
                now,
            );
        }
        E::BudgetWarned {
            owner, dimension, ..
        } => {
            if let Some(a) = current(d, owner)
                && !a.budget_warnings.contains(dimension)
            {
                a.budget_warnings.push(*dimension);
            }
        }
        E::SessionStarted {
            owner,
            resume_cause,
            ..
        } => {
            if *resume_cause == Some(ResumeCause::Lost)
                && let Some(a) = current(d, owner)
            {
                a.resumed_lost = true;
            }
            session::apply(d.run_parts(), event, now);
        }
        E::EnvironmentTornDown { environment_id, .. }
        | E::EnvironmentTeardownFailed { environment_id, .. }
            if d.retired_environments
                .iter()
                .any(|e| e.id == *environment_id) =>
        {
            let torn_down = matches!(event, E::EnvironmentTornDown { .. });
            if let Some(env) = d
                .retired_environments
                .iter_mut()
                .find(|e| e.id == *environment_id)
            {
                env.state = if torn_down {
                    EnvironmentState::TornDown
                } else {
                    EnvironmentState::TeardownFailed
                };
                env.updated_at = now;
            }
        }
        _ => {
            session::apply(d.run_parts(), event, now);
        }
    }
}

fn job(kind: JobKind, key: impl std::fmt::Display, payload: Value, now: Timestamp) -> Job {
    Job::new(kind, key, payload, now)
}

pub fn react(d: &Draft, event: &Event, now: Timestamp) -> Vec<Job> {
    use Event as E;
    match event {
        E::SpecDraftAttemptStarted { .. }
        | E::SpecDraftPacketRendered { .. }
        | E::SpecDraftCommitting { .. }
        | E::SpecDraftSucceeded { .. }
        | E::SpecDraftFailed { .. }
        | E::SpecDraftCancelled { .. } => phase_jobs(d, now),
        E::SessionTurnEnded {
            owner,
            session_id,
            turn,
            status,
            ..
        } if status == "success"
            && d.is_drafting()
            && d.live_session()
                .is_some_and(|s| s.id == *session_id && s.interrupt_requested.is_none()) =>
        {
            vec![lint_job(*owner, *session_id, *turn, now)]
        }
        _ => run_view(d)
            .map(|view| session::react(&view, event, now))
            .unwrap_or_default(),
    }
}

fn lint_job(owner: RunOwner, session_id: crate::ids::SessionId, turn: u32, now: Timestamp) -> Job {
    job(
        JobKind::DraftLint,
        format!("{session_id}:{turn}"),
        json!({ "owner": owner, "session_id": session_id, "turn": turn }),
        now,
    )
}

impl Draft {
    fn is_drafting(&self) -> bool {
        self.draft.state == SpecDraftState::Drafting
    }
}

pub fn phase_jobs(d: &Draft, now: Timestamp) -> Vec<Job> {
    let mut jobs = Vec::new();
    let owner = d.running_attempt().and_then(|_| d.run_owner());
    let finished = d.draft.state.is_finished();
    let current = d
        .environment
        .as_ref()
        .map(|e| (e, owner != Some(e.owner) || finished));
    let older = d.retired_environments.iter().map(|e| (e, true));
    for (env, retired) in current.into_iter().chain(older) {
        match env.state {
            EnvironmentState::TeardownFailed if retired => {
                let hour = now.0 / TEARDOWN_RETRY_INTERVAL_MS;
                jobs.push(job(
                    JobKind::EnvTeardown,
                    format!("{}:{hour}", env.id),
                    json!({ "owner": env.owner, "environment_id": env.id }),
                    now,
                ));
            }
            EnvironmentState::Ready | EnvironmentState::Preparing if retired => {
                jobs.push(job(
                    JobKind::EnvTeardown,
                    env.id,
                    json!({ "owner": env.owner, "environment_id": env.id }),
                    now,
                ));
            }
            _ => {}
        }
    }
    if let Some(s) = d
        .live_session()
        .filter(|s| owner != Some(s.owner) || !d.is_drafting())
    {
        jobs.push(session::stop_job(s, now));
    }
    let Some(owner) = owner else {
        return jobs;
    };
    let Some(attempt) = d.running_attempt() else {
        return jobs;
    };
    match d.draft.state {
        SpecDraftState::Preparing if !d.ready_environment().is_some_and(|e| e.owner == owner) => {
            jobs.push(job(
                JobKind::EnvPrepare,
                attempt.id,
                json!({ "owner": owner }),
                now,
            ));
        }
        SpecDraftState::Drafting => {
            if let Some(view) = run_view(d).filter(|v| resume::wants_session(v)) {
                let plan = resume::plan(&view);
                let key = match plan.as_ref().filter(|p| p.cause == ResumeCause::Lost) {
                    Some(p) => format!("{}:{}", attempt.id, p.from),
                    None => attempt.id.to_string(),
                };
                jobs.push(job(
                    JobKind::SessionStart,
                    key,
                    json!({
                        "owner": owner,
                        "environment_id": view.ready_environment,
                        "resume": plan.map(|p| json!({
                            "session_id": p.from,
                            "cause": p.cause,
                            "cli_session_id": p.cli_session_id,
                        })),
                    }),
                    now,
                ));
            }
            for p in &d.permissions {
                if matches!(p.state, PermissionState::Allowed | PermissionState::Denied)
                    && p.delivered_at.is_none()
                {
                    jobs.push(session::deliver_job(p, now));
                }
            }
            if let Some(s) = d.live_session().filter(|s| {
                s.owner == owner
                    && s.state == SessionState::Idle
                    && s.interrupt_requested.is_none()
                    && s.turns > 0
                    && s.recorded_turn == s.turns
            }) && !d
                .last_round
                .is_some_and(|m| m.session_id == s.id && m.turn >= s.turns)
            {
                jobs.push(lint_job(owner, s.id, s.turns, now));
            }
        }
        SpecDraftState::Committing => {
            jobs.push(job(
                JobKind::DraftCommit,
                attempt.id,
                json!({ "owner": owner }),
                now,
            ));
        }
        _ => {}
    }
    jobs
}

pub fn needs_tick(d: &Draft, cfg: &DraftConfig, now: Timestamp) -> bool {
    struct Probe;
    impl crate::ids::IdSource for Probe {
        fn next_id(&mut self) -> uuid::Uuid {
            uuid::Uuid::nil()
        }
    }
    if d.live_session().is_none() {
        return false;
    }
    let mut ids = Probe;
    let mut ctx = Ctx { now, ids: &mut ids };
    decide(d, DraftInput::Tick, cfg, &mut ctx).is_ok_and(|events| !events.is_empty())
}

#[derive(Debug, Clone, PartialEq)]
pub enum DraftAction {
    Admit(SpecDraftId),
    Tick(SpecDraftId),
    Enqueue(Job),
}

pub fn waiting(
    drafts: &[Draft],
    quotas: &[AccountQuota],
    cfg: &DraftConfig,
    now: Timestamp,
) -> Vec<(SpecDraftId, Vec<Waiting>)> {
    let quota = admission::quota_waits(
        quotas,
        &cfg.session.account,
        cfg.session.rate_limit_pause,
        now,
    );
    let mut queued: Vec<&Draft> = drafts
        .iter()
        .filter(|d| d.draft.state == SpecDraftState::Queued)
        .collect();
    queued.sort_by_key(|d| (d.draft.queued_at, d.draft.id));
    let active = drafts.iter().filter(|d| d.draft.state.holds_slot()).count();
    let free = cfg.max_active.saturating_sub(active);
    queued
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            let mut waits = Vec::new();
            if i >= free {
                waits.push(Waiting::ArchitectSlot);
            }
            waits.extend(quota.iter().cloned());
            (d.draft.id, waits)
        })
        .filter(|(_, waits)| !waits.is_empty())
        .collect()
}

pub fn reconcile(
    drafts: &[Draft],
    quotas: &[AccountQuota],
    cfg: &DraftConfig,
    now: Timestamp,
) -> Vec<DraftAction> {
    let waits = waiting(drafts, quotas, cfg, now);
    let mut queued: Vec<&Draft> = drafts
        .iter()
        .filter(|d| d.draft.state == SpecDraftState::Queued)
        .filter(|d| !waits.iter().any(|(id, _)| *id == d.draft.id))
        .collect();
    queued.sort_by_key(|d| (d.draft.queued_at, d.draft.id));
    let mut actions: Vec<DraftAction> = queued
        .into_iter()
        .map(|d| DraftAction::Admit(d.draft.id))
        .collect();
    for d in drafts {
        actions.extend(phase_jobs(d, now).into_iter().map(DraftAction::Enqueue));
        if needs_tick(d, cfg, now) {
            actions.push(DraftAction::Tick(d.draft.id));
        }
    }
    actions
}
