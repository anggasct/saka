#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{
    BlockerCode, CheckConclusion, GateOutcome, GateSource, Stage, StagePhase, WorkItemState,
};
use saka_ports::StatusState;
use saka_testkit::{CiScript, CommandScript, Sim, SimConfig};
use serde_json::json;

const STEPS: usize = 8_000;
const STEP_COMMAND: &str = "cd cli && go test ./...";

fn sim(ci: CiScript, recipe: Option<&str>) -> Sim {
    let mut cfg = SimConfig {
        ci,
        ..SimConfig::default()
    };
    cfg.operator.merge = false;
    let mut sim = Sim::new(cfg);
    sim.world.set_recipe(recipe);
    sim.approve();
    sim
}

fn blocked(s: &Sim) -> bool {
    s.task(1).item.state == WorkItemState::Blocked
}

fn awaiting_merge(s: &Sim) -> bool {
    s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
}

fn gate_commands(sim: &Sim) -> Vec<String> {
    sim.world
        .recipe_runs()
        .iter()
        .filter(|r| r.phase == "gate")
        .map(|r| r.command.clone())
        .collect()
}

#[test]
fn a_saka_gate_merges_a_repo_without_ci() {
    let mut sim = sim(
        CiScript::Silent,
        Some("[gate]\nsource = \"saka\"\ncommands = [\"make lint\"]\n"),
    );
    assert!(sim.run_until(STEPS, awaiting_merge));
    assert_eq!(gate_commands(&sim), [STEP_COMMAND, "make lint"]);
    let t = sim.task(1);
    let head = t.change_request.clone().unwrap().head_sha;
    let saka = t.gate_for(GateSource::Saka, &head).unwrap();
    assert_eq!(saka.outcome, GateOutcome::Passed);
    let names: Vec<&str> = saka.checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, [STEP_COMMAND, "make lint"]);
    assert!(
        saka.checks
            .iter()
            .all(|c| c.kind == saka_core::vocab::CheckKind::Command)
    );
}

#[test]
fn both_sources_must_pass_and_either_failing_blocks() {
    let mut sim = sim(
        CiScript::Pass,
        Some("[gate]\nsource = \"both\"\ncommands = [\"make lint\"]\n"),
    );
    sim.world
        .script_command("make lint", CommandScript::Exit(2));
    sim.world
        .command_output("make lint", "lint: x.go:3: unused");
    assert!(sim.run_until(STEPS, blocked));
    let blocker = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(blocker.code, BlockerCode::CiRed);
    let head = sim.task(1).change_request.unwrap().head_sha;
    assert_eq!(blocker.params["source"], "saka");
    assert_eq!(blocker.params["command"], "make lint");
    assert_eq!(blocker.params["exit_code"], 2);
    assert_eq!(blocker.params["log_tail"], "lint: x.go:3: unused");
    assert_eq!(blocker.params["head_sha"], json!(head));
    assert_eq!(blocker.params["checks"], json!(["make lint"]));

    let none = sim_without_checks("both");
    let blocker = none.task(1).open_blocker().cloned().unwrap();
    assert_eq!(
        (
            blocker.params["source"].as_str(),
            blocker.params["reason"].as_str()
        ),
        (Some("scm"), Some("no_checks"))
    );
}

fn sim_without_checks(source: &str) -> Sim {
    let mut sim = sim(
        CiScript::Silent,
        Some(&format!("[gate]\nsource = \"{source}\"\n")),
    );
    assert!(sim.run_until(STEPS, |s| blocked(s) || awaiting_merge(s)));
    sim
}

#[test]
fn auto_waits_for_ci_then_merges_on_saka_alone_or_follows_failing_ci() {
    let mut sim = sim(CiScript::Silent, None);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingCi)
    }));
    let entered = sim.engine().now();
    assert!(sim.run_until(STEPS, awaiting_merge));
    let waited = sim.engine().now().since(entered);
    assert!(
        waited >= SimConfig::default().loop_cfg.ci_appear_timeout_ms,
        "{waited}"
    );
    assert!(!blocked(&sim));

    let mut red = sim_with(CiScript::Fail(vec!["test".into()]));
    assert!(red.run_until(STEPS, blocked));
    let blocker = red.task(1).open_blocker().cloned().unwrap();
    assert_eq!(blocker.params["source"], "scm");
    assert_eq!(blocker.params["checks"], json!(["test"]));
}

fn sim_with(ci: CiScript) -> Sim {
    sim(ci, None)
}

#[test]
fn the_scm_source_never_runs_saka_commands() {
    let mut sim = sim(CiScript::Pass, Some("[gate]\nsource = \"scm\"\n"));
    assert!(sim.run_until(STEPS, awaiting_merge));
    assert!(gate_commands(&sim).is_empty());
    let t = sim.task(1);
    let head = t.change_request.clone().unwrap().head_sha;
    assert!(t.gate_for(GateSource::Saka, &head).is_none());
}

#[test]
fn required_checks_ignore_others_and_block_when_missing() {
    let mut sim = sim(
        CiScript::Fail(vec!["lint".into()]),
        Some("[gate]\nsource = \"scm\"\nrequired_checks = [\"test\"]\n"),
    );
    assert!(sim.run_until(STEPS, |s| blocked(s) || awaiting_merge(s)));
    let blocker = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(blocker.params["reason"], "required_missing");
    assert_eq!(blocker.params["missing"], json!(["test"]));
}

#[test]
fn the_first_failing_command_stops_the_rest_and_timeouts_count_as_failures() {
    let mut sim = sim(
        CiScript::Pass,
        Some("[gate]\ncommands = [\"make slow\", \"make never\"]\n"),
    );
    sim.world
        .script_command("make slow", CommandScript::Timeout);
    assert!(sim.run_until(STEPS, blocked));
    assert_eq!(gate_commands(&sim), [STEP_COMMAND, "make slow"]);
    let t = sim.task(1);
    let head = t.change_request.clone().unwrap().head_sha;
    let saka = t.gate_for(GateSource::Saka, &head).unwrap();
    assert_eq!(
        saka.checks.last().unwrap().conclusion,
        Some(CheckConclusion::TimedOut)
    );
    assert_eq!(t.open_blocker().unwrap().params["timed_out"], true);
}

#[test]
fn long_commands_are_named_within_120_characters() {
    let long = format!("echo {}", "x".repeat(200));
    let mut sim = sim(
        CiScript::Pass,
        Some(&format!("[gate]\ncommands = [\"{long}\"]\n")),
    );
    assert!(sim.run_until(STEPS, awaiting_merge));
    let t = sim.task(1);
    let head = t.change_request.clone().unwrap().head_sha;
    let name = &t.gate_for(GateSource::Saka, &head).unwrap().checks[1].name;
    assert_eq!(name.chars().count(), 120);
    assert!(name.ends_with('…'));
}

#[test]
fn a_moved_worktree_fails_the_gate_without_running_commands() {
    let mut sim = sim(CiScript::Pass, None);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingCi)
    }));
    sim.world.dirty_worktrees();
    let before = gate_commands(&sim).len();
    assert!(sim.run_until(STEPS, blocked));
    let blocker = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(blocker.params["reason"], "worktree_moved");
    assert_eq!(gate_commands(&sim).len(), before);
}

#[test]
fn a_worktree_moved_while_the_commands_ran_is_not_reported_and_the_verify_runs_again() {
    let mut sim = sim(CiScript::Pass, None);
    sim.world
        .script_command(STEP_COMMAND, CommandScript::MovesWorktree);
    assert!(sim.run_until(STEPS, awaiting_merge));

    let ran = gate_commands(&sim)
        .iter()
        .filter(|c| c.as_str() == STEP_COMMAND)
        .count();
    assert_eq!(ran, 2);
    let head = sim.task(1).change_request.unwrap().head_sha;
    let observed: Vec<_> = sim
        .events_of("delivery.gate.observed")
        .into_iter()
        .filter(|e| e.payload["source"] == "saka")
        .collect();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].payload["head_sha"], head.as_str());
    assert_eq!(observed[0].payload["outcome"], "passed");
    assert!(sim.events_of("delivery.work_item.blocked").is_empty());
}

#[test]
fn a_crashing_runner_ends_as_runner_failed() {
    let mut sim = sim(CiScript::Pass, None);
    for _ in 0..3 {
        sim.world.script_command(STEP_COMMAND, CommandScript::Crash);
    }
    assert!(sim.run_until(STEPS, blocked));
    let blocker = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(blocker.code, BlockerCode::CiRed);
    assert_eq!(blocker.params["reason"], "runner_failed");
}

#[test]
fn a_saka_failure_retries_with_the_failed_command_and_its_output() {
    let mut cfg = SimConfig::default();
    cfg.operator.retry = vec![BlockerCode::CiRed];
    let mut sim = Sim::new(cfg);
    sim.world.set_recipe(Some("[gate]\nsource = \"saka\"\n"));
    sim.world
        .script_command(STEP_COMMAND, CommandScript::Exit(1));
    sim.world
        .command_output(STEP_COMMAND, "--- FAIL: TestDone (0.00s)");
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Done));
    let prompts = sim.world.prompts();
    let head = sim.task(1).change_request.unwrap().head_sha;
    let expected = "`cd cli && go test ./...` failed (exit code 1). Last output:\n```\n--- FAIL: TestDone (0.00s)\n```\nFix the failure, run the Done when commands again, and commit. Do not push.";
    assert!(
        prompts
            .iter()
            .any(|p| p.contains("Saka ran the Done when commands on ") && p.contains(expected)),
        "{prompts:#?}"
    );
    assert!(!head.is_empty());
}

#[test]
fn saka_results_are_reported_to_the_change_and_refusals_do_not_block() {
    let mut sim = sim(CiScript::Pass, None);
    assert!(sim.run_until(STEPS, |s| awaiting_merge(s)
        && s.world.statuses().len() >= 2));
    let head = sim.task(1).change_request.unwrap().head_sha;
    let states: Vec<(String, StatusState, String)> = sim
        .world
        .statuses()
        .into_iter()
        .map(|(sha, r)| (sha, r.state, r.description))
        .collect();
    assert_eq!(
        states,
        [
            (
                head.clone(),
                StatusState::Pending,
                "Running the Done when commands".to_string()
            ),
            (
                head.clone(),
                StatusState::Success,
                "1 commands passed".to_string()
            ),
        ]
    );
    assert!(
        sim.world
            .statuses()
            .iter()
            .all(|(_, r)| r.context == "saka/verify")
    );

    let mut refused = Sim::new(SimConfig::default());
    refused.world.refuse_statuses();
    refused.approve();
    assert!(refused.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Done));
}
