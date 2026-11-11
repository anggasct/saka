#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::model::Task;
use saka_core::vocab::{
    BlockerCode, ReviewBasis, ReviewEvent, ReviewOutcome, Stage, StagePhase, StageRunState,
    WorkItemState,
};
use saka_ports::LaunchProfile;
use saka_testkit::{
    AgentOutcome, AgentScript, ChangeSpec, Layout, ReviewReply, SHOP_REPO, Sim, SimConfig,
    repo_entry, saka_toml,
};

const STEPS: usize = 12_000;

const REVIEW_ONLY: &str = "[workflow]\npreset = \"custom\"\n\n[workflow.change_request]\n\n[workflow.stages]\nreview = \"auto\"\n";
const MANUAL_REVIEW: &str = "[workflow]\npreset = \"custom\"\n\n[workflow.change_request]\n\n[workflow.stages]\nreview = \"manual\"\n";

fn layout(workflow: &str) -> Layout {
    let mut text = saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]);
    text.push('\n');
    text.push_str(workflow);
    Layout {
        saka_toml: text,
        ..Layout::default()
    }
}

fn sim_with(workflow: &str, max_active_tasks: usize) -> Sim {
    let cfg = SimConfig {
        max_active_tasks,
        ..SimConfig::default()
    };
    Sim::multi_repo(cfg, &layout(workflow))
}

fn sim(workflow: &str) -> Sim {
    sim_with(workflow, 2)
}

fn outside(title: &str, branch: &str) -> ChangeSpec {
    ChangeSpec::new(title, "alice", branch)
}

fn only(sim: &Sim) -> Task {
    let tasks = sim.externals();
    assert_eq!(tasks.len(), 1, "one outside task");
    tasks.into_iter().next().unwrap()
}

fn reviewed(sim: &Sim) -> bool {
    sim.externals()
        .iter()
        .any(|t| t.is_at(Stage::Review, StagePhase::Reviewed))
}

fn runs(sim: &Sim, task: &Task) -> Vec<(u32, String, Option<String>, Option<String>)> {
    let id = task.item.id.0.to_string();
    sim.engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT attempt, state, head_sha, failure FROM stage_run WHERE work_item_id = ?1 ORDER BY attempt",
            )?;
            let rows = stmt.query_map([id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            Ok(rows.map(Result::unwrap).collect())
        })
        .unwrap()
}

fn open_and_review(sim: &mut Sim, title: &str, branch: &str) -> u64 {
    let number = sim
        .world
        .open_external_change(SHOP_REPO, outside(title, branch));
    assert!(sim.run_until(STEPS, reviewed), "the review finishes");
    number
}

#[test]
fn a_review_runs_in_a_fresh_read_only_session_and_posts_an_approval_at_the_head() {
    let mut sim = sim(REVIEW_ONLY);
    let number = open_and_review(&mut sim, "Add retries", "feature/retries");
    let task = only(&sim);

    assert_eq!(task.item.state, WorkItemState::InStage);
    assert!(task.open_blocker().is_none());
    let cr = task.change_request.as_ref().unwrap();
    let run = task.stage_run.as_ref().unwrap();
    assert_eq!(run.stage, Stage::Review);
    assert_eq!(run.state, StageRunState::Succeeded);
    assert_eq!(run.head_sha.as_deref(), Some(cr.head_sha.as_str()));

    let launches = sim.world.reviewer_launches();
    assert_eq!(launches.len(), 1);
    let launch = &launches[0];
    assert_eq!(launch.profile, LaunchProfile::Reviewer);
    assert!(launch.vars.is_empty() && launch.secrets.is_empty() && launch.path_prefix.is_empty());
    assert!(launch.extra_dirs.is_empty());
    assert_eq!(sim.world.launches().len(), 1, "no other session ran");

    let posts = sim.world.reviews();
    assert_eq!(posts.len(), 1);
    assert_eq!(
        (posts[0].number, posts[0].event, posts[0].head_sha.as_str()),
        (number, ReviewEvent::Approve, cr.head_sha.as_str())
    );
    assert!(posts[0].body.contains("No spec"), "{}", posts[0].body);

    assert_eq!(task.reviews.len(), 1);
    let review = &task.reviews[0];
    assert_eq!(review.outcome, ReviewOutcome::Approved);
    assert_eq!(review.basis, ReviewBasis::None);
    assert_eq!(review.head_sha, cr.head_sha);
    let posted = review.posted.as_ref().unwrap();
    assert_eq!(posted.event, ReviewEvent::Approve);
    assert!(posted.url.is_some());

    let env = task.environment.as_ref().unwrap();
    assert_eq!(
        env.state,
        saka_core::vocab::EnvironmentState::TornDown,
        "the checkout is released after the review"
    );
    assert_eq!(sim.events_of("delivery.review.recorded").len(), 1);
    assert_eq!(sim.events_of("delivery.review.posted").len(), 1);
    assert_eq!(sim.stats().prepares, 1);
}

#[test]
fn a_review_that_asks_for_changes_is_posted_as_such_and_waits_without_a_blocker() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world.script_reviews(vec![ReviewReply::request_changes(
        "The loop never backs off.",
    )]);
    let number = open_and_review(&mut sim, "Add retries", "feature/retries");
    let task = only(&sim);
    assert!(task.open_blocker().is_none());
    assert_eq!(task.reviews[0].outcome, ReviewOutcome::ChangesRequested);
    assert_eq!(sim.world.reviews()[0].event, ReviewEvent::RequestChanges);
    assert!(sim.world.reviews()[0].body.contains("backs off"));

    sim.world.merge_outside(number);
    assert!(sim.run_until(STEPS, |s| only(s).item.state == WorkItemState::Done));
}

#[test]
fn a_pull_request_closed_after_its_review_cancels_the_task() {
    let mut sim = sim(REVIEW_ONLY);
    let number = open_and_review(&mut sim, "Add retries", "feature/retries");
    sim.world.close_outside(number);
    assert!(sim.run_until(STEPS, |s| only(s).item.state == WorkItemState::Cancelled));
    assert_eq!(
        only(&sim).stage_run.unwrap().state,
        StageRunState::Succeeded
    );
}

#[test]
fn manual_review_waits_for_a_command_after_the_head_moves_past_a_finished_review() {
    let mut sim = sim(MANUAL_REVIEW);
    let number = sim
        .world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    assert!(sim.run_until(STEPS, |s| {
        s.externals()
            .iter()
            .any(|t| t.is_at(Stage::Review, StagePhase::AwaitingTrigger))
    }));
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let id = only(&sim).item.id;
    sim.block_on(engine.request_review_with(id, &cause, None))
        .unwrap();
    assert!(sim.run_until(STEPS, reviewed));
    sim.world.push_to_change(SHOP_REPO, number);
    assert!(sim.run_until(STEPS, |s| {
        only(s).is_at(Stage::Review, StagePhase::AwaitingTrigger)
    }));
    assert_eq!(sim.world.reviews().len(), 1, "nothing is reviewed unasked");
}

fn wait_for_blocker(sim: &mut Sim, code: BlockerCode) {
    assert!(sim.run_until(STEPS, |s| {
        s.externals()
            .iter()
            .any(|t| t.open_blocker().is_some_and(|b| b.code == code))
    }));
}

fn retry(sim: &Sim) {
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry(only(sim).item.id, "operator retry", &cause)
        .unwrap();
}

#[test]
fn the_basis_of_a_review_follows_the_spec_the_pull_request_names() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world
        .open_external_change(SHOP_REPO, outside("Implements tasks-done/s2", "feature/s2"));
    assert!(sim.run_until(STEPS, reviewed));
    let task = only(&sim);
    assert_eq!(task.reviews[0].basis, ReviewBasis::Step);
    let packet = sim.world.prompts().into_iter().next().unwrap();
    assert!(packet.contains("<change_request_content>"));
    assert!(
        !sim.world.reviews()[0].body.contains("No spec"),
        "a linked review is not marked"
    );
}

#[test]
fn spec_unclear_without_a_spec_is_a_format_error_and_asks_once_more() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world.script_reviews(vec![
        ReviewReply::spec_unclear("What is the limit?"),
        ReviewReply::approve(),
    ]);
    open_and_review(&mut sim, "Add retries", "feature/retries");
    let task = only(&sim);
    assert_eq!(task.reviews[0].outcome, ReviewOutcome::Approved);
    assert_eq!(
        sim.world.prompts().len(),
        2,
        "the packet and one correction"
    );
    assert!(sim.world.prompts()[1].contains("spec_unclear_without_spec"));
    assert_eq!(
        sim.world.launches().len(),
        1,
        "the same session answers twice"
    );
}

#[test]
fn an_unclear_spec_blocks_the_task_without_posting_and_a_retry_reviews_again() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world.script_reviews(vec![
        ReviewReply::spec_unclear("What is the limit?"),
        ReviewReply::approve(),
    ]);
    sim.world
        .open_external_change(SHOP_REPO, outside("Implements tasks-done/s2", "feature/s2"));
    wait_for_blocker(&mut sim, BlockerCode::SpecUnclear);
    let task = only(&sim);
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.params["questions"][0], "What is the limit?");
    assert_eq!(blocker.params["step_id"], "S2");
    assert_eq!(
        saka_core::blocker::primary_action(&task).unwrap().kind,
        saka_core::vocab::BlockerAction::EditSpec
    );
    assert!(sim.world.reviews().is_empty(), "nothing is posted");
    assert_eq!(task.reviews[0].outcome, ReviewOutcome::SpecUnclear);

    retry(&sim);
    assert!(sim.run_until(STEPS, reviewed));
    let launches = sim.world.reviewer_launches();
    assert_eq!(launches.len(), 2, "a retry is a new session");
    assert_eq!(sim.world.reviews().len(), 1);
    assert_eq!(runs(&sim, &only(&sim)).len(), 2);
}

#[test]
fn a_reply_outside_the_format_twice_blocks_and_a_retry_starts_a_fresh_session() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world.script_reviews(vec![
        ReviewReply::Raw("Sure, looks fine to me!".into()),
        ReviewReply::Raw("{\"verdict\": \"approve\"}".into()),
    ]);
    sim.world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    wait_for_blocker(&mut sim, BlockerCode::SessionFailed);
    let task = only(&sim);
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.params["cause"], "verdict_invalid");
    assert!(sim.world.reviews().is_empty());
    let first = task.session.as_ref().unwrap().id;

    retry(&sim);
    assert!(sim.run_until(STEPS, reviewed));
    let task = only(&sim);
    assert_ne!(task.session.as_ref().unwrap().id, first);
    assert!(
        sim.world.resumes().iter().all(Option::is_none),
        "a retried review never resumes the earlier conversation"
    );
    assert_eq!(sim.world.reviews().len(), 1);
}

fn hang() -> AgentScript {
    AgentScript {
        outcome: AgentOutcome::Hang,
        ..AgentScript::default()
    }
}

#[test]
fn a_push_during_the_review_cancels_it_and_reviews_the_new_head() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world.set_script(hang());
    let number = sim
        .world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    assert!(sim.run_until(STEPS, |s| {
        s.externals().iter().any(|t| {
            t.is_at(Stage::Review, StagePhase::RunningSession)
                && t.live_session().is_some_and(|s| s.turns == 1)
        })
    }));
    let old = only(&sim).change_request.unwrap().head_sha;
    let moved = sim.world.push_to_change(SHOP_REPO, number);
    assert!(sim.run_until(STEPS, |s| runs(s, &only(s)).len() == 2));
    sim.world.set_script(AgentScript::default());
    assert!(sim.run_until(STEPS, reviewed));

    let task = only(&sim);
    assert_eq!(task.change_request.as_ref().unwrap().head_sha, moved);
    let all = runs(&sim, &task);
    assert_eq!(all.len(), 2);
    assert_eq!(
        (all[0].1.as_str(), all[0].2.as_deref()),
        ("cancelled", Some(old.as_str()))
    );
    assert_eq!(
        (all[1].1.as_str(), all[1].2.as_deref()),
        ("succeeded", Some(moved.as_str()))
    );
    let posts = sim.world.reviews();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].head_sha, moved);
    assert_eq!(sim.world.reviewer_launches().len(), 2);
    assert!(
        sim.world.stats().sessions_stopped >= 1,
        "the old runner is stopped"
    );
    let env = task.environment.as_ref().unwrap();
    assert_eq!(env.state, saka_core::vocab::EnvironmentState::TornDown);
    assert_eq!(sim.world.stats().prepares, 2);
}

#[test]
fn a_push_while_a_manual_review_runs_waits_for_a_command() {
    let mut sim = sim(MANUAL_REVIEW);
    sim.world.set_script(hang());
    let number = sim
        .world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    assert!(sim.run_until(STEPS, |s| {
        s.externals()
            .iter()
            .any(|t| t.is_at(Stage::Review, StagePhase::AwaitingTrigger))
    }));
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let id = only(&sim).item.id;
    sim.block_on(engine.request_review_with(id, &cause, None))
        .unwrap();
    assert!(sim.run_until(STEPS, |s| {
        s.externals().iter().any(|t| {
            t.is_at(Stage::Review, StagePhase::RunningSession)
                && t.live_session().is_some_and(|s| s.turns == 1)
        })
    }));
    sim.world.push_to_change(SHOP_REPO, number);
    assert!(sim.run_until(STEPS, |s| {
        only(s).is_at(Stage::Review, StagePhase::AwaitingTrigger)
    }));
    sim.world.set_script(AgentScript::default());
    for _ in 0..300 {
        sim.step();
    }
    assert!(sim.world.reviews().is_empty(), "no review for the old head");
    assert!(sim.world.stats().sessions_stopped >= 1);
    assert_eq!(
        only(&sim).stage_run.unwrap().state,
        StageRunState::Cancelled
    );
}

#[test]
fn reviews_share_the_slots_of_the_loop_and_a_finished_review_frees_one() {
    let mut sim = sim_with(REVIEW_ONLY, 2);
    for n in 0..3 {
        sim.world.open_external_change(
            SHOP_REPO,
            outside(&format!("Change {n}"), &format!("feature/{n}")),
        );
    }
    let mut peak = 0;
    assert!(sim.run_until(STEPS * 2, |s| {
        let running = s
            .externals()
            .iter()
            .filter(|t| {
                t.is_at(Stage::Review, StagePhase::PreparingEnvironment)
                    || t.is_at(Stage::Review, StagePhase::RunningSession)
            })
            .count();
        peak = peak.max(running);
        s.externals().len() == 3
            && s.externals()
                .iter()
                .all(|t| t.is_at(Stage::Review, StagePhase::Reviewed))
    }));
    assert_eq!(peak, 2, "two reviews at a time, never three");
    assert_eq!(sim.world.reviews().len(), 3);
}

#[test]
fn no_slot_means_no_review_and_a_spec_step_fills_the_pool_first() {
    let mut sim = sim_with(
        "[workflow]\npreset = \"custom\"\nspec = true\n\n[workflow.change_request]\n\n[workflow.stages]\ndev = \"auto\"\nreview = \"auto\"\nmerge = \"manual\"\n",
        1,
    );
    sim.world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.all_tasks()
            .iter()
            .any(|t| t.item.spec_id.is_some() && t.item.state == WorkItemState::Admitted)
            || s.all_tasks()
                .iter()
                .any(|t| t.item.spec_id.is_some() && t.item.state == WorkItemState::InStage)
    }));
    for _ in 0..200 {
        sim.step();
    }
    assert!(
        sim.externals()
            .iter()
            .all(|t| t.is_at(Stage::Review, StagePhase::AwaitingReview)),
        "the only slot belongs to the step"
    );
}

#[test]
fn a_post_that_fails_is_retried_then_blocks_and_a_retry_posts_the_same_verdict() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world.fail_reviews(10);
    sim.world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    wait_for_blocker(&mut sim, BlockerCode::ScmUnavailable);
    let task = only(&sim);
    assert_eq!(
        task.open_blocker().unwrap().params["phase"],
        "posting_review"
    );
    assert_eq!(task.reviews.len(), 1);
    assert!(task.reviews[0].posted.is_none());
    let sessions = sim.world.launches().len();

    sim.world.fail_reviews(0);
    retry(&sim);
    assert!(sim.run_until(STEPS, reviewed));
    assert_eq!(sim.world.launches().len(), sessions, "no new session");
    assert_eq!(sim.world.reviews().len(), 1);
    assert_eq!(sim.events_of("delivery.review.posted").len(), 1);
    assert_eq!(sim.events_of("delivery.review.recorded").len(), 1);
}

#[test]
fn a_restart_in_the_middle_of_a_review_neither_repeats_the_run_nor_loses_it() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    assert!(sim.run_until(STEPS, |s| {
        s.externals()
            .iter()
            .any(|t| t.is_at(Stage::Review, StagePhase::RunningSession))
    }));
    sim.restart();
    assert!(sim.run_until(STEPS, reviewed));
    sim.crash_mid_job();
    for _ in 0..100 {
        sim.step();
    }
    let task = only(&sim);
    assert_eq!(runs(&sim, &task).len(), 1);
    assert_eq!(sim.world.reviews().len(), 1);
    assert_eq!(task.reviews.len(), 1);
}
