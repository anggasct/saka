#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{BlockerCode, Stage, StagePhase, WorkItemState};
use saka_testkit::{
    ChangeSpec, FileEdit, HOME_REPO, Layout, Operator, ReviewReply, SHOP_REPO, Sim, SimConfig,
    repo_entry, saka_toml, without_fix,
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
        workflow: without_fix(),
        ..SimConfig::default()
    }
}

fn edit(path: &str) -> FileEdit {
    (path.to_string(), Some(format!("{path}\n")))
}

fn own_task_sim(replies: Vec<ReviewReply>) -> Sim {
    let mut sim = Sim::new(hand_merge());
    sim.world.script_edits(
        (1..=4)
            .map(|n| vec![edit(&format!("src/c{n}.rs"))])
            .collect(),
    );
    sim.world.script_reviews(replies);
    sim.world.set_write_access("bob", true);
    sim.approve();
    sim
}

fn own_pr(sim: &Sim) -> u64 {
    sim.world.prs()[0].number
}

fn comment(sim: &Sim, repo: &str, number: u64, author: &str, body: &str) {
    sim.clock.advance(1_000);
    sim.world.comment_on_change(repo, number, author, body);
}

fn awaiting_merge(sim: &Sim) -> bool {
    sim.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
}

fn done(sim: &Sim) -> bool {
    sim.task(1).item.stage == Some(Stage::Sync) || sim.task(1).item.state == WorkItemState::Done
}

fn requests(sim: &Sim) -> Vec<(String, String)> {
    sim.events_of("delivery.merge.requested")
        .iter()
        .map(|e| {
            (
                e.payload["head_sha"].as_str().unwrap().to_string(),
                e.payload["reason"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[test]
fn a_writer_asks_for_the_merge_of_a_ready_task_of_saka_in_a_comment_once() {
    let mut sim = own_task_sim(vec![ReviewReply::approve()]);
    assert!(sim.run_until(STEPS, awaiting_merge));
    let head = sim.task(1).change_request.unwrap().head_sha;
    let number = own_pr(&sim);
    comment(&sim, HOME_REPO, number, "bob", "Ship it.\n/saka merge\n");
    assert!(sim.run_until(STEPS, done));

    assert_eq!(
        requests(&sim),
        [(head.clone(), "comment by bob".to_string())]
    );
    let pr = sim.world.prs().into_iter().next().unwrap();
    assert_eq!(pr.merged_head.as_deref(), Some(head.as_str()));
    assert!(sim.world.comments().is_empty());
    let engine = sim.engine();
    let project = sim.project_id;
    let repo = sim.task(1).item.repo_id;
    sim.block_on(engine.scan_changes(project, repo)).unwrap();
    assert_eq!(requests(&sim).len(), 1, "the same comment is read once");
}

#[test]
fn a_comment_of_someone_without_write_access_changes_nothing_and_gets_no_reply() {
    let mut sim = own_task_sim(vec![ReviewReply::approve()]);
    assert!(sim.run_until(STEPS, awaiting_merge));
    let number = own_pr(&sim);
    let version = sim.task(1).item.version;
    comment(&sim, HOME_REPO, number, "mallory", "/saka merge");
    sim.run_until(600, |_| false);

    assert!(awaiting_merge(&sim));
    assert_eq!(sim.task(1).item.version, version);
    assert!(requests(&sim).is_empty());
    assert!(sim.world.comments().is_empty());
    assert!(sim.world.merge_calls().is_empty());
}

#[test]
fn a_refused_command_leaves_the_task_alone_and_is_answered_once_per_head() {
    let mut sim = own_task_sim(vec![ReviewReply::request_changes("Cover the empty list.")]);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .open_blocker()
            .is_some_and(|b| b.code == BlockerCode::VerdictFailed)
    }));
    let before = sim.task(1);
    let number = own_pr(&sim);
    comment(&sim, HOME_REPO, number, "bob", "/saka merge");
    assert!(sim.run_until(STEPS, |s| s.world.comments().len() == 1));

    let comments = sim.world.comments();
    assert_eq!(comments[0].number, number);
    assert!(comments[0].body.contains("(merge_not_ready)"));
    assert_eq!(sim.task(1).item.version, before.item.version);
    assert!(requests(&sim).is_empty());

    comment(&sim, HOME_REPO, number, "bob", "/saka merge\n/saka merge");
    sim.run_until(1_500, |_| false);
    assert_eq!(sim.world.comments().len(), 1, "one reply per head and code");
    assert_eq!(sim.task(1).item.version, before.item.version);
}

#[test]
fn a_writer_asks_for_the_merge_of_an_approved_outside_change_in_a_comment() {
    let text = format!(
        "{}\n[workflow]\npreset = \"review_only\"\n",
        saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")])
    );
    let layout = Layout {
        saka_toml: text,
        ..Layout::default()
    };
    let mut cfg = SimConfig::default();
    cfg.operator.merge = false;
    let mut sim = Sim::multi_repo(cfg, &layout);
    sim.world.script_reviews(vec![ReviewReply::approve()]);
    sim.world.set_write_access("bob", true);
    let number = sim.world.open_external_change(
        SHOP_REPO,
        ChangeSpec::new("Add retries", "alice", "feature/retries"),
    );
    let head = sim.world.branches_of(SHOP_REPO)["feature/retries"].clone();
    assert!(sim.run_until(STEPS, |s| {
        s.externals()
            .iter()
            .any(|t| t.is_at(Stage::Merge, StagePhase::AwaitingMerge))
    }));
    comment(&sim, SHOP_REPO, number, "bob", "/saka merge");
    assert!(sim.run_until(STEPS, |s| {
        s.externals()
            .iter()
            .all(|t| t.item.state == WorkItemState::Done)
    }));

    assert_eq!(
        requests(&sim),
        [(head.clone(), "comment by bob".to_string())]
    );
    let pr = sim
        .world
        .prs()
        .into_iter()
        .find(|p| p.number == number)
        .unwrap();
    assert_eq!(pr.merged_head.as_deref(), Some(head.as_str()));
}
