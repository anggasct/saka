#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::model::Task;
use saka_core::rejection::Rejection;
use saka_core::vocab::{BlockerCode, Stage, StagePhase, StageRunState};
use saka_engine::EngineError;
use saka_ports::LaunchProfile;
use saka_testkit::{
    AgentOutcome, AgentScript, ChangeSpec, Layout, ReviewReply, SHOP_REPO, Sim, SimConfig,
    repo_entry, saka_toml,
};

const STEPS: usize = 12_000;

const FIX_AUTO: &str =
    "[workflow]\npreset = \"review_only\"\n\n[workflow.stages]\nfix = \"auto\"\n";
const FIX_MANUAL: &str =
    "[workflow]\npreset = \"review_only\"\n\n[workflow.stages]\nfix = \"manual\"\n";
const ONE_ROUND: &str =
    "[workflow]\npreset = \"review_only\"\nfix_rounds = 1\n\n[workflow.stages]\nfix = \"auto\"\n";
const FORKS_MANUAL: &str = "[workflow]\npreset = \"review_only\"\n\n[workflow.change_request]\ninclude_forks = true\n\n[workflow.stages]\nfix = \"manual\"\n";
const WITH_FORKS: &str = "[workflow]\npreset = \"review_only\"\n\n[workflow.change_request]\ninclude_forks = true\n\n[workflow.stages]\nfix = \"auto\"\n";

fn sim(workflow: &str) -> Sim {
    let mut text = saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]);
    text.push('\n');
    text.push_str(workflow);
    let layout = Layout {
        saka_toml: text,
        ..Layout::default()
    };
    let sim = Sim::multi_repo(SimConfig::default(), &layout);
    sim.world.script_edits(
        (1..=4)
            .map(|n| vec![(format!("src/fix{n}.rs"), Some("fixed\n".to_string()))])
            .collect(),
    );
    sim
}

fn only(sim: &Sim) -> Task {
    let tasks = sim.externals();
    assert_eq!(tasks.len(), 1, "one outside task");
    tasks.into_iter().next().unwrap()
}

fn at(sim: &Sim, check: impl Fn(&Task) -> bool) -> bool {
    sim.externals().iter().any(check)
}

fn reviews(sim: &Sim, n: usize) -> bool {
    sim.externals()
        .iter()
        .any(|t| t.reviews.len() == n && t.is_at(Stage::Review, StagePhase::Reviewed))
}

fn stages(sim: &Sim) -> Vec<String> {
    let id = only(sim).item.id.0.to_string();
    sim.engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT stage FROM stage_run WHERE work_item_id = ?1 ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([id], |r| r.get::<_, String>(0))?;
            Ok(rows.map(Result::unwrap).collect())
        })
        .unwrap()
}

fn ask_for_a_fix(sim: &Sim) -> Result<(), EngineError> {
    let engine = sim.engine();
    let id = only(sim).item.id;
    sim.block_on(engine.request_fix_with(id, &engine.cause("human:operator:local"), None))
        .map(|_| ())
}

fn refused(result: Result<(), EngineError>) -> &'static str {
    match result {
        Err(EngineError::Rejected(Rejection::FixNotAvailable { reason })) => reason,
        other => panic!("expected a refused fix, got {other:?}"),
    }
}

fn open(sim: &Sim) -> u64 {
    sim.world.open_external_change(
        SHOP_REPO,
        ChangeSpec::new("Add retries", "alice", "feature/retries"),
    )
}

#[test]
fn a_rejected_change_from_our_own_repo_is_fixed_on_its_branch_and_reviewed_again() {
    let mut sim = sim(FIX_AUTO);
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::approve(),
    ]);
    open(&sim);
    assert!(sim.run_until(STEPS, |s| reviews(s, 2)));

    let task = only(&sim);
    assert_eq!(stages(&sim), ["review", "fix", "review"]);
    assert_eq!(task.fix_rounds_used(), 1);
    assert!(task.open_blocker().is_none());
    let cr = task.change_request.as_ref().unwrap();
    assert!(task.approved_at(&cr.head_sha), "the new head was approved");
    assert_eq!(sim.stats().pushes, 1);

    let launches = sim.world.launches();
    let profiles: Vec<LaunchProfile> = launches.iter().map(|l| l.profile.clone()).collect();
    assert_eq!(
        profiles,
        [
            LaunchProfile::Reviewer,
            LaunchProfile::Task,
            LaunchProfile::Reviewer
        ]
    );
    assert!(
        sim.world
            .prompts()
            .iter()
            .any(|p| { p.contains("<review_findings>") && p.contains("Cover the empty list.") }),
        "the fix agent gets the findings as data"
    );
    assert_eq!(
        sim.world.reviews().len(),
        2,
        "both verdicts were posted, one per head"
    );
}

#[test]
fn a_fix_waits_for_the_operator_when_the_workflow_says_manual() {
    let mut sim = sim(FIX_MANUAL);
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::approve(),
    ]);
    open(&sim);
    assert!(sim.run_until(STEPS, |s| reviews(s, 1)));
    sim.run_until(300, |_| false);
    assert_eq!(stages(&sim), ["review"], "nothing starts by itself");
    assert!(saka_core::task::available_actions(&only(&sim)).contains(&"fix"));

    ask_for_a_fix(&sim).unwrap();
    assert!(sim.run_until(STEPS, |s| reviews(s, 2)));
    assert_eq!(stages(&sim), ["review", "fix", "review"]);
    assert!(!saka_core::task::available_actions(&only(&sim)).contains(&"fix"));
}

#[test]
fn a_change_from_a_fork_is_never_fixed_and_nothing_is_pushed() {
    let mut sim = sim(WITH_FORKS);
    sim.world
        .script_reviews(vec![ReviewReply::request_changes("Cover the empty list.")]);
    let mut change = ChangeSpec::new("From a fork", "mallory", "patch-1");
    change.fork = true;
    sim.world.open_external_change(SHOP_REPO, change);
    assert!(sim.run_until(STEPS, |s| reviews(s, 1)));
    sim.run_until(300, |_| false);

    assert_eq!(stages(&sim), ["review"], "auto does not apply to a fork");
    assert!(!saka_core::task::available_actions(&only(&sim)).contains(&"fix"));
    assert_eq!(refused(ask_for_a_fix(&sim)), "fork");
    assert_eq!(sim.stats().pushes, 0);
    assert_eq!(sim.world.launches().len(), 1, "only the reviewer ran");
}

#[test]
fn a_change_nobody_rejected_has_nothing_to_fix_and_the_rounds_are_limited() {
    let mut sim = sim(ONE_ROUND);
    sim.world.script_reviews(vec![
        ReviewReply::approve(),
        ReviewReply::request_changes("Cover the empty list."),
    ]);
    open(&sim);
    assert!(sim.run_until(STEPS, |s| reviews(s, 1)));
    assert_eq!(refused(ask_for_a_fix(&sim)), "no_findings");

    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::request_changes("Still wrong."),
    ]);
    let number = only(&sim).change_request.unwrap().number;
    sim.world.push_to_change(SHOP_REPO, number);
    assert!(sim.run_until(STEPS, |s| reviews(s, 3)));
    assert_eq!(stages(&sim).iter().filter(|s| *s == "fix").count(), 1);
    assert_eq!(refused(ask_for_a_fix(&sim)), "rounds_exhausted");
    assert!(only(&sim).open_blocker().is_none());
}

#[test]
fn a_push_the_provider_refuses_blocks_the_task_and_is_not_tried_again() {
    let mut sim = sim(FIX_AUTO);
    sim.world
        .script_reviews(vec![ReviewReply::request_changes("Cover the empty list.")]);
    open(&sim);
    assert!(sim.run_until(STEPS, |s| {
        at(s, |t| t.is_at(Stage::Fix, StagePhase::Pushing))
    }));
    sim.world.push_outside(
        SHOP_REPO,
        "feature/retries",
        &[("other.rs".to_string(), Some("theirs\n".to_string()))],
    );

    assert!(sim.run_until(STEPS, |s| {
        at(s, |t| {
            t.open_blocker()
                .is_some_and(|b| b.code == BlockerCode::PushRejected)
        })
    }));
    assert_eq!(
        sim.stats().pushes,
        0,
        "nothing of ours landed and nothing was sent twice"
    );
}

#[test]
fn a_push_by_hand_while_the_fix_runs_drops_the_round_and_reviews_the_new_head() {
    let mut sim = sim(FIX_AUTO);
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::approve(),
    ]);
    open(&sim);
    assert!(sim.run_until(STEPS, |s| at(s, |t| !t.reviews.is_empty())));
    sim.world.set_script(AgentScript {
        outcome: AgentOutcome::Hang,
        ..AgentScript::default()
    });
    assert!(sim.run_until(STEPS, |s| {
        at(s, |t| {
            t.is_at(Stage::Fix, StagePhase::RunningSession) && t.live_session().is_some()
        })
    }));
    let number = only(&sim).change_request.unwrap().number;
    let human = sim.world.push_to_change(SHOP_REPO, number);
    assert!(sim.run_until(STEPS, |s| {
        at(s, |t| {
            t.stage_run
                .as_ref()
                .is_some_and(|r| r.stage == Stage::Review)
        })
    }));
    sim.world.set_script(AgentScript::default());
    assert!(sim.run_until(STEPS, |s| reviews(s, 2)));
    let task = only(&sim);
    assert!(
        task.approved_at(&human),
        "the head the human pushed was reviewed"
    );
    let fix = task
        .retired_runs
        .iter()
        .find(|r| r.stage == Stage::Fix)
        .unwrap();
    assert_eq!(fix.state, StageRunState::Cancelled);
    assert_eq!(sim.stats().pushes, 0, "Saka never pushed over theirs");
}

#[test]
fn a_comment_from_someone_who_can_write_starts_the_round_once() {
    let mut sim = sim(FIX_MANUAL);
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::approve(),
    ]);
    let number = open(&sim);
    assert!(sim.run_until(STEPS, |s| reviews(s, 1)));

    sim.world.set_write_access("bob", true);
    sim.world
        .comment_on_change(SHOP_REPO, number, "bob", "Looks fixable.\n/saka fix\n");
    assert!(sim.run_until(STEPS, |s| reviews(s, 2)));
    assert_eq!(stages(&sim), ["review", "fix", "review"]);
    let fix = only(&sim)
        .retired_runs
        .into_iter()
        .find(|r| r.stage == Stage::Fix)
        .unwrap();
    assert_eq!(fix.reason.as_deref(), Some("comment by bob"));
    sim.run_until(300, |_| false);
    assert_eq!(stages(&sim).len(), 3, "the comment is not read twice");
}

#[test]
fn a_comment_from_someone_who_cannot_write_or_on_a_fork_starts_nothing() {
    let mut sim = sim(FORKS_MANUAL);
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::request_changes("Cover the empty list."),
    ]);
    let number = open(&sim);
    let mut fork = ChangeSpec::new("From a fork", "mallory", "patch-1");
    fork.fork = true;
    let forked = sim.world.open_external_change(SHOP_REPO, fork);
    assert!(sim.run_until(STEPS, |s| {
        s.externals().len() == 2
            && s.externals()
                .iter()
                .all(|t| t.is_at(Stage::Review, StagePhase::Reviewed))
    }));
    sim.world.set_write_access("bob", true);
    sim.world
        .comment_on_change(SHOP_REPO, number, "stranger", "/saka fix");
    sim.world
        .comment_on_change(SHOP_REPO, forked, "bob", "/saka fix");
    sim.run_until(600, |_| false);

    assert!(
        sim.externals()
            .iter()
            .all(|t| t.stage_run.as_ref().unwrap().stage == Stage::Review),
        "neither comment started a round"
    );
    assert_eq!(sim.stats().pushes, 0);
    let denied = sim
        .engine()
        .metrics()
        .unwrap()
        .intake_changes
        .into_iter()
        .find(|(outcome, _)| outcome == "command_denied")
        .map_or(0, |(_, n)| n);
    assert_eq!(denied, 2, "both comments were counted as refused");
}
