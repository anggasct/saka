#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail loudly on a broken setup"
)]

use std::collections::BTreeMap;

use saka_core::Ctx;
use saka_core::admission::Waiting;
use saka_core::event::{Event, Prompt};
use saka_core::ids::{
    EnvironmentId, IdSource, PermissionRequestId, ProjectId, RepoId, SessionId, SpecId,
    SpecRevisionId, WorkItemId,
};
use saka_core::job::{Job, JobKind};
use saka_core::model::{AccountQuota, Budget, PermissionRule, Session, Task};
use saka_core::policy::{
    Decision, DecisionSource, Effect, Policy, RequestPath, RuleSpec, ToolRequest,
};
use saka_core::react::{phase_jobs, react};
use saka_core::rejection::Rejection;
use saka_core::session::SessionInput;
use saka_core::time::Timestamp;
use saka_core::vocab::{
    BlockerCode, BudgetDimension, EnvironmentState, InterruptBy, MergeMethod, PermissionState,
    ResumeCause, ResumeReason, SessionState, Stage, StagePhase, WorkItemState,
};
use saka_core::{resume, rule, task};
use serde_json::{Value, json};
use uuid::Uuid;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

struct Counter(u128);

impl IdSource for Counter {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

fn budget(running_minutes: u32, turns: u32, tokens: i64, cost_usd_micros: i64) -> Budget {
    Budget {
        running_minutes,
        turns,
        tokens,
        cost_usd_micros,
    }
}

fn cfg_with(dev: Budget) -> task::LoopConfig {
    task::LoopConfig {
        max_followup_turns: 1,
        ci_appear_timeout_ms: 600_000,
        poll_interval_ms: 15_000,
        merge_method: MergeMethod::Squash,
        delete_branch_after_merge: true,
        commit_status: true,
        permission_wait_ms: 24 * HOUR,
        idle_ms: 30 * MINUTE,
        budgets: BTreeMap::from([(Stage::Dev, dev)]),
        budget_warn_percent: 80,
        rate_limit_pause: 0.9,
        account: "claude:default".into(),
    }
}

struct Run {
    task: Task,
    ids: Counter,
    now: Timestamp,
    jobs: Vec<Job>,
    cfg: task::LoopConfig,
    launches: u128,
}

impl Run {
    fn new(dev: Budget) -> Run {
        let now = Timestamp(10_000_000);
        let created = Event::WorkItemCreated {
            work_item_id: WorkItemId(Uuid::from_u128(1)),
            project_id: ProjectId(Uuid::from_u128(2)),
            spec_id: SpecId(Uuid::from_u128(3)),
            spec_revision_id: SpecRevisionId(Uuid::from_u128(4)),
            step_id: "S1".into(),
            step_index: 1,
            task_key: "tasks/s1".into(),
            work: "Add done".into(),
            branch: "saka/tasks/s1".into(),
            repo_id: RepoId(Uuid::from_u128(5)),
            priority: 0,
        };
        Run {
            task: task::evolve(None, &created, now).unwrap(),
            ids: Counter(100),
            now,
            jobs: Vec::new(),
            cfg: cfg_with(dev),
            launches: 0,
        }
    }

    fn try_decide(&mut self, input: task::TaskInput) -> Result<Vec<Event>, Rejection> {
        let mut ctx = Ctx {
            now: self.now,
            ids: &mut self.ids,
        };
        let events = task::decide(&self.task, input, &self.cfg, &mut ctx)?;
        let mut t = self.task.clone();
        for e in &events {
            t = task::evolve(Some(t), e, self.now).unwrap();
            self.jobs.extend(react(&t, e, &self.cfg, self.now));
        }
        self.task = t;
        Ok(events)
    }

    fn decide(&mut self, input: task::TaskInput) -> Vec<Event> {
        self.try_decide(input).unwrap()
    }

    fn take_jobs(&mut self) -> Vec<JobKind> {
        self.jobs.drain(..).map(|j| j.kind).collect()
    }

    fn wait(&mut self, ms: i64) {
        self.now = self.now.plus_ms(ms);
    }

    fn session(&self) -> &Session {
        self.task.session.as_ref().unwrap()
    }

    fn sid(&self) -> SessionId {
        self.session().id
    }

    fn blocker(&self) -> Option<(BlockerCode, Value)> {
        self.task.open_blocker().map(|b| (b.code, b.params.clone()))
    }

    fn launch(&mut self) -> Vec<Event> {
        self.launches += 1;
        self.decide(task::TaskInput::Session(SessionInput::Launched {
            session_id: SessionId(Uuid::from_u128(600 + self.launches)),
            cli: "claude".into(),
            cli_version: Some("2.1.0".into()),
            model: None,
            account: "default".into(),
            runner_unit: format!("saka-session-{}", self.launches),
            socket_path: "/run/s.sock".into(),
            log_path: "/data/s/events.ndjson".into(),
            author: saka_core::authorship::SessionAuthor::saka(),
        }))
    }

    fn prepare(&mut self, env: u128) {
        self.decide(task::TaskInput::EnvironmentPrepared {
            environment_id: EnvironmentId(Uuid::from_u128(env)),
            root_path: "/envs/1".into(),
            worktree_path: "/envs/1/worktree".into(),
            base_sha: "a".repeat(40),
            setup: Default::default(),
        });
    }

    fn running(dev: Budget) -> Run {
        let mut r = Run::new(dev);
        r.decide(task::TaskInput::Admit);
        r.prepare(500);
        r.launch();
        r.initialize();
        r.accept(1);
        r.jobs.clear();
        r
    }

    fn initialize(&mut self) {
        let cli = format!("cli-{}", self.launches);
        self.decide(task::TaskInput::Session(SessionInput::Initialized {
            cli_session_id: cli,
            model: Some("sonnet".into()),
        }));
    }

    fn accept(&mut self, turn: u32) {
        self.decide(task::TaskInput::Session(SessionInput::TurnAccepted {
            turn,
        }));
    }

    fn end_turn(&mut self, turn: u32, status: &str) -> Vec<Event> {
        self.decide(task::TaskInput::Session(SessionInput::TurnEnded {
            turn,
            status: status.into(),
            cost_usd_micros: Some(1_000),
            input_tokens: Some(100),
            output_tokens: Some(50),
        }))
    }

    fn ask(&mut self, id: u128, command: &str) {
        self.decide(task::TaskInput::Session(
            SessionInput::PermissionRequested {
                permission_request_id: PermissionRequestId(Uuid::from_u128(id)),
                cli_request_id: format!("req-{id}"),
                tool: "Bash".into(),
                summary: command.into(),
                input: json!({ "command": command }),
                decision: Decision {
                    effect: Effect::Ask,
                    source: DecisionSource::Default,
                    message: None,
                },
            },
        ));
    }

    fn answer(
        &mut self,
        id: u128,
        allow: bool,
        remember: bool,
        active: Vec<PermissionRule>,
    ) -> Result<Vec<Event>, Rejection> {
        self.try_decide(task::TaskInput::Session(SessionInput::AnswerPermission {
            permission_request_id: PermissionRequestId(Uuid::from_u128(id)),
            allow,
            message: None,
            actor: "operator:local".into(),
            remember,
            active_rules: active,
        }))
    }

    fn steer(&mut self, text: &str) -> Result<Vec<Event>, Rejection> {
        let session_id = self.sid();
        self.try_decide(task::TaskInput::Session(SessionInput::Steer {
            session_id,
            text: text.into(),
        }))
    }

    fn interrupt(&mut self) -> Result<Vec<Event>, Rejection> {
        let session_id = self.sid();
        self.try_decide(task::TaskInput::Session(SessionInput::Interrupt {
            session_id,
        }))
    }

    fn stop(&mut self, reason: &str) -> Result<Vec<Event>, Rejection> {
        let session_id = self.sid();
        self.try_decide(task::TaskInput::Session(SessionInput::Stop {
            session_id,
            reason: reason.into(),
        }))
    }

    fn start_jobs(&self) -> Vec<Job> {
        phase_jobs(&self.task, &self.cfg, self.now)
            .into_iter()
            .filter(|j| j.kind == JobKind::SessionStart)
            .collect()
    }
}

fn unlimited() -> Budget {
    budget(60, 10, 0, 0)
}

fn invalid(state: &str) -> Rejection {
    Rejection::InvalidState {
        state: state.into(),
    }
}

#[test]
fn steer_reaches_a_running_turn_and_starts_a_new_turn_from_idle() {
    let mut r = Run::running(unlimited());
    let sid = r.sid();
    let events = r.steer("use the existing helper").unwrap();
    assert_eq!(
        events,
        vec![Event::SessionSteered {
            owner: r.session().owner,
            session_id: sid,
            text: "use the existing helper".into(),
            turn: 1,
            n: 1,
        }]
    );
    assert_eq!(r.session().turns, 1);
    assert_eq!(
        r.jobs
            .iter()
            .map(|j| j.idempotency_key.clone())
            .collect::<Vec<_>>(),
        vec![format!("session.steer:{sid}:1")]
    );
    r.jobs.clear();

    r.end_turn(1, "success");
    assert_eq!(r.session().state, SessionState::Idle);
    r.jobs.clear();
    let events = r.steer("now add the test").unwrap();
    assert!(matches!(
        &events[..],
        [
            Event::SessionSteered { turn: 2, n: 2, .. },
            Event::SessionTurnRequested { turn: 2, prompt: Prompt::Steer { text }, .. },
        ] if text == "now add the test"
    ));
    assert_eq!(r.take_jobs(), vec![JobKind::SessionSendTurn]);
    r.accept(2);
    assert_eq!(r.session().state, SessionState::Running);
    assert_eq!(r.session().steers, 2);
}

#[test]
fn an_operator_interrupt_leaves_the_session_idle_waiting_for_a_steer() {
    let mut r = Run::running(unlimited());
    let sid = r.sid();
    r.interrupt().unwrap();
    assert_eq!(r.session().state, SessionState::Interrupted);
    assert_eq!(r.session().interrupt_requested, Some(InterruptBy::Operator));
    assert_eq!(
        r.jobs
            .iter()
            .map(|j| j.idempotency_key.clone())
            .collect::<Vec<_>>(),
        vec![format!("session.interrupt:{sid}:1")]
    );
    r.jobs.clear();

    r.end_turn(1, "error_during_execution");
    assert_eq!(r.session().state, SessionState::Idle);
    assert!(r.session().idle_since.is_some());
    assert_eq!(r.blocker(), None);
    assert!(
        !r.take_jobs().contains(&JobKind::EnvInspectWorktree),
        "an interrupted turn is not inspected"
    );
    assert_eq!(
        r.decide(task::TaskInput::WorktreeInspected {
            turn: 1,
            dirty: false,
            commits_ahead: 1,
            head_sha: "b".repeat(40),
            outside: None,
        }),
        vec![]
    );

    r.steer("try the other approach").unwrap();
    assert_eq!(r.session().interrupt_requested, None);
    assert_eq!(r.session().turns, 2);
}

#[test]
fn stop_fails_the_session_from_every_live_state() {
    let starting = || {
        let mut r = Run::new(unlimited());
        r.decide(task::TaskInput::Admit);
        r.prepare(500);
        r.launch();
        r
    };
    let running = || Run::running(unlimited());
    let waiting = || {
        let mut r = Run::running(unlimited());
        r.ask(900, "npm install");
        r
    };
    let idle = || {
        let mut r = Run::running(unlimited());
        r.end_turn(1, "success");
        r
    };
    let interrupted = || {
        let mut r = Run::running(unlimited());
        r.interrupt().unwrap();
        r
    };
    let cases: [(SessionState, &dyn Fn() -> Run); 5] = [
        (SessionState::Starting, &starting),
        (SessionState::Running, &running),
        (SessionState::Waiting, &waiting),
        (SessionState::Idle, &idle),
        (SessionState::Interrupted, &interrupted),
    ];
    for (state, make) in cases {
        let mut r = make();
        assert_eq!(r.session().state, state);
        r.jobs.clear();
        let events = r.stop("wrong approach").unwrap();
        assert!(
            matches!(events[0], Event::SessionStopRequested { .. }),
            "{state}"
        );
        let s = r.session();
        assert_eq!(s.state, SessionState::Failed, "{state}");
        assert_eq!(
            s.end_reason
                .as_ref()
                .map(|e| (e.code.as_str(), e.params.clone())),
            Some(("operator_stop", json!({ "reason": "wrong approach" })))
        );
        assert_eq!(
            r.blocker(),
            Some((
                BlockerCode::SessionFailed,
                json!({ "cause": "operator_stop", "reason": "wrong approach" })
            )),
            "{state}"
        );
        assert!(
            r.task
                .permissions
                .iter()
                .all(|p| p.state == PermissionState::Expired),
            "{state}"
        );
        assert_eq!(r.take_jobs(), vec![JobKind::SessionStop], "{state}");
        assert_eq!(r.stop("again"), Err(invalid("failed")));
    }
}

#[test]
fn operator_actions_outside_their_states_are_refused() {
    let mut r = Run::running(unlimited());
    r.end_turn(1, "success");
    assert_eq!(r.interrupt(), Err(invalid("idle")));
    let other = SessionId(Uuid::from_u128(4242));
    assert_eq!(
        r.try_decide(task::TaskInput::Session(SessionInput::Interrupt {
            session_id: other
        })),
        Err(Rejection::NotFound)
    );

    let mut w = Run::running(unlimited());
    w.ask(900, "npm install");
    assert_eq!(w.steer("x"), Err(invalid("waiting")));
    assert_eq!(w.interrupt(), Err(invalid("waiting")));

    let mut s = Run::new(unlimited());
    s.decide(task::TaskInput::Admit);
    s.prepare(500);
    s.launch();
    assert_eq!(s.steer("x"), Err(invalid("starting")));
}

#[test]
fn an_idle_session_past_idle_minutes_times_out() {
    let mut r = Run::running(unlimited());
    r.interrupt().unwrap();
    r.end_turn(1, "error_during_execution");
    r.wait(29 * MINUTE);
    assert_eq!(r.decide(task::TaskInput::Tick), vec![]);
    assert!(!task::needs_tick(&r.task, &r.cfg, r.now));
    r.wait(2 * MINUTE);
    assert!(task::needs_tick(&r.task, &r.cfg, r.now));
    r.jobs.clear();
    r.decide(task::TaskInput::Tick);
    assert_eq!(
        r.blocker(),
        Some((
            BlockerCode::SessionTimeout,
            json!({ "cause": "idle", "minutes": 30 })
        ))
    );
    assert_eq!(r.session().state, SessionState::Failed);
    assert_eq!(r.session().idle_since, None);
    assert_eq!(r.take_jobs(), vec![JobKind::SessionStop]);
}

#[test]
fn waiting_and_idle_time_do_not_count_as_running() {
    let mut r = Run::running(unlimited());
    r.wait(10 * MINUTE);
    r.ask(900, "npm install");
    r.wait(2 * HOUR);
    r.answer(900, true, false, vec![]).unwrap();
    r.wait(5 * MINUTE);
    let events = r.end_turn(1, "success");
    assert!(matches!(
        events[0],
        Event::SessionTurnEnded { running_ms, .. } if running_ms == 15 * MINUTE
    ));
    r.wait(20 * MINUTE);
    assert_eq!(r.session().running_ms_at(r.now), 15 * MINUTE);
    assert_eq!(r.task.usage_records.len(), 1);
    assert_eq!(r.task.usage_records[0].running_ms, 15 * MINUTE);
    assert_eq!(r.task.usage_records[0].account, "claude:default");
}

#[test]
fn the_first_loss_resumes_the_session_and_the_second_blocks() {
    let mut r = Run::running(unlimited());
    let first = r.sid();
    let run = r.task.stage_run.as_ref().unwrap().id;
    r.decide(task::TaskInput::Session(SessionInput::CliExited {
        code: Some(137),
        signal: None,
    }));
    assert_eq!(r.session().state, SessionState::Lost);
    assert_eq!(r.blocker(), None);
    assert_eq!(
        r.task.item.stage_phase,
        Some(StagePhase::RunningSession),
        "the task stays in the session phase"
    );
    let start = r.start_jobs();
    assert_eq!(start.len(), 1);
    assert_eq!(
        start[0].idempotency_key,
        format!("session.start:{run}:{first}")
    );
    assert_eq!(
        start[0].payload["resume"],
        json!({ "session_id": first, "cause": "lost", "cli_session_id": "cli-1" })
    );

    let events = r.launch();
    assert!(matches!(
        &events[0],
        Event::SessionStateChanged { session_id, state: SessionState::Resumed, .. } if *session_id == first
    ));
    assert!(matches!(
        &events[1],
        Event::SessionStarted { resumed_from_session_id: Some(from), resume_cause: Some(ResumeCause::Lost), .. }
            if *from == first
    ));
    assert!(matches!(
        &events[2],
        Event::SessionTurnRequested {
            turn: 1,
            prompt: Prompt::Resume {
                cause: ResumeReason::RunnerLost,
                retry_of: None
            },
            ..
        }
    ));
    assert_eq!(r.task.retired_sessions[0].state, SessionState::Resumed);
    assert!(r.task.stage_run.as_ref().unwrap().resumed_lost);
    assert!(r.start_jobs().is_empty());

    r.decide(task::TaskInput::Session(SessionInput::CliExited {
        code: None,
        signal: Some(9),
    }));
    assert_eq!(
        r.blocker(),
        Some((
            BlockerCode::SessionLost,
            json!({ "cause": "runner_exited" })
        ))
    );
    assert!(r.start_jobs().is_empty());
}

#[test]
fn a_session_lost_before_init_restarts_from_the_work_packet() {
    let mut r = Run::new(unlimited());
    r.decide(task::TaskInput::Admit);
    r.prepare(500);
    r.launch();
    let first = r.sid();
    r.decide(task::TaskInput::Session(SessionInput::Gone {
        cause: "runner_silent".into(),
    }));
    assert_eq!(
        r.start_jobs()[0].payload["resume"]["cli_session_id"],
        Value::Null
    );
    let events = r.launch();
    assert!(matches!(
        &events[1],
        Event::SessionStarted { resumed_from_session_id: Some(from), resume_cause: Some(ResumeCause::Lost), .. }
            if *from == first
    ));
    assert!(matches!(
        &events[2],
        Event::SessionTurnRequested {
            prompt: Prompt::WorkPacket { retry_of: None },
            ..
        }
    ));
}

#[test]
fn retry_from_each_session_blocker_resumes_when_the_session_can_continue() {
    let cases = [
        (
            BlockerCode::SessionLost,
            json!({ "cause": "runner_exited" }),
            ResumeReason::RunnerLost,
        ),
        (
            BlockerCode::SessionFailed,
            json!({ "cause": "operator_stop", "reason": "x" }),
            ResumeReason::OperatorStopped,
        ),
        (
            BlockerCode::SessionFailed,
            json!({ "subtype": "error_during_execution" }),
            ResumeReason::Retried,
        ),
        (
            BlockerCode::SessionTimeout,
            json!({ "cause": "idle", "minutes": 30 }),
            ResumeReason::IdleTimeout,
        ),
        (
            BlockerCode::PermissionExpired,
            json!({ "tool": "Bash", "summary": "x", "hours": 24 }),
            ResumeReason::PermissionExpired,
        ),
        (
            BlockerCode::BudgetExhausted,
            json!({ "dimension": "turns", "used": 10, "limit": 10 }),
            ResumeReason::BudgetExhausted,
        ),
        (
            BlockerCode::QuotaExhausted,
            json!({ "account": "claude:default", "window": "five_hour", "resets_at": null }),
            ResumeReason::QuotaExhausted,
        ),
    ];
    let mut base = Run::running(unlimited());
    base.stop("swap the blocker").unwrap();
    let last = base.sid();
    for (code, params, reason) in cases {
        let mut r = Run {
            task: base.task.clone(),
            ids: Counter(5_000),
            now: base.now,
            jobs: Vec::new(),
            cfg: base.cfg.clone(),
            launches: base.launches,
        };
        let b = r.task.blocker.as_mut().unwrap();
        b.code = code;
        b.params = params.clone();
        r.decide(task::TaskInput::Retry {
            reason: "try again".into(),
        });
        let run = r.task.stage_run.as_ref().unwrap();
        assert_eq!(
            (run.attempt, run.entry_phase),
            (2, StagePhase::RunningSession)
        );
        assert_eq!(run.budget, Some(unlimited()), "a retry gets a full budget");
        let start = r.start_jobs();
        assert_eq!(
            start[0].payload["resume"]["cause"],
            json!("retry"),
            "{code}"
        );
        let events = r.launch();
        assert!(
            matches!(
                &events[0],
                Event::SessionStarted { resumed_from_session_id: Some(from), resume_cause: Some(ResumeCause::Retry), .. }
                    if *from == last
            ),
            "{code}"
        );
        match &events[1] {
            Event::SessionTurnRequested {
                prompt:
                    Prompt::Resume {
                        cause,
                        retry_of: Some(of),
                    },
                ..
            } => {
                assert_eq!(*cause, reason, "{code}");
                assert_eq!((of.code.as_str(), &of.params), (code.as_str(), &params));
            }
            other => panic!("{code}: {other:?}"),
        }
    }

    let mut fresh = Run::new(unlimited());
    fresh.decide(task::TaskInput::Admit);
    fresh.prepare(500);
    fresh.launch();
    fresh.stop("before init").unwrap();
    fresh.decide(task::TaskInput::Retry {
        reason: "again".into(),
    });
    let events = fresh.launch();
    assert!(
        matches!(
            &events[0],
            Event::SessionStarted {
                resumed_from_session_id: None,
                resume_cause: None,
                ..
            }
        ),
        "no cli session to resume"
    );
    assert!(matches!(
        &events[1],
        Event::SessionTurnRequested {
            prompt: Prompt::WorkPacket { retry_of: Some(_) },
            ..
        }
    ));

    let mut moved = Run {
        task: base.task.clone(),
        ids: Counter(9_000),
        now: base.now,
        jobs: Vec::new(),
        cfg: base.cfg.clone(),
        launches: base.launches,
    };
    moved.task.environment.as_mut().unwrap().state = EnvironmentState::Failed;
    moved.decide(task::TaskInput::Retry {
        reason: "again".into(),
    });
    assert_eq!(moved.task.item.state, WorkItemState::Admitted);
    moved.prepare(501);
    let events = moved.launch();
    assert!(
        matches!(
            &events[0],
            Event::SessionStarted {
                resumed_from_session_id: None,
                ..
            }
        ),
        "a new environment starts a plain session"
    );
}

#[test]
fn the_resume_turn_names_the_cause_and_keeps_the_step() {
    let phrases = [
        (ResumeReason::RunnerLost, "the runner was lost"),
        (ResumeReason::OperatorStopped, "the operator stopped it"),
        (
            ResumeReason::IdleTimeout,
            "the operator interrupted it and did not answer in time",
        ),
        (
            ResumeReason::PermissionExpired,
            "a permission request expired",
        ),
        (ResumeReason::BudgetExhausted, "its budget ran out"),
        (
            ResumeReason::QuotaExhausted,
            "the agent account hit its rate limit",
        ),
    ];
    for (reason, phrase) in phrases {
        let text = resume::text(reason);
        assert!(
            text.starts_with(&format!("Your previous session stopped ({phrase}).")),
            "{text}"
        );
        assert!(text.contains("Check `git status` and the last commits"));
        assert!(text.contains("acceptance criteria, and its Done when commands are unchanged"));
        assert!(text.ends_with("Do not push."));
    }
}

#[test]
fn a_permission_pending_past_the_wait_limit_expires_and_blocks() {
    let mut r = Run::running(unlimited());
    r.ask(900, "npm install");
    r.wait(24 * HOUR);
    assert_eq!(r.decide(task::TaskInput::Tick), vec![]);
    r.wait(1);
    let sid = r.sid();
    let events = r.decide(task::TaskInput::Tick);
    assert!(matches!(
        &events[0],
        Event::PermissionAnswered { state: PermissionState::Expired, answer_source, message: Some(m), .. }
            if answer_source == "system:wait_limit" && m == "The permission request expired."
    ));
    assert!(matches!(
        &events[1],
        Event::SessionInterruptRequested {
            actor: InterruptBy::Saka,
            ..
        }
    ));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Event::PermissionAnswered { .. }))
            .count(),
        1,
        "the expired request is answered once"
    );
    assert_eq!(
        r.blocker(),
        Some((
            BlockerCode::PermissionExpired,
            json!({ "tool": "Bash", "summary": "npm install", "hours": 24 })
        ))
    );
    assert_eq!(r.session().state, SessionState::Failed);
    let jobs: Vec<(JobKind, Value)> = r.jobs.iter().map(|j| (j.kind, j.payload.clone())).collect();
    assert_eq!(jobs[0].0, JobKind::SessionDeliverPermissionAnswer);
    assert_eq!(jobs[0].1["behavior"], json!("deny"));
    assert_eq!(
        jobs.iter().map(|j| j.0).collect::<Vec<_>>(),
        vec![
            JobKind::SessionDeliverPermissionAnswer,
            JobKind::SessionInterrupt,
            JobKind::SessionStop
        ]
    );
    let stop = r
        .jobs
        .iter()
        .find(|j| j.kind == JobKind::SessionStop)
        .unwrap();
    assert!(stop.run_at > r.now, "the stop waits for the interrupt");
    assert_eq!(
        r.jobs[1].idempotency_key,
        format!("session.interrupt:{sid}:1")
    );
}

fn policy() -> Policy {
    Policy::compile(&[
        RuleSpec {
            id: "no-deploy".into(),
            effect: Effect::Deny,
            tools: vec!["Bash".into()],
            paths: vec![],
            commands: vec!["make deploy*".into()],
        },
        RuleSpec {
            id: "ask-curl".into(),
            effect: Effect::Ask,
            tools: vec!["Bash".into()],
            paths: vec![],
            commands: vec!["curl *".into()],
        },
    ])
    .unwrap()
}

fn bash(command: &str) -> ToolRequest {
    ToolRequest {
        tool: "Bash".into(),
        command: Some(command.into()),
        path: None,
    }
}

#[test]
fn remember_makes_an_exact_project_rule_that_never_beats_a_deny() {
    let mut r = Run::running(unlimited());
    r.ask(900, "npm install");
    let events = r.answer(900, true, true, vec![]).unwrap();
    let added = events
        .iter()
        .find_map(|e| match e {
            Event::PermissionRuleAdded {
                tool,
                command,
                created_by,
                permission_request_id,
                ..
            } => Some((
                tool.clone(),
                command.clone(),
                created_by.clone(),
                *permission_request_id,
            )),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        added,
        (
            "Bash".into(),
            Some("npm install".into()),
            "human:operator:local".into(),
            PermissionRequestId(Uuid::from_u128(900))
        )
    );
    assert!(matches!(
        events[0],
        Event::PermissionAnswered {
            state: PermissionState::Allowed,
            ..
        }
    ));
    let rules = r.task.added_rules.clone();
    assert_eq!(rules.len(), 1);
    let id = rules[0].id;

    let p = policy();
    let decision = p.evaluate_with(&bash("npm install"), &rules);
    assert_eq!(decision.effect, Effect::Allow);
    assert_eq!(
        decision.source.answer_source(),
        Some(format!("policy:project:{id}"))
    );
    assert_eq!(
        p.evaluate_with(&bash("npm installx"), &rules).effect,
        Effect::Ask
    );
    assert_eq!(
        p.evaluate_with(&bash("npm  install"), &rules).effect,
        Effect::Ask
    );

    let wide = vec![
        PermissionRule {
            command: Some("make deploy-prod".into()),
            ..rules[0].clone()
        },
        PermissionRule {
            command: Some("git push".into()),
            ..rules[0].clone()
        },
        PermissionRule {
            tool: "Edit".into(),
            command: None,
            ..rules[0].clone()
        },
        PermissionRule {
            command: Some("curl https://example.test".into()),
            ..rules[0].clone()
        },
    ];
    assert_eq!(
        p.evaluate_with(&bash("make deploy-prod"), &wide).source,
        DecisionSource::Rule("no-deploy".into())
    );
    assert_eq!(
        p.evaluate_with(&bash("git push"), &wide).source,
        DecisionSource::Builtin("git_push".into())
    );
    let outside = ToolRequest {
        tool: "Edit".into(),
        command: None,
        path: Some(RequestPath {
            path: "/etc/passwd".into(),
            inside_worktree: false,
            through_symlink: false,
        }),
    };
    assert_eq!(p.evaluate_with(&outside, &wide).effect, Effect::Deny);
    assert_eq!(
        p.evaluate_with(&bash("curl https://example.test"), &wide)
            .effect,
        Effect::Allow,
        "a project rule comes before the operator's ask rules"
    );

    let mut again = Run::running(unlimited());
    again.ask(901, "npm install");
    let events = again.answer(901, true, true, rules.clone()).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::PermissionRuleAdded { .. })),
        "an identical active rule is not added twice"
    );

    let mut file = Run::running(unlimited());
    file.decide(task::TaskInput::Session(
        SessionInput::PermissionRequested {
            permission_request_id: PermissionRequestId(Uuid::from_u128(902)),
            cli_request_id: "req-902".into(),
            tool: "Write".into(),
            summary: "src/a.rs".into(),
            input: json!({ "file_path": "src/a.rs" }),
            decision: Decision {
                effect: Effect::Ask,
                source: DecisionSource::Default,
                message: None,
            },
        },
    ));
    file.answer(902, true, true, vec![]).unwrap();
    assert_eq!(
        (
            file.task.added_rules[0].tool.as_str(),
            file.task.added_rules[0].command.clone()
        ),
        ("Write", None)
    );
}

#[test]
fn remember_is_refused_for_a_deny_or_a_redacted_command() {
    let mut r = Run::running(unlimited());
    r.ask(900, "npm install");
    assert_eq!(
        r.answer(900, false, true, vec![]),
        Err(Rejection::Invalid {
            field: "remember",
            code: "allow_only"
        })
    );
    r.ask(
        901,
        "curl -H 'Authorization: Bearer «secret:auth_header»' x",
    );
    assert_eq!(
        r.answer(901, true, true, vec![]),
        Err(Rejection::Invalid {
            field: "remember",
            code: "contains_secret"
        })
    );
    assert!(
        r.task
            .permissions
            .iter()
            .all(|p| p.state == PermissionState::Pending)
    );
}

#[test]
fn a_revoked_rule_no_longer_answers_and_cannot_be_revoked_twice() {
    let mut r = Run::running(unlimited());
    r.ask(900, "npm install");
    r.answer(900, true, true, vec![]).unwrap();
    let added = r.task.added_rules[0].clone();
    let events = rule::revoke(&added, "too broad".into()).unwrap();
    let revoked = rule::evolve(Some(added.clone()), &events[0], r.now).unwrap();
    assert_eq!(revoked.revoke_reason.as_deref(), Some("too broad"));
    assert!(revoked.revoked_at.is_some());
    assert_eq!(
        policy()
            .evaluate_with(&bash("npm install"), std::slice::from_ref(&revoked))
            .effect,
        Effect::Ask
    );
    assert_eq!(
        rule::revoke(&revoked, "again".into()),
        Err(invalid("revoked"))
    );
}

#[test]
fn spend_budgets_stop_at_turn_end_after_one_warning() {
    let mut r = Run::running(budget(60, 10, 1_000, 0));
    let events = r.end_turn(1, "success");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::BudgetWarned { .. }))
    );
    r.steer("more").unwrap();
    r.accept(2);
    let mut r2 = r;
    let ended = r2.decide(task::TaskInput::Session(SessionInput::TurnEnded {
        turn: 2,
        status: "success".into(),
        cost_usd_micros: None,
        input_tokens: Some(650),
        output_tokens: Some(0),
    }));
    let warned: Vec<&Event> = ended
        .iter()
        .filter(|e| matches!(e, Event::BudgetWarned { .. }))
        .collect();
    assert!(matches!(
        warned[..],
        [Event::BudgetWarned { dimension: BudgetDimension::Tokens, used, limit, .. }]
            if *used == json!(800) && *limit == json!(1000)
    ));
    assert_eq!(
        r2.task.stage_run.as_ref().unwrap().budget_warnings,
        vec![BudgetDimension::Tokens]
    );
    r2.steer("more").unwrap();
    r2.accept(3);
    r2.jobs.clear();
    let last = r2.decide(task::TaskInput::Session(SessionInput::TurnEnded {
        turn: 3,
        status: "success".into(),
        cost_usd_micros: None,
        input_tokens: Some(150),
        output_tokens: Some(50),
    }));
    assert!(!last.iter().any(|e| matches!(e, Event::BudgetWarned { .. })));
    assert_eq!(
        r2.blocker(),
        Some((
            BlockerCode::BudgetExhausted,
            json!({ "dimension": "tokens", "used": 1000, "limit": 1000 })
        ))
    );
    assert_eq!(r2.session().state, SessionState::Failed);
    let jobs = r2.take_jobs();
    assert!(jobs.contains(&JobKind::SessionStop));
    assert!(
        !jobs.contains(&JobKind::SessionInterrupt),
        "no interrupt once the turn has ended"
    );
}

#[test]
fn the_turn_budget_is_checked_before_the_next_turn_is_sent() {
    let mut r = Run::running(budget(60, 1, 0, 0));
    r.end_turn(1, "success");
    let events = r.steer("one more").unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::SessionSteered { .. }))
    );
    assert_eq!(
        r.blocker(),
        Some((
            BlockerCode::BudgetExhausted,
            json!({ "dimension": "turns", "used": 1, "limit": 1 })
        ))
    );

    let mut f = Run::running(budget(60, 1, 0, 0));
    f.end_turn(1, "success");
    f.decide(task::TaskInput::WorktreeInspected {
        turn: 1,
        dirty: true,
        commits_ahead: 0,
        head_sha: "b".repeat(40),
        outside: None,
    });
    assert_eq!(f.blocker().map(|b| b.0), Some(BlockerCode::BudgetExhausted));
}

#[test]
fn the_running_budget_counts_every_session_of_the_stage_run() {
    let mut r = Run::running(budget(30, 10, 0, 0));
    r.wait(20 * MINUTE);
    r.decide(task::TaskInput::Session(SessionInput::CliExited {
        code: Some(1),
        signal: None,
    }));
    r.launch();
    r.initialize();
    r.accept(1);
    assert_eq!(r.task.prior_usage.running_ms, 20 * MINUTE);
    r.wait(4 * MINUTE);
    let warned = r.decide(task::TaskInput::Tick);
    assert!(matches!(
        &warned[..],
        [Event::BudgetWarned { dimension: BudgetDimension::RunningMinutes, used, .. }] if *used == json!(24)
    ));
    assert_eq!(r.decide(task::TaskInput::Tick), vec![], "warned once");
    r.wait(6 * MINUTE);
    r.decide(task::TaskInput::Tick);
    assert_eq!(
        r.blocker(),
        Some((
            BlockerCode::BudgetExhausted,
            json!({ "dimension": "running_minutes", "used": 30, "limit": 30 })
        ))
    );
}

#[test]
fn a_zero_limit_never_stops_the_session() {
    let mut r = Run::running(budget(60, 10, 0, 0));
    for turn in 1..=3 {
        let events = r.decide(task::TaskInput::Session(SessionInput::TurnEnded {
            turn,
            status: "success".into(),
            cost_usd_micros: Some(50_000_000),
            input_tokens: Some(9_000_000),
            output_tokens: Some(9_000_000),
        }));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Event::BudgetWarned { .. }))
        );
        r.steer("go on").unwrap();
        r.accept(turn + 1);
    }
    assert_eq!(r.blocker(), None);
}

#[test]
fn only_a_refused_rate_limit_stops_the_session_as_quota_exhausted() {
    let mut r = Run::running(unlimited());
    let observe = |status: &str, refused: bool| {
        task::TaskInput::Session(SessionInput::QuotaObserved {
            window: "five_hour".into(),
            utilization: 0.95,
            status: status.into(),
            resets_at: Some(Timestamp(99_000_000)),
            refused,
        })
    };
    for status in ["allowed_warning", "rejected"] {
        let events = r.decide(observe(status, false));
        assert!(
            matches!(&events[..], [Event::QuotaObserved { account, .. }] if account == "claude:default"),
            "{status}"
        );
        assert_eq!(r.blocker(), None);
        assert_eq!(r.session().interrupt_requested, None);
    }
    assert_eq!(r.task.quota_observations.len(), 2);
    r.decide(observe("rejected", true));
    assert_eq!(
        r.blocker(),
        Some((
            BlockerCode::QuotaExhausted,
            json!({ "account": "claude:default", "window": "five_hour", "resets_at": 99_000_000 })
        ))
    );
    assert_eq!(r.session().interrupt_requested, Some(InterruptBy::Saka));
}

#[test]
fn a_busy_account_holds_queued_tasks_until_the_window_resets() {
    let now = Timestamp(1_000);
    let quota = |utilization: f64, resets_at: i64| AccountQuota {
        account: "claude:default".into(),
        window: "seven_day".into(),
        utilization,
        status: "allowed".into(),
        resets_at: Some(Timestamp(resets_at)),
        observed_at: now,
    };
    let waits =
        saka_core::admission::quota_waits(&[quota(0.93, 5_000)], "claude:default", 0.9, now);
    assert_eq!(
        waits,
        vec![Waiting::Quota {
            account: "claude:default".into(),
            window: "seven_day".into(),
            utilization: 0.93,
            resets_at: Some(Timestamp(5_000)),
        }]
    );
    for quiet in [quota(0.5, 5_000), quota(0.95, 500)] {
        assert!(saka_core::admission::quota_waits(&[quiet], "claude:default", 0.9, now).is_empty());
    }
    assert!(
        saka_core::admission::quota_waits(&[quota(0.99, 5_000)], "codex:other", 0.9, now)
            .is_empty()
    );
}

#[test]
fn session_actions_follow_the_state() {
    let mut r = Run::running(unlimited());
    assert_eq!(
        saka_core::session::session_actions(r.session()),
        vec!["steer", "interrupt", "stop"]
    );
    r.ask(900, "npm install");
    assert_eq!(
        saka_core::session::session_actions(r.session()),
        vec!["stop"]
    );
    r.answer(900, true, false, vec![]).unwrap();
    r.end_turn(1, "success");
    assert_eq!(
        saka_core::session::session_actions(r.session()),
        vec!["steer", "stop"]
    );
    r.stop("done").unwrap();
    assert!(saka_core::session::session_actions(r.session()).is_empty());
}

#[test]
fn a_turn_that_ends_before_its_send_is_recorded_still_ends_once() {
    let mut r = Run::new(unlimited());
    r.decide(task::TaskInput::Admit);
    r.prepare(500);
    r.launch();
    r.initialize();
    r.end_turn(1, "success");
    assert_eq!(r.session().state, SessionState::Idle);
    assert_eq!(r.session().recorded_turn, 1);
    assert_eq!(
        r.decide(task::TaskInput::Session(SessionInput::TurnAccepted {
            turn: 1
        })),
        vec![]
    );
    assert_eq!(r.session().state, SessionState::Idle);
    assert_eq!(
        r.end_turn(1, "success"),
        vec![],
        "a replayed end is ignored"
    );
}
