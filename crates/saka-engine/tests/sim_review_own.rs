#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::blocker::primary_action;
use saka_core::model::Task;
use saka_core::rejection::Rejection;
use saka_core::vocab::{
    BlockerAction, BlockerCode, GateOutcome, ReviewBasis, ReviewEvent, ReviewOutcome, SessionState,
    SpecState, Stage, StagePhase, StageRunState, WorkItemState,
};
use saka_engine::EngineError;
use saka_ports::LaunchProfile;
use saka_testkit::{FileEdit, HOME_REPO, Operator, ReviewReply, Sim, SimConfig, without_review};

const STEPS: usize = 20_000;

fn hand_merge() -> SimConfig {
    SimConfig {
        operator: Operator {
            merge: false,
            retry: vec![],
            answer: None,
        },
        ..SimConfig::default()
    }
}

fn edit(path: &str) -> FileEdit {
    (path.to_string(), Some(format!("{path}\n")))
}

fn fresh_commits(sim: &Sim) {
    sim.world.script_edits(
        (1..=8)
            .map(|n| vec![edit(&format!("src/change{n}.rs"))])
            .collect(),
    );
}

fn awaiting_merge(sim: &Sim) -> bool {
    sim.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
}

fn blocked_on(sim: &Sim, code: BlockerCode) -> bool {
    sim.task(1).open_blocker().is_some_and(|b| b.code == code)
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

fn retry(sim: &Sim) {
    let engine = sim.engine();
    engine
        .retry(
            sim.task(1).item.id,
            "send it back",
            &engine.cause("human:operator:local"),
        )
        .unwrap();
}

fn runs(sim: &Sim) -> Vec<(String, String, Option<String>)> {
    let id = sim.task(1).item.id.0.to_string();
    sim.engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT stage, state, head_sha FROM stage_run WHERE work_item_id = ?1 ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.map(Result::unwrap).collect())
        })
        .unwrap()
}

fn head(task: &Task) -> String {
    task.change_request.as_ref().unwrap().head_sha.clone()
}

#[test]
fn a_task_is_reviewed_after_ci_by_a_fresh_read_only_session_and_merges_on_the_approval() {
    let mut sim = Sim::new(hand_merge());
    sim.approve();
    assert!(sim.run_until(STEPS, awaiting_merge));

    let task = sim.task(1);
    let head = head(&task);
    assert_eq!(task.reviews.len(), 1);
    let verdict = &task.reviews[0];
    assert_eq!(
        (verdict.outcome, verdict.basis, verdict.head_sha.as_str()),
        (ReviewOutcome::Approved, ReviewBasis::Step, head.as_str())
    );
    assert!(saka_core::task::merge_ready(&task));
    assert!(!task.gates.is_empty(), "the gates survive the review");
    assert!(
        task.gates
            .iter()
            .all(|g| g.head_sha == head && g.outcome == GateOutcome::Passed)
    );
    assert_eq!(sim.stats().merges, 0, "nothing merges without the operator");

    let launches = sim.world.reviewer_launches();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0].profile, LaunchProfile::Reviewer);
    assert!(
        launches[0].vars.is_empty() && launches[0].secrets.is_empty(),
        "the reviewer gets neither recipe variables nor secrets"
    );
    let sessions = sim.sessions(1);
    assert_eq!(sessions.len(), 2, "the author and the reviewer");
    assert_ne!(sessions[0].id, sessions[1].id);
    assert_eq!(sessions[0].environment_id, sessions[1].environment_id);
    assert!(sessions.iter().all(|s| s.state == SessionState::Ended));
    assert_eq!(sessions[1].resumed_from_session_id, None);
    assert_eq!(
        sim.world.resumes(),
        vec![None, None],
        "no conversation is shared"
    );

    let posts = sim.world.reviews();
    assert_eq!(posts.len(), 1);
    assert_eq!(
        (posts[0].event, posts[0].head_sha.as_str()),
        (ReviewEvent::Comment, head.as_str()),
        "a change of ours is reviewed with a comment"
    );

    request_merge(&sim).unwrap();
    sim.cfg.operator.merge = true;
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.spec().state, SpecState::Shipped);
    assert_eq!(sim.task(1).item.state, WorkItemState::Done);
    let stages: Vec<(String, String)> = runs(&sim).into_iter().map(|(s, st, _)| (s, st)).collect();
    assert_eq!(
        stages,
        [
            ("dev", "succeeded"),
            ("merge", "succeeded"),
            ("review", "succeeded"),
            ("merge", "succeeded"),
            ("sync", "succeeded"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
}

#[test]
fn a_head_that_moves_after_the_approval_is_checked_and_reviewed_again_before_the_merge() {
    let mut sim = Sim::new(hand_merge());
    sim.approve();
    assert!(sim.run_until(STEPS, awaiting_merge));
    let first = head(&sim.task(1));
    let number = sim.task(1).change_request.unwrap().number;

    let moved = sim.world.push_to_change(HOME_REPO, number);
    assert_ne!(moved, first);
    let mut asked_early = false;
    let settled = sim.run_until(STEPS, |s| {
        let t = s.task(1);
        if t.is_at(Stage::Merge, StagePhase::AwaitingMerge) {
            assert!(
                t.approved_at(&head(&t)),
                "the merge never opens on a head nobody approved"
            );
        }
        if t.is_at(Stage::Merge, StagePhase::AwaitingCi) && !asked_early {
            asked_early = true;
            assert!(matches!(
                request_merge(s),
                Err(EngineError::Rejected(Rejection::MergeNotReady { .. }))
            ));
        }
        t.is_at(Stage::Merge, StagePhase::AwaitingMerge) && head(&t) == moved
    });
    assert!(settled);
    assert!(asked_early, "CI looked at the new head first");

    let task = sim.task(1);
    let heads: Vec<&str> = task.reviews.iter().map(|r| r.head_sha.as_str()).collect();
    assert_eq!(heads, [first.as_str(), moved.as_str()]);
    assert_eq!(sim.world.reviewer_launches().len(), 2);
    assert_eq!(sim.world.reviews().len(), 2);

    request_merge(&sim).unwrap();
    sim.cfg.operator.merge = true;
    assert!(sim.run_to_shipped(STEPS));
}

#[test]
fn a_rejected_verdict_blocks_the_task_and_a_retry_hands_the_findings_to_the_author() {
    let mut sim = Sim::new(hand_merge());
    fresh_commits(&sim);
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::approve(),
    ]);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| blocked_on(s, BlockerCode::VerdictFailed)));

    let task = sim.task(1);
    let rejected = head(&task);
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.params["head_sha"], rejected.as_str());
    assert_eq!(blocker.params["findings"], 1);
    assert_eq!(blocker.params["summary"], "Something needs to change.");
    assert_eq!(primary_action(&task).unwrap().kind, BlockerAction::Retry);
    let posts = sim.world.reviews();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].event, ReviewEvent::Comment);
    let author = sim.sessions(1)[0].clone();

    retry(&sim);
    assert!(sim.run_until(STEPS, awaiting_merge));
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
        "the author continues its own conversation: {:?} / {:?}",
        sim.world.resumes(),
        author.id
    );
    let task = sim.task(1);
    assert_ne!(head(&task), rejected, "the retry produced a new commit");
    assert!(task.approved_at(&head(&task)));
    assert_eq!(task.reviews.len(), 2);
    let reviewer_sessions = sim.world.reviewer_launches().len();
    assert_eq!(reviewer_sessions, 2, "each review is a new session");

    request_merge(&sim).unwrap();
    sim.cfg.operator.merge = true;
    assert!(sim.run_to_shipped(STEPS));
}

#[test]
fn after_three_rejected_rounds_the_only_action_left_is_to_cancel() {
    let mut sim = Sim::new(hand_merge());
    fresh_commits(&sim);
    sim.world.script_reviews(
        (0..4)
            .map(|_| ReviewReply::request_changes("Still wrong."))
            .collect(),
    );
    sim.approve();
    for round in 0..3 {
        assert!(
            sim.run_until(STEPS, |s| blocked_on(s, BlockerCode::VerdictFailed)),
            "round {round}"
        );
        assert_eq!(
            primary_action(&sim.task(1)).unwrap().kind,
            BlockerAction::Retry,
            "round {round}"
        );
        retry(&sim);
    }
    assert!(sim.run_until(STEPS, |s| blocked_on(s, BlockerCode::VerdictFailed)));
    assert_eq!(
        primary_action(&sim.task(1)).unwrap().kind,
        BlockerAction::Cancel
    );
}

#[test]
fn an_unclear_spec_stops_the_task_with_the_questions_and_a_retry_reviews_it_again() {
    let mut sim = Sim::new(hand_merge());
    sim.world.script_reviews(vec![
        ReviewReply::spec_unclear("Which list is meant?"),
        ReviewReply::approve(),
    ]);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| blocked_on(s, BlockerCode::SpecUnclear)));
    let task = sim.task(1);
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.params["questions"][0], "Which list is meant?");
    assert_eq!(blocker.params["step_id"], "S1");
    assert_eq!(primary_action(&task).unwrap().kind, BlockerAction::EditSpec);
    assert!(
        sim.world.reviews().is_empty(),
        "nothing is posted for an unclear spec"
    );

    retry(&sim);
    assert!(sim.run_until(STEPS, awaiting_merge));
    let task = sim.task(1);
    assert_eq!(task.reviews.len(), 2);
    assert_eq!(sim.world.reviewer_launches().len(), 2);
    let sessions = sim.sessions(1);
    let reviewers: Vec<_> = sessions.iter().skip(1).collect();
    assert!(
        reviewers
            .iter()
            .all(|s| s.resumed_from_session_id.is_none())
    );
}

#[test]
fn a_workflow_without_a_review_stage_never_asks_for_one() {
    let mut sim = Sim::new(SimConfig {
        workflow: without_review(),
        ..SimConfig::default()
    });
    sim.approve();
    sim.cfg.operator.merge = true;
    assert!(sim.run_to_shipped(STEPS));
    assert!(sim.world.reviewer_launches().is_empty());
    assert!(sim.task(1).reviews.is_empty());
    assert!(runs(&sim).iter().all(|(stage, _, _)| stage != "review"));
    assert!(
        runs(&sim)
            .iter()
            .all(|(_, state, _)| state != &StageRunState::Blocked.as_str().to_string())
    );
}

#[test]
fn a_task_taken_over_while_its_review_waits_comes_back_through_ci_and_a_new_review() {
    let mut sim = Sim::new(hand_merge());
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Review, StagePhase::AwaitingReview)
    }));
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let id = sim.task(1).item.id;
    engine
        .takeover_with(id, "I will look first", &cause, None)
        .unwrap();
    assert_eq!(sim.task(1).item.state, WorkItemState::TakenOver);
    assert!(sim.world.reviewer_launches().is_empty());

    sim.block_on(engine.return_with(id, None, "back to the loop", &cause, None))
        .unwrap();
    drop(engine);
    assert!(sim.run_until(STEPS, awaiting_merge));
    let task = sim.task(1);
    assert!(task.approved_at(&head(&task)));
    assert_eq!(sim.world.reviewer_launches().len(), 1);
}
