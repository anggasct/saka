#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::blocker::primary_action;
use saka_core::vocab::{
    BlockerAction, BlockerCode, RegressionOutcome, SessionState, SpecState, Stage, StagePhase,
    WorkItemState,
};
use saka_ports::{RegressionRefusal, Regressions, Requested};
use saka_testkit::{
    AgentOutcome, AgentScript, CommandScript, Operator, ReviewReply, Sim, SimConfig, one_step_spec,
};

const STEPS: usize = 8_000;
const COMMAND: &str = "make test-regression";
const TEST_FILE: &str = "tests/regression.rs";
const SOURCE_FILE: &str = "src/lib.rs";

fn bugfix_text() -> String {
    format!(
        "{}\n## Regression\n\n- `{COMMAND}`\n",
        one_step_spec("Login loop", "kind: bugfix\n")
    )
}

fn edit(path: &str, text: Option<&str>) -> (String, Option<String>) {
    (path.to_string(), text.map(String::from))
}

fn bugfix_sim(agent: AgentScript) -> Sim {
    bugfix_sim_with(SimConfig {
        agent,
        ..SimConfig::default()
    })
}

fn bugfix_sim_with(cfg: SimConfig) -> Sim {
    let mut sim = Sim::new(cfg);
    let spec = sim.register("docs/specs/login-loop.md", &bugfix_text());
    assert!(spec.lint_ok, "{:?}", spec.issues);
    sim.approve_spec(spec.spec_id, spec.revision_id).unwrap();
    sim.spec_id = spec.spec_id;
    sim
}

fn task(sim: &Sim) -> saka_core::model::Task {
    sim.task(1)
}

fn regressions(sim: &Sim) -> Vec<saka_core::model::RegressionCheck> {
    task(sim).regressions
}

fn payload(sim: &Sim, kind: &str) -> Vec<serde_json::Value> {
    sim.events_of(kind).into_iter().map(|e| e.payload).collect()
}

fn regression_runs(sim: &Sim) -> usize {
    sim.world
        .recipe_runs()
        .iter()
        .filter(|r| r.command == COMMAND && r.phase == "gate")
        .count()
}

#[test]
fn a_bugfix_is_proven_by_a_test_that_fails_first_and_then_ships_with_the_fix() {
    let mut sim = bugfix_sim(AgentScript::default());
    sim.world.script_edits(vec![
        vec![edit(TEST_FILE, Some("a test"))],
        vec![edit(SOURCE_FILE, Some("the fix"))],
    ]);
    sim.world.script_regression_calls(vec![true, false]);
    sim.world.script_command(COMMAND, CommandScript::Exit(1));

    assert!(sim.run_to_shipped(STEPS), "{:?}", sim.outcome());

    let results = sim.world.regression_results();
    assert!(matches!(
        results.as_slice(),
        [Ok(Requested::Recorded { attempt: 1, .. })]
    ));
    let checks = regressions(&sim);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].outcome, Some(RegressionOutcome::Reproduced));
    assert_eq!(checks[0].paths, [TEST_FILE]);
    assert!(checks[0].checks[0].failed());
    assert!(checks[0].delivered_at.is_some());
    assert_eq!(payload(&sim, "delivery.regression.requested").len(), 1);
    let observed = payload(&sim, "delivery.regression.observed");
    assert_eq!(observed[0]["outcome"], "reproduced");

    let prompts = sim.world.prompts();
    assert!(
        prompts
            .iter()
            .any(|p| p.contains("Result: reproduced") && p.contains(COMMAND)),
        "{prompts:?}"
    );
    assert_eq!(task(&sim).item.state, WorkItemState::Done);
    assert_eq!(sim.spec().state, SpecState::Shipped);
    assert!(regression_runs(&sim) >= 2);
    assert!(
        sim.engine()
            .store()
            .read(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM regression_check WHERE outcome = 'reproduced' AND delivered_at IS NOT NULL",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(Into::into)
            })
            .unwrap()
            == 1
    );
}

#[test]
fn a_commit_that_touches_more_than_tests_is_refused_without_running_the_commands() {
    let mut sim = bugfix_sim(AgentScript::default());
    sim.world.script_edits(vec![vec![
        edit(TEST_FILE, Some("a test")),
        edit(SOURCE_FILE, Some("a fix too early")),
    ]]);
    sim.world.script_regression_calls(vec![true]);

    sim.run_until(STEPS, |s| {
        s.world
            .prompts()
            .iter()
            .any(|p| p.contains("Result: invalid paths"))
    });

    let check = &regressions(&sim)[0];
    assert_eq!(check.outcome, Some(RegressionOutcome::InvalidPaths));
    assert_eq!(check.paths, [SOURCE_FILE]);
    assert!(check.checks.is_empty());
    assert_eq!(regression_runs(&sim), 0);
    let prompts = sim.world.prompts();
    assert!(
        prompts
            .iter()
            .any(|p| p.contains("Result: invalid paths") && p.contains(SOURCE_FILE)),
        "{prompts:?}"
    );
}

#[test]
fn the_third_check_that_does_not_reproduce_the_bug_blocks_the_task_with_a_retry() {
    let mut sim = bugfix_sim(AgentScript::default());
    sim.world
        .script_edits(vec![vec![edit(TEST_FILE, Some("a test"))]]);
    sim.world.script_regression_calls(vec![true, true, true]);

    assert!(sim.run_until(STEPS, |s| task(s).item.state == WorkItemState::Blocked));

    let task = task(&sim);
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::RegressionNotReproduced);
    assert_eq!(blocker.params["attempts"], 3);
    assert_eq!(blocker.params["last_outcome"], "not_reproduced");
    assert_eq!(primary_action(&task).unwrap().kind, BlockerAction::Retry);
    let outcomes: Vec<_> = task.regressions.iter().map(|c| c.outcome).collect();
    assert_eq!(
        outcomes,
        [Some(RegressionOutcome::NotReproduced); 3].to_vec()
    );
    assert!(task.session.as_ref().is_none_or(|s| !s.state.is_live()));
}

#[test]
fn a_dev_session_that_ends_without_a_reproduced_test_blocks_the_task() {
    let mut sim = bugfix_sim(AgentScript::default());
    sim.world
        .script_edits(vec![vec![edit(TEST_FILE, Some("a test"))]]);
    sim.world.script_regression_calls(vec![false]);

    assert!(sim.run_until(STEPS, |s| task(s).item.state == WorkItemState::Blocked));

    let blocker = task(&sim).open_blocker().cloned().unwrap();
    assert_eq!(blocker.code, BlockerCode::RegressionNotReproduced);
    assert_eq!(blocker.params["attempts"], 0);
    assert_eq!(blocker.params["last_outcome"], "none");
    assert!(sim.world.regression_results().is_empty());
}

#[test]
fn a_test_that_disappears_from_the_head_fails_the_gate_on_the_regression_commit() {
    let mut sim = bugfix_sim(AgentScript::default());
    sim.world.script_edits(vec![
        vec![edit(TEST_FILE, Some("a test"))],
        vec![edit(TEST_FILE, None), edit(SOURCE_FILE, Some("the fix"))],
    ]);
    sim.world.script_regression_calls(vec![true, false]);
    sim.world.script_command(COMMAND, CommandScript::Exit(1));
    sim.run_until(STEPS, |s| {
        regressions(s).first().is_some_and(|c| c.outcome.is_some())
    });
    sim.world.rewrite_history_on_next_commit();

    assert!(sim.run_until(STEPS, |s| task(s).item.state == WorkItemState::Blocked));

    let task = task(&sim);
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::CiRed);
    assert_eq!(blocker.params["source"], "saka");
    assert_eq!(
        blocker.params["checks"],
        serde_json::json!(["saka/regression-commit"])
    );
    assert_eq!(blocker.params["reason"], "regression_commit_missing");
    assert_eq!(regression_runs(&sim), 1);
}

#[test]
fn a_rebase_that_keeps_the_test_patch_still_passes_the_gate() {
    let mut sim = bugfix_sim(AgentScript::default());
    sim.world.script_edits(vec![
        vec![edit(TEST_FILE, Some("a test"))],
        vec![
            edit(TEST_FILE, Some("a test")),
            edit(SOURCE_FILE, Some("the fix")),
        ],
    ]);
    sim.world.script_regression_calls(vec![true, false]);
    sim.world.script_command(COMMAND, CommandScript::Exit(1));
    sim.run_until(STEPS, |s| {
        regressions(s).first().is_some_and(|c| c.outcome.is_some())
    });
    sim.world.rewrite_history_on_next_commit();

    assert!(sim.run_to_shipped(STEPS), "{:?}", sim.outcome());
}

#[test]
fn only_the_dev_session_of_a_bugfix_can_call_the_tool_and_a_dirty_worktree_records_nothing() {
    let mut sim = Sim::new(SimConfig::default());
    sim.approve();
    sim.world.script_regression_calls(vec![true]);
    sim.run_until(STEPS, |s| !s.world.regression_results().is_empty());
    assert_eq!(
        sim.world.regression_results(),
        [Err(RegressionRefusal::ToolUnavailable)]
    );
    assert!(payload(&sim, "delivery.regression.requested").is_empty());

    let mut dirty = bugfix_sim(AgentScript {
        outcome: AgentOutcome::LeaveDirty,
        ..AgentScript::default()
    });
    dirty.world.script_regression_calls(vec![true]);
    dirty.run_until(STEPS, |s| !s.world.regression_results().is_empty());
    assert_eq!(
        dirty.world.regression_results(),
        [Ok(Requested::DirtyWorktree)]
    );
    assert!(payload(&dirty, "delivery.regression.requested").is_empty());
}

#[test]
fn asking_twice_for_the_same_commit_while_the_check_runs_queues_one_job() {
    let mut sim = bugfix_sim(AgentScript::default());
    sim.world
        .script_edits(vec![vec![edit(TEST_FILE, Some("a test"))]]);
    sim.world.script_regression_calls(vec![true]);
    sim.run_until(STEPS, |s| !s.world.regression_results().is_empty());

    let session = task(&sim).session.unwrap().id;
    let again = sim.block_on(sim.engine().request(session));
    assert!(matches!(again, Ok(Requested::Recorded { attempt: 1, .. })));

    let jobs: i64 = sim
        .engine()
        .store()
        .read(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM job WHERE kind = 'gate.reproduce'",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(jobs, 1);
    assert_eq!(payload(&sim, "delivery.regression.requested").len(), 1);
    assert!(task(&sim).is_at(Stage::Dev, StagePhase::RunningSession));
}

#[test]
fn the_session_waits_for_the_result_while_the_check_runs_and_the_task_still_ships() {
    let mut sim = bugfix_sim(AgentScript::default());
    sim.world.script_edits(vec![
        vec![edit(TEST_FILE, Some("a test"))],
        vec![edit(SOURCE_FILE, Some("the fix"))],
    ]);
    sim.world.script_regression_calls(vec![true, false]);
    sim.world.script_command(COMMAND, CommandScript::Exit(1));
    assert!(sim.run_until(STEPS, |s| {
        task(s)
            .session
            .as_ref()
            .is_some_and(|s| s.state == SessionState::Waiting)
    }));
    assert!(regressions(&sim)[0].awaiting_result());
    assert!(sim.run_to_shipped(STEPS));
}

#[test]
fn a_fix_round_after_review_keeps_the_proof_and_asks_for_no_new_check() {
    let mut sim = bugfix_sim(AgentScript::default());
    sim.world.script_edits(vec![
        vec![edit(TEST_FILE, Some("a test"))],
        vec![edit(SOURCE_FILE, Some("the fix"))],
        vec![edit(SOURCE_FILE, Some("a better fix"))],
    ]);
    sim.world.script_regression_calls(vec![true, false, false]);
    sim.world.script_command(COMMAND, CommandScript::Exit(1));
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("name the fix"),
        ReviewReply::approve(),
    ]);

    assert!(sim.run_to_shipped(STEPS), "{:?}", sim.outcome());

    assert_eq!(sim.world.regression_results().len(), 1);
    assert_eq!(regressions(&sim).len(), 1);
    assert_eq!(sim.task(1).fix_rounds_used(), 1);
}

#[test]
fn a_retry_resets_the_branch_to_its_base_so_a_test_only_commit_is_possible_again() {
    let mut sim = bugfix_sim_with(SimConfig {
        operator: Operator {
            retry: vec![BlockerCode::RegressionNotReproduced],
            ..Operator::default()
        },
        ..SimConfig::default()
    });
    let both = vec![
        edit(TEST_FILE, Some("a test")),
        edit(SOURCE_FILE, Some("a fix too early")),
    ];
    sim.world.script_edits(vec![
        both.clone(),
        both.clone(),
        both,
        vec![edit(TEST_FILE, Some("a test"))],
        vec![edit(SOURCE_FILE, Some("the fix"))],
    ]);
    sim.world
        .script_regression_calls(vec![true, true, true, true, false]);
    sim.world.script_command(COMMAND, CommandScript::Exit(1));

    assert!(sim.run_to_shipped(STEPS), "{:?}", sim.outcome());

    let task = task(&sim);
    let resets = payload(&sim, "delivery.regression.branch_reset");
    assert_eq!(resets.len(), 1, "{resets:?}");
    assert_ne!(resets[0]["old_head_sha"], resets[0]["base_sha"]);
    assert_eq!(
        resets[0]["base_sha"].as_str().unwrap(),
        task.environment.unwrap().base_sha.unwrap()
    );
    let outcomes: Vec<_> = task.regressions.iter().map(|c| c.outcome).collect();
    assert_eq!(
        outcomes,
        [
            Some(RegressionOutcome::InvalidPaths),
            Some(RegressionOutcome::InvalidPaths),
            Some(RegressionOutcome::InvalidPaths),
            Some(RegressionOutcome::Reproduced),
        ]
    );
    assert_eq!(
        sim.events_of("delivery.work_item.blocked")
            .iter()
            .filter(|e| e.payload["code"] == "regression_not_reproduced")
            .count(),
        1
    );
}

#[test]
fn the_output_of_a_check_is_redacted_and_cut_to_its_tail_before_anything_stores_it() {
    let mut sim = bugfix_sim(AgentScript::default());
    let cause = saka_engine::Cause {
        actor: "operator".into(),
        causation_id: None,
        correlation_id: uuid::Uuid::from_u128(42),
    };
    sim.engine()
        .set_secret(
            sim.project_id,
            "STORAGE_KEY",
            &saka_core::secret::SecretText::new("sk-live-0123456789".into()),
            &cause,
            None,
        )
        .unwrap();
    let noise = "x".repeat(20_000);
    sim.world.command_output(
        COMMAND,
        &format!("{noise}\nfailed with key sk-live-0123456789\nassertion failed: left != right\n"),
    );
    sim.world
        .script_edits(vec![vec![edit(TEST_FILE, Some("a test"))]]);
    sim.world.script_regression_calls(vec![true]);
    sim.world.script_command(COMMAND, CommandScript::Exit(101));
    sim.run_until(STEPS, |s| {
        regressions(s).first().is_some_and(|c| c.outcome.is_some())
    });

    let run = &regressions(&sim)[0].checks[0];
    assert_eq!(run.exit_code, Some(101));
    assert!(run.duration_ms > 0);
    assert!(run.output_tail.len() <= saka_ports::OUTPUT_TAIL_BYTES);
    assert!(run.output_tail.contains("assertion failed: left != right"));
    assert!(!run.output_tail.contains("sk-live-0123456789"));
    let stored: String = sim
        .engine()
        .store()
        .read(|c| {
            c.query_row("SELECT checks FROM regression_check", [], |r| r.get(0))
                .map_err(Into::into)
        })
        .unwrap();
    assert!(!stored.contains("sk-live-0123456789"));
}

#[test]
fn the_reviewer_of_a_bugfix_gets_the_proof_and_the_reviewer_of_a_feature_does_not() {
    let mut sim = bugfix_sim(AgentScript::default());
    sim.world.script_edits(vec![
        vec![edit(TEST_FILE, Some("a test"))],
        vec![edit(SOURCE_FILE, Some("the fix"))],
    ]);
    sim.world.script_regression_calls(vec![true, false]);
    sim.world.script_command(COMMAND, CommandScript::Exit(1));
    sim.world
        .command_output(COMMAND, "thread 'login' panicked: loop");

    assert!(sim.run_to_shipped(STEPS), "{:?}", sim.outcome());

    let prompts = sim.world.prompts();
    let review = prompts
        .iter()
        .find(|p| p.contains("independent code reviewer"))
        .unwrap();
    assert!(review.contains("<regression_evidence>"));
    assert!(review.contains(&format!("- {COMMAND}")));
    assert!(review.contains(&format!("At the test commit: {COMMAND} exited with code 1")));
    assert!(review.contains("thread 'login' panicked: loop"));
    assert!(review.contains("At the head "));
    assert!(review.contains("passed in Saka's gate (exit code 0)"));
    assert!(review.contains("does not show the bug the spec describes"));
    assert!(!review.contains("<security_checklist>"));

    let mut feature = Sim::new(SimConfig::default());
    feature.approve();
    assert!(feature.run_to_shipped(STEPS));
    let plain = feature.world.prompts();
    assert!(
        plain
            .iter()
            .filter(|p| p.contains("independent code reviewer"))
            .all(|p| !p.contains("<regression_evidence>") && !p.contains("<security_checklist>"))
    );
}

#[test]
fn the_reviewer_of_a_security_patch_gets_the_security_checklist() {
    let mut sim = Sim::new(SimConfig::default());
    let text = one_step_spec("Close the hole", "kind: security_patch\n");
    let spec = sim.register("docs/specs/close-hole.md", &text);
    sim.approve_spec(spec.spec_id, spec.revision_id).unwrap();
    sim.spec_id = spec.spec_id;
    sim.cfg.operator.start = true;

    assert!(sim.run_to_shipped(STEPS), "{:?}", sim.outcome());

    let prompts = sim.world.prompts();
    let review = prompts
        .iter()
        .find(|p| p.contains("independent code reviewer"))
        .unwrap();
    for item in [
        "Untrusted input",
        "Authorization",
        "Secrets",
        "Dependencies",
        "Logs",
    ] {
        assert!(review.contains(item), "{item}");
    }
    assert!(!review.contains("<regression_evidence>"));
}
