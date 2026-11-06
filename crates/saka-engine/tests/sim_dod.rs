#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{BlockerCode, GateOutcome, GateSource, Stage, StagePhase, WorkItemState};
use saka_testkit::{CiScript, CommandScript, Sim, SimConfig};

const STEPS: usize = 8_000;
const STEP_COMMAND: &str = "cd cli && go test ./...";
const SAKA: &str = "[gate]\nsource = \"saka\"\n";
const SCM: &str = "[gate]\nsource = \"scm\"\n";

fn sim(ci: CiScript, recipe: Option<&str>) -> Sim {
    let mut cfg = SimConfig {
        ci,
        ..SimConfig::without_review()
    };
    cfg.operator.merge = false;
    cfg.operator.retry = vec![];
    let mut sim = Sim::new(cfg);
    sim.world.set_recipe(recipe);
    sim.approve();
    sim
}

fn blocked(s: &Sim) -> bool {
    s.task(1).item.state == WorkItemState::Blocked
}

fn done(s: &Sim) -> bool {
    s.task(1).item.state == WorkItemState::Done
}

fn blocker_code(s: &Sim) -> Option<BlockerCode> {
    s.task(1).open_blocker().map(|b| b.code)
}

fn merge_on_host(sim: &Sim) -> String {
    let number = sim.task(1).change_request.unwrap().number;
    sim.world.merge_outside(number);
    sim.world.base_head()
}

fn sync_target(s: &Sim) -> Option<String> {
    s.task(1)
        .stage_run
        .filter(|r| r.stage == Stage::Sync)
        .and_then(|r| r.head_sha)
}

fn count(sim: &Sim, sql: &str) -> i64 {
    let id = sim.task(1).item.id.0.to_string();
    sim.engine()
        .store()
        .read(|c| Ok(c.query_row(sql, [id], |r| r.get(0))?))
        .unwrap()
}

fn gate_runs(sim: &Sim) -> usize {
    sim.world
        .recipe_runs()
        .iter()
        .filter(|r| r.phase == "gate")
        .count()
}

fn red_saka_gate_then_merged(times: usize, recipe: &str) -> (Sim, String) {
    let mut sim = sim(CiScript::Silent, Some(recipe));
    for _ in 0..times {
        sim.world
            .script_command(STEP_COMMAND, CommandScript::Exit(1));
    }
    assert!(sim.run_until(STEPS, blocked));
    assert_eq!(blocker_code(&sim), Some(BlockerCode::CiRed));
    let merge_commit = merge_on_host(&sim);
    (sim, merge_commit)
}

#[test]
fn a_proven_merge_goes_straight_to_teardown() {
    let mut sim = sim(CiScript::Pass, None);
    sim.cfg.operator.merge = true;
    assert!(sim.run_to_shipped(STEPS));
    assert!(sim.world.checkouts().is_empty());
    assert_eq!(
        count(
            &sim,
            "SELECT COUNT(*) FROM job WHERE kind = 'scm.poll_commit'
             AND json_extract(payload, '$.work_item_id') = ?1"
        ),
        0
    );
    let metrics = sim.engine().metrics().unwrap();
    assert_eq!(metrics.dod_verifications, vec![("proven".to_string(), 2)]);
}

#[test]
fn a_host_merge_with_every_source_passed_is_proven() {
    let mut sim = sim(CiScript::Pass, None);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    merge_on_host(&sim);
    assert!(sim.run_until(STEPS, done));
    assert!(sim.world.checkouts().is_empty());
}

#[test]
fn an_unproven_merge_is_verified_at_its_merge_commit_before_the_next_step() {
    let (mut sim, merge_commit) = red_saka_gate_then_merged(1, SAKA);
    let gates_before = gate_runs(&sim);
    assert!(sim.run_until(STEPS, done));

    let env = sim.task(1).environment.unwrap().id;
    assert_eq!(sim.world.checkouts(), vec![(env, merge_commit.clone())]);
    assert_eq!(gate_runs(&sim), gates_before + 1);
    let sync_rows = count(
        &sim,
        "SELECT COUNT(*) FROM gate_result g JOIN stage_run r ON r.id = g.stage_run_id
         WHERE g.work_item_id = ?1 AND r.stage = 'sync' AND g.source = 'saka' AND g.outcome = 'passed'",
    );
    assert_eq!(sync_rows, 1);
    assert!(sim.run_until(STEPS, |s| s.task(2).item.state != WorkItemState::Queued));
    let metrics = sim.engine().metrics().unwrap();
    assert_eq!(metrics.dod_verifications, vec![("passed".to_string(), 1)]);
}

#[test]
fn a_failed_verification_blocks_the_task_and_holds_the_next_step() {
    let (mut sim, merge_commit) = red_saka_gate_then_merged(2, SAKA);
    sim.world.command_output(STEP_COMMAND, "--- FAIL: TestDone");
    assert!(sim.run_until(STEPS, |s| {
        blocker_code(s) == Some(BlockerCode::DodFailed)
    }));
    let b = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(b.code, BlockerCode::DodFailed);
    assert_eq!(b.params["reason"], "verify_failed");
    assert_eq!(b.params["source"], "saka");
    assert_eq!(b.params["head_sha"], merge_commit.as_str());
    assert_eq!(b.params["command"], STEP_COMMAND);
    assert_eq!(b.params["exit_code"], 1);
    assert!(b.params["log"].as_str().is_some());
    assert_eq!(b.params["log_tail"], "--- FAIL: TestDone");

    let at = sim.engine().now();
    assert!(!sim.run_until(400, |s| !blocked(s)
        || s.engine().now().since(at) > 3_600_000));
    assert!(
        blocked(&sim),
        "a failed verification is not retried by itself"
    );
    assert_eq!(sim.task(2).item.state, WorkItemState::Queued);
    assert_eq!(
        sim.task(1).environment.unwrap().state,
        saka_core::vocab::EnvironmentState::Ready
    );
    let metrics = sim.engine().metrics().unwrap();
    assert_eq!(metrics.dod_verifications, vec![("failed".to_string(), 1)]);
}

#[test]
fn a_ci_only_repo_reads_ci_at_the_merge_commit() {
    let mut sim = sim(CiScript::Fail(vec!["test".into()]), Some(SCM));
    assert!(sim.run_until(STEPS, blocked));
    let merge_commit = merge_on_host(&sim);
    sim.world.set_commit_ci(CiScript::Pass);
    assert!(sim.run_until(STEPS, done));
    assert!(
        sim.world.checkouts().is_empty(),
        "scm alone runs no command"
    );
    let scm = count(
        &sim,
        "SELECT COUNT(*) FROM gate_result g JOIN stage_run r ON r.id = g.stage_run_id
         WHERE g.work_item_id = ?1 AND r.stage = 'sync' AND g.source = 'scm' AND g.outcome = 'passed'",
    );
    assert_eq!(scm, 1);
    assert_eq!(
        sim.task(1).change_request.unwrap().merge_commit_sha,
        Some(merge_commit)
    );
}

#[test]
fn red_ci_at_the_merge_commit_blocks_with_its_checks() {
    let mut sim = sim(CiScript::Fail(vec!["test".into()]), Some(SCM));
    assert!(sim.run_until(STEPS, blocked));
    merge_on_host(&sim);
    sim.world.set_commit_ci(CiScript::Fail(vec!["test".into()]));
    assert!(sim.run_until(STEPS, |s| {
        blocker_code(s) == Some(BlockerCode::DodFailed)
    }));
    let b = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(b.params["reason"], "verify_failed");
    assert_eq!(b.params["source"], "scm");
    assert_eq!(b.params["checks"], serde_json::json!(["test"]));
}

#[test]
fn no_ci_at_the_merge_commit_blocks_a_ci_only_repo() {
    let mut sim = sim(CiScript::Fail(vec!["test".into()]), Some(SCM));
    assert!(sim.run_until(STEPS, blocked));
    merge_on_host(&sim);
    assert!(sim.run_until(STEPS, |s| {
        blocker_code(s) == Some(BlockerCode::DodFailed)
    }));
    assert_eq!(
        sim.task(1).open_blocker().unwrap().params["reason"],
        "no_checks"
    );
}

#[test]
fn a_missing_required_check_at_the_merge_commit_blocks() {
    let mut sim = sim(
        CiScript::Fail(vec!["test".into()]),
        Some("[gate]\nsource = \"scm\"\nrequired_checks = [\"build\"]\n"),
    );
    assert!(sim.run_until(STEPS, blocked));
    merge_on_host(&sim);
    sim.world.set_commit_ci(CiScript::Pass);
    assert!(sim.run_until(STEPS, |s| {
        blocker_code(s) == Some(BlockerCode::DodFailed)
    }));
    let b = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(b.params["reason"], "required_missing");
    assert_eq!(b.params["missing"], serde_json::json!(["build"]));
}

#[test]
fn auto_without_ci_at_the_merge_commit_relies_on_the_saka_gate() {
    let (mut sim, merge_commit) = red_saka_gate_then_merged(1, "[gate]\nsource = \"auto\"\n");
    assert!(sim.run_until(STEPS, done));
    let t = sim.task(1);
    assert!(
        t.gate_for(GateSource::Scm, &merge_commit)
            .is_none_or(|g| g.checks.is_empty())
    );
    assert_eq!(
        t.gate_for(GateSource::Saka, &merge_commit)
            .map(|g| g.outcome),
        Some(GateOutcome::Passed)
    );
}

#[test]
fn a_retry_after_a_fix_verifies_the_tip_of_the_base() {
    let (mut sim, merge_commit) = red_saka_gate_then_merged(2, SAKA);
    assert!(sim.run_until(STEPS, |s| {
        blocker_code(s) == Some(BlockerCode::DodFailed)
    }));
    let fix = sim.world.push_to_base();
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry(sim.task(1).item.id, "fixed on main", &cause)
        .unwrap();
    assert!(sim.run_until(STEPS, done));
    let env = sim.task(1).environment.unwrap().id;
    assert_eq!(sim.world.checkouts(), vec![(env, merge_commit), (env, fix)]);
    assert!(sim.run_until(STEPS, |s| s.task(2).item.state != WorkItemState::Queued));
}

#[test]
fn a_retry_without_a_fix_fails_again_at_the_tip_of_the_base() {
    let (mut sim, _) = red_saka_gate_then_merged(3, SAKA);
    assert!(sim.run_until(STEPS, |s| {
        blocker_code(s) == Some(BlockerCode::DodFailed)
    }));
    let other = sim.world.push_to_base();
    let first = sim.task(1).open_blocker().unwrap().id;
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry(sim.task(1).item.id, "try again", &cause)
        .unwrap();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .open_blocker()
            .is_some_and(|b| b.id != first && b.code == BlockerCode::DodFailed)
    }));
    let b = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(b.params["head_sha"], other.as_str());
    assert_eq!(sync_target(&sim), Some(other));
}

#[test]
fn a_crashed_verification_runner_is_retried_by_itself() {
    let (mut sim, _) = red_saka_gate_then_merged(1, SAKA);
    for _ in 0..3 {
        sim.world.script_command(STEP_COMMAND, CommandScript::Crash);
    }
    assert!(sim.run_until(STEPS, |s| {
        blocker_code(s) == Some(BlockerCode::DodFailed)
    }));
    let b = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(b.params["reason"], "runner_failed");
    assert!(sim.run_until(STEPS, |s| !blocked(s)));
    assert!(sim.engine().now().since(b.opened_at) >= 60_000);
    assert!(sim.run_until(STEPS, done));
}

#[test]
fn an_unreachable_scm_during_verification_is_retried_by_itself() {
    let mut sim = sim(CiScript::Fail(vec!["test".into()]), Some(SCM));
    assert!(sim.run_until(STEPS, blocked));
    sim.world.fail_commit_reads(true);
    merge_on_host(&sim);
    assert!(sim.run_until(STEPS, |s| {
        blocker_code(s) == Some(BlockerCode::ScmUnavailable)
    }));
    assert_eq!(
        sim.task(1).open_blocker().unwrap().params["phase"],
        "verifying"
    );
    sim.world.fail_commit_reads(false);
    sim.world.set_commit_ci(CiScript::Pass);
    assert!(sim.run_until(STEPS, done));
}

#[test]
fn a_merge_commit_missing_from_the_base_blocks_as_not_in_base() {
    let mut sim = sim(CiScript::Pass, None);
    sim.world.set_lose_merges(true);
    sim.cfg.operator.merge = true;
    assert!(sim.run_until(STEPS, blocked));
    let b = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(b.code, BlockerCode::DodFailed);
    assert_eq!(b.params["reason"], "not_in_base");
}
