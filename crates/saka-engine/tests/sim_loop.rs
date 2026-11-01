#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::rejection::Rejection;
use saka_core::vocab::{
    BlockerCode, EnvironmentState, PermissionState, SessionState, SpecState, Stage, StagePhase,
    StageRunState, Trigger, WorkItemState,
};
use saka_engine::EngineError;
use saka_testkit::{AgentOutcome, AgentScript, Sim, SimConfig};
use serde_json::json;

const STEPS: usize = 5_000;

fn blocked_with(sim: &Sim, step: u32) -> Option<BlockerCode> {
    let t = sim.task(step);
    (t.item.state == WorkItemState::Blocked)
        .then(|| t.open_blocker().map(|b| b.code))
        .flatten()
}

#[test]
fn a_two_step_spec_runs_from_approve_to_shipped() {
    let mut sim = Sim::new(SimConfig::default());
    assert_eq!(sim.approve().len(), 2);

    let mut s2_early = false;
    let shipped = sim.run_until(STEPS, |s| {
        let (s1, s2) = (s.task(1), s.task(2));
        if s2.item.state != WorkItemState::Queued && s1.item.state != WorkItemState::Done {
            s2_early = true;
        }
        s.spec().state == SpecState::Shipped
    });
    assert!(
        shipped,
        "not shipped: {:?}",
        sim.tasks()
            .iter()
            .map(|t| (t.item.state, t.item.stage_phase))
            .collect::<Vec<_>>()
    );
    assert!(!s2_early, "S2 was admitted before S1 was done");

    let stats = sim.stats();
    assert_eq!((stats.prs_opened, stats.merges, stats.pushes), (2, 2, 2));
    let prs = sim.world.prs();
    let branches: Vec<&str> = prs.iter().map(|p| p.head_branch.as_str()).collect();
    assert_eq!(branches, ["saka/tasks-done/s1", "saka/tasks-done/s2"]);
    assert!(prs[0].body.contains("<!-- saka-task: tasks-done/s1 -->"));
    assert!(prs[0].body.contains("**Spec:** `docs/specs/tasks-done.md`"));
    assert_eq!(prs[0].title, "Add the done command");

    let s1_merge = sim
        .task(1)
        .change_request
        .unwrap()
        .merge_commit_sha
        .unwrap();
    let s2_base = sim.task(2).environment.unwrap().base_sha.unwrap();
    assert!(sim.world.is_ancestor(&s1_merge, &s2_base));

    for step in [1, 2] {
        let t = sim.task(step);
        assert_eq!(t.item.state, WorkItemState::Done);
        assert_eq!(t.environment.unwrap().state, EnvironmentState::TornDown);
    }
    assert_eq!(stats.branches_deleted, 2);
    assert_eq!(stats.teardowns, 2);
    assert!(sim.world.running_sessions().is_empty());
    let outcome = sim.outcome();
    assert!(outcome.merges_on_base);
    assert!(outcome.leftover_branches.is_empty());
}

fn sim_with(agent: AgentScript, f: impl FnOnce(&mut SimConfig)) -> Sim {
    let mut cfg = SimConfig {
        agent,
        ..SimConfig::default()
    };
    f(&mut cfg);
    let mut sim = Sim::new(cfg);
    sim.approve();
    sim
}

fn script(outcome: AgentOutcome) -> AgentScript {
    AgentScript { outcome, ask: None }
}

fn run_to_block(sim: &mut Sim) -> BlockerCode {
    assert!(
        sim.run_until(STEPS, |s| blocked_with(s, 1).is_some()),
        "S1 never blocked"
    );
    blocked_with(sim, 1).unwrap()
}

#[test]
fn uncommitted_work_gets_one_follow_up_then_blocks_as_no_changes() {
    let mut sim = sim_with(script(AgentOutcome::LeaveDirty), |_| {});
    assert_eq!(run_to_block(&mut sim), BlockerCode::NoChanges);
    let t = sim.task(1);
    assert_eq!(
        t.open_blocker().unwrap().params,
        json!({ "dirty": true, "commits": 0 })
    );
    let session = t.session.unwrap();
    assert_eq!((session.turns, session.followups), (2, 1));
    assert_eq!(session.state, SessionState::Ended);
    assert_eq!(sim.stats().pushes, 0);

    let mut sim = sim_with(script(AgentOutcome::NoCommit), |_| {});
    assert_eq!(run_to_block(&mut sim), BlockerCode::NoChanges);
    assert_eq!(
        sim.task(1).open_blocker().unwrap().params,
        json!({ "dirty": false, "commits": 0 })
    );
}

#[test]
fn a_follow_up_that_commits_moves_on_to_the_change_request() {
    let mut sim = sim_with(script(AgentOutcome::CommitOnFollowup), |_| {});
    assert!(sim.run_until(STEPS, |s| s.task(1).change_request.is_some()));
    let t = sim.task(1);
    assert_eq!(t.session.unwrap().turns, 2);
    assert_eq!(t.item.stage, Some(Stage::Merge));
}

#[test]
fn a_cli_error_result_blocks_as_session_failed() {
    let mut sim = sim_with(
        script(AgentOutcome::Fail("error_during_execution".into())),
        |_| {},
    );
    assert_eq!(run_to_block(&mut sim), BlockerCode::SessionFailed);
    assert_eq!(
        sim.task(1).open_blocker().unwrap().params,
        json!({ "subtype": "error_during_execution" })
    );
}

#[test]
fn a_runner_exit_without_a_result_blocks_as_session_lost() {
    let mut sim = sim_with(script(AgentOutcome::Exit), |c| c.operator.retry.clear());
    assert_eq!(run_to_block(&mut sim), BlockerCode::SessionLost);
    assert_eq!(sim.task(1).session.unwrap().state, SessionState::Lost);
    assert!(
        sim.run_until(STEPS, |s| s.world.running_sessions().is_empty()),
        "the runner unit is stopped"
    );
}

#[test]
fn running_too_long_interrupts_and_blocks_as_session_timeout_not_counting_waits() {
    let ask = AgentScript {
        outcome: AgentOutcome::Hang,
        ask: Some(("Bash".into(), json!({ "command": "make deploy" }))),
    };
    let mut sim = sim_with(ask, |_| {});
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Waiting)
    }));
    let waited_from = sim.engine().now();
    assert!(sim.run_until(STEPS, |s| s.engine().now().since(waited_from)
        > 2 * 3_600_000));
    assert_eq!(sim.task(1).item.state, WorkItemState::InStage);

    let pending = sim.task(1).permissions[0].id;
    let engine = sim.engine();
    engine
        .answer_permission(pending, true, None, &engine.cause("human:operator:local"))
        .unwrap();
    let answered_at = engine.now();
    assert_eq!(run_to_block(&mut sim), BlockerCode::SessionTimeout);
    let blocked_at = sim.task(1).open_blocker().unwrap().opened_at;
    assert!(blocked_at.since(answered_at) >= 60 * 60_000);
    assert_eq!(
        sim.task(1).open_blocker().unwrap().params,
        json!({ "minutes": 60 })
    );
    assert_eq!(
        sim.task(1).session.unwrap().state,
        SessionState::Interrupted
    );
    assert!(sim.run_until(STEPS, |s| s.world.running_sessions().is_empty()));
    assert_eq!(sim.stats().interrupts, 1);
}

#[test]
fn an_unmatched_tool_request_waits_for_the_operator_and_is_delivered_once() {
    let ask = AgentScript {
        outcome: AgentOutcome::Commit,
        ask: Some((
            "Bash".into(),
            json!({ "command": "make deploy && echo ok" }),
        )),
    };
    let mut sim = sim_with(ask, |_| {});
    assert!(sim.run_until(STEPS, |s| !s.task(1).permissions.is_empty()));
    for _ in 0..50 {
        sim.step();
    }
    let t = sim.task(1);
    assert!(t.is_at(Stage::Dev, StagePhase::RunningSession));
    assert_eq!(t.session.as_ref().unwrap().state, SessionState::Waiting);
    let request = &t.permissions[0];
    assert_eq!(request.state, PermissionState::Pending);
    assert_eq!(request.summary, "Bash: make deploy && echo ok");
    assert_eq!(sim.stats().answers_to_cli, 0);

    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .answer_permission(request.id, true, None, &cause)
        .unwrap();
    assert!(matches!(
        engine.answer_permission(request.id, false, None, &cause),
        Err(EngineError::Rejected(Rejection::PermissionAlreadyAnswered))
    ));
    sim.cfg.operator.answer = Some(true);
    assert!(sim.run_until(STEPS, |s| s.spec().state == SpecState::Shipped));
    let stats = sim.stats();
    assert_eq!(
        (stats.answers_to_cli, stats.answer_frames),
        (2, 2),
        "one answer per request"
    );
}

#[test]
fn policy_answers_matching_requests_and_denies_git_push() {
    let rules = vec![saka_core::policy::RuleSpec {
        id: "safe-commands".into(),
        effect: saka_core::policy::Effect::Allow,
        tools: vec!["Bash".into()],
        paths: vec![],
        commands: vec!["go test *".into()],
    }];
    for (command, state, source) in [
        (
            "go test ./...",
            PermissionState::Allowed,
            "policy:safe-commands",
        ),
        (
            "git push origin HEAD",
            PermissionState::Denied,
            "policy:builtin:git_push",
        ),
    ] {
        let ask = AgentScript {
            outcome: AgentOutcome::Commit,
            ask: Some(("Bash".into(), json!({ "command": command }))),
        };
        let mut sim = sim_with(ask, |c| c.rules = rules.clone());
        assert!(
            sim.run_until(STEPS, |s| s.task(1).change_request.is_some()),
            "{command}"
        );
        let events = sim
            .engine()
            .store()
            .read(|c| saka_store::read::events_after(c, 0, 10_000))
            .unwrap();
        let answered = events
            .iter()
            .find(|e| e.kind == "runtime.permission.answered")
            .unwrap();
        assert_eq!(
            answered.payload["state"],
            json!(state.as_str()),
            "{command}"
        );
        assert_eq!(
            answered.payload["answer_source"],
            json!(source),
            "{command}"
        );
        assert_eq!(sim.stats().answers_to_cli, 1);
    }
}

#[test]
fn sync_marks_done_only_when_the_merge_is_on_the_base_branch() {
    let mut sim = sim_with(AgentScript::default(), |_| {});
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Done));
    assert_eq!(sim.task(2).item.state, WorkItemState::Queued);
    sim.engine().reconcile().unwrap();
    assert_eq!(sim.task(2).item.state, WorkItemState::Admitted);

    let mut sim = sim_with(AgentScript::default(), |_| {});
    sim.world.set_lose_merges(true);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Sync, StagePhase::Verifying)
    }));
    let verifying_from = sim.engine().now();
    assert_eq!(run_to_block(&mut sim), BlockerCode::DodFailed);
    let t = sim.task(1);
    let blocker = t.open_blocker().unwrap();
    assert!(
        blocker.opened_at.since(verifying_from) >= 2 * 30_000,
        "three checks 30 s apart"
    );
    assert_eq!(blocker.params["base_branch"], json!("main"));
    assert_eq!(
        t.environment.unwrap().state,
        EnvironmentState::Ready,
        "nothing torn down"
    );
    assert_eq!(sim.stats().branches_deleted, 0);
}

#[test]
fn retry_needs_a_reason_and_a_blocked_task_and_starts_a_manual_attempt() {
    let mut sim = sim_with(script(AgentOutcome::Fail("error_max_turns".into())), |_| {});
    assert_eq!(run_to_block(&mut sim), BlockerCode::SessionFailed);
    let id = sim.task(1).item.id;
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    for bad in ["", "   ", &"x".repeat(501)] {
        assert!(matches!(
            engine.retry(id, bad, &cause),
            Err(EngineError::Invalid {
                field: "reason",
                ..
            })
        ));
    }
    sim.world.set_script(AgentScript::default());
    engine
        .retry(id, "the CLI hit its turn limit", &cause)
        .unwrap();
    let run = sim.task(1).stage_run.unwrap();
    assert_eq!(
        (run.stage, run.attempt, run.trigger),
        (Stage::Dev, 2, Trigger::Manual)
    );
    assert_eq!(run.reason.as_deref(), Some("the CLI hit its turn limit"));
    assert!(matches!(
        engine.retry(id, "again", &cause),
        Err(EngineError::Rejected(Rejection::InvalidState { .. }))
    ));
    assert!(sim.run_until(STEPS, |s| s.spec().state == SpecState::Shipped));
}

#[test]
fn cancel_stops_the_session_tears_down_and_ends_the_task() {
    let mut sim = sim_with(script(AgentOutcome::Hang), |_| {});
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Running)
    }));
    let id = sim.task(1).item.id;
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    assert!(matches!(
        engine.cancel(id, "", &cause),
        Err(EngineError::Invalid {
            field: "reason",
            ..
        })
    ));
    engine.cancel(id, "no longer needed", &cause).unwrap();
    assert_eq!(sim.task(1).item.state, WorkItemState::Cancelled);
    assert!(sim.run_until(STEPS, |s| {
        s.world.running_sessions().is_empty()
            && s.task(1)
                .environment
                .is_some_and(|e| e.state == EnvironmentState::TornDown)
    }));
    assert_eq!(sim.stats().teardowns, 1);
    assert_eq!(
        sim.task(1).stage_run.unwrap().state,
        StageRunState::Cancelled
    );
    for result in [
        engine.cancel(id, "again", &cause),
        engine.retry(id, "again", &cause),
    ] {
        assert!(matches!(
            result,
            Err(EngineError::Rejected(Rejection::InvalidState { .. }))
        ));
    }
    for _ in 0..50 {
        sim.step();
    }
    assert_eq!(sim.task(2).item.state, WorkItemState::Queued);
}
