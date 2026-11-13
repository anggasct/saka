#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{Stage, StagePhase};
use saka_testkit::{AgentScript, FileEdit, Operator, ReviewReply, Sim, SimConfig};

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

fn script(results: &[&str]) -> AgentScript {
    AgentScript {
        results: results.iter().map(|r| (*r).to_string()).collect(),
        ..AgentScript::default()
    }
}

fn awaiting_merge(sim: &Sim) -> bool {
    sim.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
}

fn body(sim: &Sim) -> String {
    sim.world.prs().first().unwrap().body.clone()
}

#[test]
fn the_pull_request_body_carries_the_redacted_summary_of_the_dev_session() {
    let token = format!("ghp_{}", "aB3d".repeat(9));
    let mut sim = Sim::new(hand_merge());
    fresh_commits(&sim);
    sim.world.set_script(script(&[&format!(
        "Committed.\n\n## Summary\nRetried the poller because it dropped events.\nRan `cargo test` with {token}: 12 passed.\n\n## Notes\nnot part of it"
    )]));
    sim.approve();
    assert!(sim.run_until(STEPS, awaiting_merge));

    let body = body(&sim);
    assert!(body.contains("<!-- saka-summary -->\n## Summary\n\nRetried the poller"));
    assert!(body.contains("`cargo test` with «secret:github_token»: 12 passed."));
    assert!(!body.contains(&token));
    assert!(!body.contains("not part of it"));
    assert!(body.contains("**Done when:**"));
    assert!(body.trim_end().ends_with("-->"));
}

#[test]
fn a_fix_round_replaces_the_summary_and_keeps_the_earlier_one_under_details() {
    let mut sim = Sim::new(hand_merge());
    fresh_commits(&sim);
    sim.world.set_script(script(&[
        "## Summary\nFirst version of the poller.",
        "## Summary\nCovered the empty list as the reviewer asked.",
    ]));
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::approve(),
    ]);
    sim.approve();
    assert!(sim.run_until(STEPS, awaiting_merge));

    let body = body(&sim);
    let (current, earlier) = body
        .split_once("<summary>Earlier rounds</summary>")
        .expect("the first summary moves under details");
    assert!(current.contains("## Summary\n\nCovered the empty list"));
    assert!(!current.contains("First version"));
    assert!(earlier.contains("### Initial work\n\nFirst version of the poller."));
    assert_eq!(body.matches("<!-- saka-summary -->").count(), 1);
    assert!(body.contains("<!-- saka-task:"));
}

#[test]
fn a_session_without_a_summary_leaves_the_replacement_line() {
    let mut sim = Sim::new(hand_merge());
    fresh_commits(&sim);
    sim.world.set_script(script(&["All done, no heading."]));
    sim.approve();
    assert!(sim.run_until(STEPS, awaiting_merge));
    assert!(
        body(&sim).contains("## Summary\n\nNo summary from the agent.\n<!-- /saka-summary -->")
    );
}

#[test]
fn a_body_edited_by_a_human_keeps_its_text_when_the_summary_is_refreshed() {
    let mut sim = Sim::new(hand_merge());
    fresh_commits(&sim);
    sim.world
        .set_script(script(&["## Summary\nFirst.", "## Summary\nSecond."]));
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Again."),
        ReviewReply::approve(),
    ]);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Fix, StagePhase::RunningSession)
    }));
    sim.world
        .edit_pr_body(1, |b| format!("Human note on top.\n\n{b}"));
    assert!(sim.run_until(STEPS, awaiting_merge));
    let body = body(&sim);
    assert!(body.starts_with("Human note on top."));
    assert!(body.contains("Second."));
}
