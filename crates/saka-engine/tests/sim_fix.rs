#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::blocker::primary_action;
use saka_core::model::Task;
use saka_core::vocab::{
    BlockerAction, BlockerCode, SessionState, SpecState, Stage, StagePhase, StageRunState, Trigger,
    WorkItemState,
};
use saka_core::workflow::Workflow;
use saka_engine::EngineError;
use saka_testkit::{
    AgentOutcome, AgentScript, FileEdit, HOME_REPO, Operator, ReviewReply, Sim, SimConfig,
};

const STEPS: usize = 20_000;

fn hand_merge() -> SimConfig {
    SimConfig {
        operator: Operator {
            start: true,
            merge: false,
            retry: vec![],
            answer: None,
            decide: None,
        },
        ..SimConfig::default()
    }
}

fn fresh_commits(sim: &Sim) {
    sim.world.script_edits(
        (1..=12)
            .map(|n| {
                let path = format!("src/change{n}.rs");
                let edit: FileEdit = (path.clone(), Some(format!("{path}\n")));
                vec![edit]
            })
            .collect(),
    );
}

fn awaiting_merge(sim: &Sim) -> bool {
    sim.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
}

fn blocked_on(sim: &Sim, code: BlockerCode) -> bool {
    sim.task(1).open_blocker().is_some_and(|b| b.code == code)
}

fn head(task: &Task) -> String {
    task.change_request.as_ref().unwrap().head_sha.clone()
}

fn retry(sim: &Sim) {
    let engine = sim.engine();
    engine
        .retry(
            sim.task(1).item.id,
            "one more round",
            &engine.cause("human:operator:local"),
        )
        .unwrap();
}

fn request_merge(sim: &Sim) -> Result<(), EngineError> {
    let engine = sim.engine();
    let task = sim.task(1);
    engine
        .request_merge(
            task.item.id,
            task.item.version,
            None,
            &engine.cause("human:operator:local"),
        )
        .map(|_| ())
}

fn stages(sim: &Sim) -> Vec<String> {
    let id = sim.task(1).item.id.0.to_string();
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

fn fixing(sim: &Sim) -> bool {
    sim.task(1).is_at(Stage::Fix, StagePhase::RunningSession)
}

fn rejects(times: usize) -> Vec<ReviewReply> {
    (0..times)
        .map(|_| ReviewReply::request_changes("Still wrong."))
        .collect()
}

#[test]
fn a_rejected_verdict_is_fixed_by_the_author_and_judged_again_at_the_new_head() {
    let mut sim = Sim::new(hand_merge());
    fresh_commits(&sim);
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::approve(),
    ]);
    sim.approve();
    assert!(sim.run_until(STEPS, fixing));

    let task = sim.task(1);
    let rejected = head(&task);
    let run = task.stage_run.as_ref().unwrap();
    assert_eq!((run.stage, run.trigger), (Stage::Fix, Trigger::Auto));
    let round = run.fix.as_ref().unwrap();
    assert_eq!(round.number, 1);
    assert_eq!(round.cause.code, "verdict_failed");
    assert!(task.open_blocker().is_none(), "no operator is asked");
    let author = sim.sessions(1)[0].clone();

    assert!(sim.run_until(STEPS, awaiting_merge));
    let task = sim.task(1);
    assert_ne!(head(&task), rejected, "the fix produced a new commit");
    assert!(task.approved_at(&head(&task)));
    assert!(!task.approved_at(&rejected));
    assert_eq!(task.reviews.len(), 2);
    assert_eq!(task.fix_rounds_used(), 1);
    assert!(
        sim.world.prompts().iter().any(|p| {
            p.contains("An independent review rejected") && p.contains("Cover the empty list.")
        }),
        "the author is told what the reviewer found"
    );
    assert!(
        sim.world
            .resumes()
            .contains(&Some(format!("cli-{}", author.id))),
        "the author continues its own conversation"
    );
    assert_eq!(
        sim.world.reviewer_launches().len(),
        2,
        "each head gets a new reviewer"
    );
    assert_eq!(
        stages(&sim),
        ["dev", "merge", "review", "fix", "merge", "review", "merge"]
    );

    request_merge(&sim).unwrap();
    sim.cfg.operator.merge = true;
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.spec().state, SpecState::Shipped);
}

#[test]
fn after_the_last_round_the_task_stops_and_each_retry_buys_one_more_until_cancel() {
    let mut sim = Sim::new(hand_merge());
    fresh_commits(&sim);
    sim.world.script_reviews(rejects(10));
    sim.approve();
    assert!(sim.run_until(STEPS, |s| blocked_on(s, BlockerCode::ReviewRoundsExhausted)));

    let task = sim.task(1);
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.params["rounds"], 2);
    assert_eq!(blocker.params["max"], 2);
    assert_eq!(blocker.params["head_sha"], head(&task).as_str());
    assert_eq!(task.fix_rounds_used(), 2);
    assert_eq!(primary_action(&task).unwrap().kind, BlockerAction::Retry);
    assert_eq!(
        stages(&sim).iter().filter(|s| s.as_str() == "fix").count(),
        2
    );

    for extra in 1..=3u32 {
        retry(&sim);
        assert!(
            sim.run_until(STEPS, |s| blocked_on(s, BlockerCode::ReviewRoundsExhausted)),
            "extra round {extra}"
        );
        assert_eq!(sim.task(1).fix_rounds_used(), 2 + extra);
    }
    assert_eq!(
        primary_action(&sim.task(1)).unwrap().kind,
        BlockerAction::Cancel,
        "the manual retry limit bounds the extra rounds"
    );
}

#[test]
fn a_manual_fix_waits_for_the_operator_and_a_retry_starts_the_round() {
    let mut workflow = Workflow::default();
    workflow.set_stage(Stage::Fix, Trigger::Manual);
    let mut sim = Sim::new(SimConfig {
        workflow,
        ..hand_merge()
    });
    fresh_commits(&sim);
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::approve(),
    ]);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| blocked_on(s, BlockerCode::VerdictFailed)));
    assert_eq!(sim.task(1).fix_rounds_used(), 0);

    retry(&sim);
    assert!(sim.run_until(STEPS, fixing));
    let task = sim.task(1);
    let run = task.stage_run.as_ref().unwrap();
    assert_eq!(
        (run.trigger, run.fix.as_ref().unwrap().number),
        (Trigger::Manual, 1)
    );
    assert!(sim.run_until(STEPS, awaiting_merge));
    assert_eq!(sim.task(1).fix_rounds_used(), 1);
}

fn hanging() -> AgentScript {
    AgentScript {
        outcome: AgentOutcome::Hang,
        ..AgentScript::default()
    }
}

fn fixing_with_a_live_agent() -> Sim {
    let mut sim = Sim::new(hand_merge());
    fresh_commits(&sim);
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::approve(),
    ]);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| !s.task(1).reviews.is_empty()));
    sim.world.set_script(hanging());
    assert!(sim.run_until(STEPS, |s| fixing(s) && s.task(1).live_session().is_some()));
    sim
}

#[test]
fn a_push_by_hand_while_the_fix_runs_blocks_the_task_for_the_operator() {
    let mut sim = fixing_with_a_live_agent();
    let number = sim.task(1).change_request.unwrap().number;
    let human = sim.world.push_to_change(HOME_REPO, number);

    assert!(sim.run_until(STEPS, |s| blocked_on(s, BlockerCode::BranchMoved)));
    let task = sim.task(1);
    assert_eq!(head(&task), human);
    assert_eq!(primary_action(&task).unwrap().kind, BlockerAction::Takeover);
    assert_eq!(sim.sessions(1).pop().unwrap().state, SessionState::Ended);
}

#[test]
fn a_task_taken_over_during_a_fix_stops_the_agent_and_cancels_the_round() {
    let mut sim = fixing_with_a_live_agent();
    let engine = sim.engine();
    engine
        .takeover_with(
            sim.task(1).item.id,
            "I will fix it",
            &engine.cause("human:operator:local"),
            None,
        )
        .unwrap();

    let task = sim.task(1);
    assert_eq!(task.item.state, WorkItemState::TakenOver);
    assert_eq!(
        task.stage_run.as_ref().unwrap().state,
        StageRunState::Cancelled
    );
    assert_eq!(sim.sessions(1).pop().unwrap().state, SessionState::Ended);
    assert_eq!(task.fix_rounds_used(), 1, "the round stays counted");

    sim.world.set_script(AgentScript::default());
    let id = task.item.id;
    sim.block_on(engine.return_with(
        id,
        Some(Stage::Fix),
        "back to the loop",
        &engine.cause("human:operator:local"),
        None,
    ))
    .unwrap();
    assert!(sim.run_until(STEPS, awaiting_merge));
    let task = sim.task(1);
    assert_eq!(
        task.fix_rounds_used(),
        1,
        "coming back is not another round"
    );
    assert!(task.approved_at(&head(&task)));
}

#[test]
fn a_restart_during_a_fix_does_not_start_another_round() {
    let mut sim = Sim::new(hand_merge());
    fresh_commits(&sim);
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::approve(),
    ]);
    sim.approve();
    assert!(sim.run_until(STEPS, fixing));
    let before = sim.task(1).fix_rounds_used();
    sim.restart();
    assert_eq!(sim.task(1).fix_rounds_used(), before);

    assert!(sim.run_until(STEPS, awaiting_merge));
    assert_eq!(sim.task(1).fix_rounds_used(), 1);
    assert_eq!(stages(&sim).iter().filter(|s| *s == "fix").count(), 1);
}
