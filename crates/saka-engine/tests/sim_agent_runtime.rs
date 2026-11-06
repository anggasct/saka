#![allow(
    clippy::unwrap_used,
    reason = "simulations fail loudly on a broken setup"
)]

use saka_core::admission::Waiting;
use saka_core::model::{Budget, Session};
use saka_core::vocab::{
    BlockerCode, BudgetDimension, ResumeCause, SessionState, Stage, WorkItemState,
};
use saka_testkit::{AgentOutcome, AgentScript, Operator, Sim, SimConfig};
use serde_json::json;

const STEPS: usize = 5_000;

fn sim(agent: AgentScript) -> Sim {
    let mut sim = Sim::new(SimConfig {
        agent,
        operator: Operator {
            start: true,
            merge: true,
            retry: vec![],
            answer: Some(true),
        },
        ..SimConfig::without_review()
    });
    sim.approve();
    sim
}

fn hanging() -> AgentScript {
    AgentScript {
        outcome: AgentOutcome::Hang,
        ..AgentScript::default()
    }
}

fn session(sim: &Sim) -> Session {
    sim.task(1).session.unwrap()
}

fn run_until_initialized(sim: &mut Sim) -> Session {
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Running && x.cli_session_id.is_some())
    }));
    session(sim)
}

fn lost_cause(s: &Session) -> serde_json::Value {
    s.end_reason.as_ref().unwrap().params["cause"].clone()
}

fn assert_resumed_once(sim: &Sim, first: &Session, cause: &str) {
    let sessions = sim.sessions(1);
    assert_eq!(sessions.len(), 2, "{sessions:?}");
    assert_eq!(sessions[0].id, first.id);
    assert_eq!(sessions[0].state, SessionState::Resumed);
    assert_eq!(lost_cause(&sessions[0]), json!(cause));
    assert_eq!(sessions[1].resumed_from_session_id, Some(first.id));
    assert_eq!(sessions[1].resume_cause, Some(ResumeCause::Lost));
    assert_eq!(sessions[1].state, SessionState::Ended);
    assert!(
        sim.world
            .resumes()
            .contains(&Some(format!("cli-{}", first.id))),
        "the replacement continues the CLI session"
    );
    assert!(
        sim.world
            .prompts()
            .iter()
            .any(|p| p.starts_with("Your previous session stopped (the runner was lost).")),
        "the replacement opens with the resume turn"
    );
}

#[test]
fn a_restart_while_a_turn_runs_keeps_the_session_and_processes_each_event_once() {
    let mut sim = sim(AgentScript::default());
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Running)
    }));
    let id = session(&sim).id;
    assert!(
        sim.world.has_events(),
        "the turn's events are still in flight"
    );

    sim.restart();
    assert!(
        !sim.engine().readiness().sessions_reattached,
        "readiness waits for the reattach"
    );
    assert!(sim.run_to_shipped(STEPS));
    assert!(sim.engine().readiness().sessions_reattached);

    let sessions = sim.sessions(1);
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        (sessions[0].id, sessions[0].state),
        (id, SessionState::Ended)
    );
    assert_eq!(sim.usage(1).len() as u32, sessions[0].turns);
    let rows = sim.timeline(id);
    let log = sim.world.session_log(id);
    assert_eq!(
        rows.iter().map(|r| r.offset).collect::<Vec<_>>(),
        log.iter().map(|e| e.offset).collect::<Vec<_>>(),
        "one timeline row per runner event"
    );
    assert_eq!(sessions[0].last_offset, log.last().unwrap().offset);
    assert!(sim.stats().reattaches >= 1);
}

#[test]
fn a_restart_while_a_permission_waits_still_delivers_the_answer_once() {
    let mut sim = sim(AgentScript {
        ask: Some(("Bash".into(), json!({ "command": "make deploy" }))),
        ..AgentScript::default()
    });
    sim.cfg.operator.answer = None;
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Waiting)
    }));
    sim.restart();
    sim.cfg.operator.answer = Some(true);
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.sessions(1).len(), 1);
    assert_eq!(sim.stats().answers_to_cli, 2, "one answer per task");
}

#[test]
fn a_dead_runner_unit_loses_the_session_and_it_resumes_once() {
    let mut sim = sim(hanging());
    let first = run_until_initialized(&mut sim);
    sim.world.kill_runner(first.id);
    sim.world.set_script(AgentScript::default());
    assert!(sim.run_to_shipped(STEPS));
    assert_resumed_once(&sim, &first, "runner_exited");
}

#[test]
fn a_silent_runner_is_reconnected_three_times_then_stopped_and_resumed() {
    let mut sim = sim(hanging());
    let first = run_until_initialized(&mut sim);
    let silent_from = sim.engine().now();
    sim.world.silence_runner(first.id);
    sim.world.set_script(AgentScript::default());
    assert!(sim.run_until(STEPS, |s| s.sessions(1).len() == 2));
    let lost = &sim.sessions(1)[0];
    assert!(
        lost.ended_at.unwrap().since(silent_from) >= 30_000,
        "a runner is given 30 s of silence"
    );
    assert!(sim.run_to_shipped(STEPS));
    assert_resumed_once(&sim, &first, "runner_silent");
    assert!(
        sim.stats().sessions_stopped >= 1,
        "the silent unit is stopped"
    );
}

#[test]
fn a_silent_runner_whose_unit_died_is_lost_without_reconnecting() {
    let mut sim = sim(hanging());
    let first = run_until_initialized(&mut sim);
    sim.world.silence_runner(first.id);
    sim.world.kill_runner(first.id);
    sim.world.set_script(AgentScript::default());
    assert!(sim.run_to_shipped(STEPS));
    assert_resumed_once(&sim, &first, "runner_exited");
}

#[test]
fn a_runner_from_another_binary_is_stopped_and_its_session_resumed() {
    let mut sim = sim(hanging());
    let first = run_until_initialized(&mut sim);
    sim.world.swap_runner_version(first.id);
    sim.restart();
    sim.world.set_script(AgentScript::default());
    assert!(sim.run_to_shipped(STEPS));
    assert_resumed_once(&sim, &first, "version_mismatch");
    assert_eq!(sim.task(1).item.state, WorkItemState::Done);
}

fn budgeted(agent: AgentScript, budget: Budget, retry: Vec<BlockerCode>) -> Sim {
    let mut cfg = SimConfig {
        agent,
        operator: Operator {
            start: true,
            merge: true,
            retry,
            answer: Some(true),
        },
        ..SimConfig::without_review()
    };
    cfg.loop_cfg.budgets.insert(Stage::Dev, budget);
    let mut sim = Sim::new(cfg);
    sim.approve();
    sim
}

fn blocked_with(sim: &Sim) -> Option<(BlockerCode, serde_json::Value)> {
    sim.task(1)
        .open_blocker()
        .map(|b| (b.code, b.params.clone()))
}

fn run_to_blocked(sim: &mut Sim) -> (BlockerCode, serde_json::Value) {
    assert!(
        sim.run_until(STEPS, |s| blocked_with(s).is_some()),
        "never blocked"
    );
    blocked_with(sim).unwrap()
}

fn warned(sim: &Sim) -> Vec<BudgetDimension> {
    sim.engine()
        .work_item(sim.task(1).item.id)
        .unwrap()
        .unwrap()
        .stage_runs
        .iter()
        .flat_map(|r| r.budget_warnings.clone())
        .collect()
}

fn dirty() -> AgentScript {
    AgentScript {
        outcome: AgentOutcome::LeaveDirty,
        ..AgentScript::default()
    }
}

#[test]
fn every_budget_dimension_warns_once_then_stops_the_stage_run() {
    let cases = [
        (
            hanging(),
            Budget {
                running_minutes: 10,
                turns: 10,
                tokens: 0,
                cost_usd_micros: 0,
            },
            json!({ "dimension": "running_minutes", "used": 10, "limit": 10 }),
            BudgetDimension::RunningMinutes,
        ),
        (
            dirty(),
            Budget {
                running_minutes: 60,
                turns: 1,
                tokens: 0,
                cost_usd_micros: 0,
            },
            json!({ "dimension": "turns", "used": 1, "limit": 1 }),
            BudgetDimension::Turns,
        ),
        (
            dirty(),
            Budget {
                running_minutes: 60,
                turns: 10,
                tokens: 180,
                cost_usd_micros: 0,
            },
            json!({ "dimension": "tokens", "used": 300, "limit": 180 }),
            BudgetDimension::Tokens,
        ),
        (
            dirty(),
            Budget {
                running_minutes: 60,
                turns: 10,
                tokens: 0,
                cost_usd_micros: 1_200,
            },
            json!({ "dimension": "cost_usd", "used": "0.002000", "limit": "0.001200" }),
            BudgetDimension::CostUsd,
        ),
    ];
    for (agent, budget, params, dimension) in cases {
        let mut sim = budgeted(agent, budget, vec![]);
        let blocked = run_to_blocked(&mut sim);
        assert_eq!(blocked, (BlockerCode::BudgetExhausted, params.clone()));
        assert_eq!(sim.task(1).session.unwrap().state, SessionState::Failed);
        if dimension == BudgetDimension::Turns {
            assert!(
                warned(&sim).is_empty(),
                "one turn jumps from 0 to the limit"
            );
        } else {
            assert_eq!(warned(&sim), vec![dimension], "{params}");
        }
        assert!(
            sim.run_until(STEPS, |s| s.world.running_sessions().is_empty()),
            "the session is shut down"
        );
        assert_eq!(
            sim.usage(1).len() as u32,
            sim.sessions(1).iter().map(|s| s.turns).sum::<u32>(),
            "one usage record per turn"
        );
    }
}

#[test]
fn a_retry_after_the_budget_runs_out_resumes_with_a_full_budget() {
    let mut sim = budgeted(
        dirty(),
        Budget {
            running_minutes: 60,
            turns: 1,
            tokens: 0,
            cost_usd_micros: 0,
        },
        vec![],
    );
    run_to_blocked(&mut sim);
    let first = session(&sim);
    sim.world.set_script(AgentScript::default());
    sim.cfg.operator.retry = vec![BlockerCode::BudgetExhausted];
    assert!(sim.run_to_shipped(STEPS));
    let sessions = sim.sessions(1);
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[1].resumed_from_session_id, Some(first.id));
    assert_eq!(sessions[1].resume_cause, Some(ResumeCause::Retry));
    assert_ne!(sessions[1].owner, first.owner);
    assert!(
        sim.world
            .resumes()
            .contains(&Some(format!("cli-{}", first.id)))
    );
    assert!(
        sim.world
            .prompts()
            .iter()
            .any(|p| p.starts_with("Your previous session stopped (its budget ran out)."))
    );
}

#[test]
fn a_stage_run_keeps_the_budget_it_started_with() {
    let mut sim = budgeted(
        hanging(),
        Budget {
            running_minutes: 10,
            turns: 10,
            tokens: 0,
            cost_usd_micros: 0,
        },
        vec![],
    );
    run_until_initialized(&mut sim);
    sim.cfg.loop_cfg.budgets.insert(
        Stage::Dev,
        Budget {
            running_minutes: 600,
            turns: 10,
            tokens: 0,
            cost_usd_micros: 0,
        },
    );
    sim.restart();
    let detail = sim
        .engine()
        .work_item(sim.task(1).item.id)
        .unwrap()
        .unwrap();
    let view = detail.budget.unwrap();
    assert_eq!(view.limit.running_minutes, 10);
    assert_eq!((view.warn_percent, view.used.turns), (80, 1));
    let (code, params) = run_to_blocked(&mut sim);
    assert_eq!(code, BlockerCode::BudgetExhausted);
    assert_eq!(params["limit"], json!(10));
}

fn rate_limited(
    top: &str,
    window: &str,
    utilization: f64,
    resets_in_ms: i64,
    now: i64,
) -> AgentScript {
    let resets_at = (now + resets_in_ms) / 1000;
    AgentScript {
        rate_limit: Some(json!({
            "status": top, "rateLimitType": "five_hour", "resetsAt": resets_at,
            "unifiedWindows": {
                "five_hour": { "utilization": utilization, "status": window, "resetsAt": resets_at },
            },
        })),
        ..AgentScript::default()
    }
}

#[test]
fn a_busy_account_holds_new_tasks_until_its_window_resets() {
    let mut sim = sim(AgentScript::default());
    let now = sim.engine().now().0;
    sim.world.set_script(rate_limited(
        "allowed_warning",
        "allowed_warning",
        0.95,
        3 * 3_600_000,
        now,
    ));
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Done));
    let accounts = sim.engine().agent_accounts().unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(
        (accounts[0].cli.as_str(), accounts[0].name.as_str()),
        ("claude", "default")
    );
    assert!(accounts[0].paused);
    assert_eq!(accounts[0].windows[0].window, "five_hour");
    assert_eq!(accounts[0].windows[0].utilization, 0.95);

    sim.world.set_script(AgentScript::default());
    assert!(
        !sim.run_until(200, |s| s.task(2).item.state != WorkItemState::Queued),
        "S2 stays queued while the window is full"
    );
    let s2 = sim.task(2).item.id;
    let waiting = sim.engine().work_item(s2).unwrap().unwrap().waiting_on;
    assert!(
        matches!(&waiting[..], [Waiting::Quota { window, .. }] if window == "five_hour"),
        "{waiting:?}"
    );
    assert!(sim.run_to_shipped(STEPS));
    assert!(
        sim.engine().now().0 >= now + 3 * 3_600_000,
        "S2 waited for the reset"
    );
    assert!(!sim.engine().agent_accounts().unwrap()[0].paused);
}

#[test]
fn a_rejected_window_alone_lets_the_task_finish() {
    let mut sim = sim(AgentScript::default());
    let now = sim.engine().now().0;
    sim.world
        .set_script(rate_limited("allowed", "rejected", 0.5, 3_600_000, now));
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Done));
    assert!(sim.task(1).blocker.is_none());
    let accounts = sim.engine().agent_accounts().unwrap();
    assert_eq!(accounts[0].windows[0].status, "rejected");
}

#[test]
fn a_refused_rate_limit_blocks_the_running_task_as_quota_exhausted() {
    let mut sim = sim(AgentScript::default());
    let now = sim.engine().now().0;
    sim.world
        .set_script(rate_limited("rejected", "allowed", 1.0, 3_600_000, now));
    let (code, params) = run_to_blocked(&mut sim);
    assert_eq!(code, BlockerCode::QuotaExhausted);
    let resets_at = (now + 3_600_000) / 1000 * 1000;
    assert_eq!(
        params,
        json!({ "account": "claude:default", "window": "five_hour", "resets_at": resets_at })
    );
}

fn asking(command: &str) -> AgentScript {
    AgentScript {
        ask: Some(("Bash".into(), json!({ "command": command }))),
        ..AgentScript::default()
    }
}

#[test]
fn a_request_pending_past_the_wait_limit_expires_denies_and_blocks() {
    let mut sim = sim(AgentScript {
        outcome: AgentOutcome::Hang,
        ..asking("make deploy")
    });
    sim.cfg.operator.answer = None;
    sim.cfg.loop_cfg.permission_wait_ms = 3_600_000;
    sim.restart();
    let (code, params) = run_to_blocked(&mut sim);
    assert_eq!(code, BlockerCode::PermissionExpired);
    assert_eq!(params["hours"], json!(1));
    let t = sim.task(1);
    assert_eq!(t.session.unwrap().state, SessionState::Failed);
    assert!(sim.run_until(STEPS, |s| s.world.running_sessions().is_empty()));
    let request = sim
        .engine()
        .permission_requests(
            &Default::default(),
            &saka_engine::Page {
                after: None,
                limit: 10,
            },
        )
        .unwrap()
        .items;
    assert_eq!(
        request[0].request.answer_source.as_deref(),
        Some("system:wait_limit")
    );
    assert_eq!(
        sim.stats().answer_frames,
        1,
        "the deny reached the CLI once"
    );
    assert_eq!(sim.stats().interrupts, 1);
}

#[test]
fn a_remembered_answer_lets_the_next_identical_request_through_until_revoked() {
    let mut sim = sim(asking("make deploy"));
    sim.cfg.operator.answer = None;
    assert!(sim.run_until(STEPS, |s| !s.task(1).permissions.is_empty()));
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let first = sim.task(1).permissions[0].id;
    engine
        .answer_permission_with(first, true, None, true, &cause, None)
        .unwrap();
    let rules = engine.permission_rules(sim.project_id).unwrap().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].command.as_deref(), Some("make deploy"));
    assert!(sim.run_to_shipped(STEPS));
    let answered: Vec<String> = engine
        .permission_requests(
            &Default::default(),
            &saka_engine::Page {
                after: None,
                limit: 10,
            },
        )
        .unwrap()
        .items
        .into_iter()
        .filter_map(|p| p.request.answer_source)
        .collect();
    assert_eq!(
        answered,
        vec![
            "human:operator:local".to_string(),
            format!("policy:project:{}", rules[0].id)
        ]
    );

    let revoked = engine
        .revoke_rule(rules[0].id, "too broad", &cause, None)
        .unwrap();
    assert!(revoked.revoked_at.is_some());
    assert!(
        engine
            .permission_rules(sim.project_id)
            .unwrap()
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        engine.revoke_rule(rules[0].id, "again", &cause, None),
        Err(saka_engine::EngineError::Rejected(
            saka_core::rejection::Rejection::InvalidState { .. }
        ))
    ));
}

#[test]
fn a_one_character_difference_still_asks_the_operator() {
    let mut sim = sim(asking("make deploy"));
    sim.cfg.operator.answer = None;
    assert!(sim.run_until(STEPS, |s| !s.task(1).permissions.is_empty()));
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .answer_permission_with(
            sim.task(1).permissions[0].id,
            true,
            None,
            true,
            &cause,
            None,
        )
        .unwrap();
    sim.world.set_script(asking("make deploys"));
    assert!(sim.run_until(STEPS, |s| {
        s.task(2)
            .permissions
            .iter()
            .any(|p| p.answer_source.is_none())
    }));
}
