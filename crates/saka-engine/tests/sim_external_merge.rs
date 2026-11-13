#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::model::Task;
use saka_core::vocab::{
    BlockerAction, BlockerCode, BlockerResolution, CheckConclusion, GateSource, Stage, StagePhase,
    WorkItemState,
};
use saka_ports::{ScmCapabilities, UpdateOutcome};
use saka_testkit::{
    ChangeSpec, CiScript, FAKE_CAPABILITIES, Layout, ReviewReply, SHOP_REPO, Sim, SimConfig,
    repo_entry, saka_toml,
};

const STEPS: usize = 12_000;
const REVIEW_ONLY: &str = "[workflow]\npreset = \"review_only\"\n";
const WITH_FORKS: &str =
    "[workflow]\npreset = \"review_only\"\n\n[workflow.change_request]\ninclude_forks = true\n";
const BRANCH: &str = "feature/retries";

fn sim(workflow: &str) -> Sim {
    let mut text = saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]);
    text.push('\n');
    text.push_str(workflow);
    let layout = Layout {
        saka_toml: text,
        ..Layout::default()
    };
    let mut cfg = SimConfig::default();
    cfg.operator.merge = false;
    Sim::multi_repo(cfg, &layout)
}

fn only(sim: &Sim) -> Task {
    let tasks = sim.externals();
    assert_eq!(tasks.len(), 1, "one outside task");
    tasks.into_iter().next().unwrap()
}

fn open(sim: &Sim, spec: ChangeSpec) -> u64 {
    sim.world.open_external_change(SHOP_REPO, spec)
}

fn change() -> ChangeSpec {
    ChangeSpec::new("Add retries", "alice", BRANCH)
}

fn awaiting_merge(sim: &mut Sim) {
    assert!(sim.run_until(STEPS, |s| {
        s.externals()
            .iter()
            .any(|t| t.is_at(Stage::Merge, StagePhase::AwaitingMerge))
    }));
}

fn ask_for_the_merge(sim: &Sim) {
    let task = only(sim);
    let engine = sim.engine();
    engine
        .request_merge(
            task.item.id,
            task.item.version,
            Some("comment by bob"),
            &engine.cause("human:operator:local"),
        )
        .unwrap();
}

fn blocked_on(sim: &mut Sim, code: BlockerCode) {
    assert!(sim.run_until(STEPS, |s| {
        s.externals()
            .iter()
            .any(|t| t.open_blocker().is_some_and(|b| b.code == code))
    }));
}

fn run_update(sim: &Sim) {
    let task = only(sim);
    let blocker = task.open_blocker().unwrap().id;
    let engine = sim.engine();
    engine
        .resolve_blocker_with(
            task.item.id,
            blocker,
            Some(BlockerAction::UpdateBranch),
            "operator",
            &engine.cause("human:operator:local"),
            None,
        )
        .unwrap();
}

fn branch_head(sim: &Sim) -> String {
    sim.world.branches_of(SHOP_REPO)[BRANCH].clone()
}

#[test]
fn an_approved_outside_change_merges_on_request_at_its_exact_head_and_keeps_its_branch() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world.script_reviews(vec![ReviewReply::approve()]);
    let number = open(&sim, change());
    let head = branch_head(&sim);
    awaiting_merge(&mut sim);
    assert!(
        sim.world.merge_calls().is_empty(),
        "approval alone never merges"
    );

    ask_for_the_merge(&sim);
    assert!(sim.run_until(STEPS, |s| {
        s.externals()
            .iter()
            .all(|t| t.item.state == WorkItemState::Done)
    }));

    let pr = sim
        .world
        .prs()
        .into_iter()
        .find(|p| p.number == number)
        .unwrap();
    assert_eq!(pr.merged_head.as_deref(), Some(head.as_str()));
    assert!(sim.world.branches_of(SHOP_REPO).contains_key(BRANCH));
    assert_eq!(sim.stats().branches_deleted, 0);
    assert!(sim.world.update_calls().is_empty());
    assert_eq!(sim.events_of("delivery.merge.requested").len(), 1);
}

#[test]
fn a_change_behind_its_base_is_updated_with_a_merge_commit_reviewed_again_and_merged_on_its_own() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world
        .script_reviews(vec![ReviewReply::approve(), ReviewReply::approve()]);
    let number = open(&sim, change());
    let old_head = branch_head(&sim);
    awaiting_merge(&mut sim);
    let tip = sim.world.push_to_base_of(SHOP_REPO);

    ask_for_the_merge(&sim);
    blocked_on(&mut sim, BlockerCode::ChangeBehindBase);
    let task = only(&sim);
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.params["updatable"], true);
    assert_eq!(blocker.params["head_sha"], old_head.as_str());
    assert_eq!(blocker.params["base_sha"], tip.as_str());
    assert_eq!(
        saka_core::blocker::primary_action(&task).unwrap().kind,
        BlockerAction::UpdateBranch
    );
    assert!(!task.is_queued());
    assert!(sim.world.update_calls().is_empty());

    run_update(&sim);
    assert!(sim.run_until(STEPS, |s| {
        s.externals()
            .iter()
            .all(|t| t.item.state == WorkItemState::Done)
    }));

    assert_eq!(sim.world.update_calls(), [(number, old_head.clone())]);
    let new_head = branch_head(&sim);
    assert_ne!(new_head, old_head);
    assert!(sim.world.is_ancestor(&old_head, &new_head));
    assert!(sim.world.is_ancestor(&tip, &new_head));
    let pr = sim
        .world
        .prs()
        .into_iter()
        .find(|p| p.number == number)
        .unwrap();
    assert_eq!(pr.merged_head.as_deref(), Some(new_head.as_str()));
    assert_eq!(sim.world.reviews().len(), 2, "one review per head");
    assert_eq!(
        sim.events_of("delivery.change_request.update_requested")
            .len(),
        1
    );
    let requests = sim.events_of("delivery.merge.requested");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].payload["reason"], "branch updated");
    let done = only_done(&sim);
    let blockers: Vec<_> = sim
        .engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare("SELECT resolution FROM blocker WHERE work_item_id = ?1")?;
            let rows = stmt.query_map([done.item.id.0.to_string()], |r| r.get::<_, String>(0))?;
            Ok(rows.map(Result::unwrap).collect())
        })
        .unwrap();
    assert_eq!(blockers, [BlockerResolution::Retried.as_str()]);
}

fn only_done(sim: &Sim) -> Task {
    sim.externals().into_iter().next().unwrap()
}

#[test]
fn an_update_overtaken_by_an_author_push_never_carries_the_merge_request_to_the_new_head() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world
        .script_reviews(vec![ReviewReply::approve(), ReviewReply::approve()]);
    let number = open(&sim, change());
    awaiting_merge(&mut sim);
    sim.world.push_to_base_of(SHOP_REPO);
    ask_for_the_merge(&sim);
    blocked_on(&mut sim, BlockerCode::ChangeBehindBase);

    run_update(&sim);
    let pushed = sim.world.push_to_change(SHOP_REPO, number);
    assert!(sim.run_until(STEPS, |s| {
        s.world.reviews().len() == 2 && only(s).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));

    let task = only(&sim);
    let cr = task.change_request.as_ref().unwrap();
    assert_eq!(cr.head_sha, pushed);
    assert!(cr.update_head_sha.is_none());
    assert!(!task.is_queued());
    assert!(task.approved_at(&pushed));
    assert!(sim.world.merge_calls().is_empty());
    assert_eq!(sim.events_of("delivery.merge.requested").len(), 1);
}

#[test]
fn a_conflicting_update_blocks_with_the_conflict_and_asks_the_author_once() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world.script_reviews(vec![ReviewReply::approve()]);
    let number = open(&sim, change());
    awaiting_merge(&mut sim);
    sim.world.push_to_base_of(SHOP_REPO);
    sim.world
        .script_updates(number, vec![UpdateOutcome::Conflict]);
    ask_for_the_merge(&sim);
    blocked_on(&mut sim, BlockerCode::ChangeBehindBase);

    run_update(&sim);
    blocked_on(&mut sim, BlockerCode::MergeConflict);
    assert!(sim.run_until(STEPS, |s| s.world.comments().len() == 1));
    let blocker = only(&sim).open_blocker().unwrap().clone();
    assert_eq!(blocker.params["reason"], "update_conflict");
    let comments = sim.world.comments();
    assert_eq!(comments[0].number, number);
    assert!(comments[0].body.contains("conflict"));
    assert!(sim.world.merge_calls().is_empty());
    assert_eq!(sim.world.update_calls().len(), 1);
}

#[test]
fn a_fork_behind_its_base_asks_the_author_and_follows_the_author_push() {
    let mut sim = sim(WITH_FORKS);
    sim.world
        .script_reviews(vec![ReviewReply::approve(), ReviewReply::approve()]);
    let number = open(&sim, change().fork());
    awaiting_merge(&mut sim);
    sim.world.push_to_base_of(SHOP_REPO);
    ask_for_the_merge(&sim);
    blocked_on(&mut sim, BlockerCode::ChangeBehindBase);

    let task = only(&sim);
    assert_eq!(task.open_blocker().unwrap().params["updatable"], false);
    assert_eq!(
        saka_core::blocker::primary_action(&task).unwrap().kind,
        BlockerAction::Retry
    );
    assert!(sim.run_until(STEPS, |s| s.world.comments().len() == 1));
    assert!(sim.world.comments()[0].body.contains("update the branch"));
    assert!(sim.world.update_calls().is_empty());

    let pushed = sim.world.push_to_change(SHOP_REPO, number);
    assert!(sim.run_until(STEPS, |s| {
        s.externals().iter().any(|t| {
            t.open_blocker().is_none()
                && t.change_request
                    .as_ref()
                    .is_some_and(|c| c.head_sha == pushed)
        })
    }));
    let resolutions: Vec<String> = sim
        .engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare("SELECT resolution FROM blocker")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.map(Result::unwrap).collect())
        })
        .unwrap();
    assert_eq!(resolutions, [BlockerResolution::Auto.as_str()]);
    assert!(sim.world.merge_calls().is_empty());
    assert!(!only(&sim).is_queued());
    assert_eq!(
        sim.world.comments().len(),
        1,
        "one comment, not one per poll"
    );
}

#[test]
fn a_provider_without_branch_updates_asks_the_author_instead() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world.set_capabilities(ScmCapabilities {
        update_branch: false,
        ..FAKE_CAPABILITIES
    });
    sim.world.script_reviews(vec![ReviewReply::approve()]);
    open(&sim, change());
    awaiting_merge(&mut sim);
    sim.world.push_to_base_of(SHOP_REPO);
    ask_for_the_merge(&sim);
    blocked_on(&mut sim, BlockerCode::ChangeBehindBase);

    let task = only(&sim);
    assert_eq!(task.open_blocker().unwrap().params["updatable"], false);
    assert_eq!(
        saka_core::blocker::primary_action(&task).unwrap().kind,
        BlockerAction::Retry
    );
    assert!(sim.run_until(STEPS, |s| s.world.comments().len() == 1));
    assert!(sim.world.update_calls().is_empty());
}

fn scm_gate_checks(task: &Task) -> Vec<(String, Option<CheckConclusion>)> {
    let head = &task.change_request.as_ref().unwrap().head_sha;
    task.gate_for(GateSource::Scm, head)
        .map(|g| {
            g.checks
                .iter()
                .map(|c| (c.name.clone(), c.conclusion))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn an_approved_outside_change_with_failing_ci_blocks_as_ci_red_instead_of_awaiting_the_merge() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world.set_commit_ci(CiScript::Fail(vec!["test".into()]));
    sim.world.script_reviews(vec![ReviewReply::approve()]);
    open(&sim, change());
    assert!(sim.run_until(STEPS, |s| {
        s.externals().iter().any(|t| {
            t.open_blocker()
                .is_some_and(|b| b.code == BlockerCode::CiRed)
                || t.is_at(Stage::Merge, StagePhase::AwaitingMerge)
        })
    }));
    let task = only(&sim);
    assert!(!task.is_at(Stage::Merge, StagePhase::AwaitingMerge));
    assert_eq!(task.open_blocker().unwrap().code, BlockerCode::CiRed);
    assert_eq!(
        scm_gate_checks(&task),
        [("test".to_string(), Some(CheckConclusion::Failure))]
    );
    assert!(sim.world.merge_calls().is_empty());
}

#[test]
fn an_approved_outside_change_with_passing_ci_awaits_the_merge_with_its_checks_as_evidence() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world.set_commit_ci(CiScript::Pass);
    sim.world.script_reviews(vec![ReviewReply::approve()]);
    open(&sim, change());
    awaiting_merge(&mut sim);
    assert_eq!(
        scm_gate_checks(&only(&sim)),
        [("test".to_string(), Some(CheckConclusion::Success))]
    );
}

#[test]
fn an_outside_change_in_review_reads_no_ci_checks() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world.set_commit_ci(CiScript::Pass);
    sim.world
        .script_reviews(vec![ReviewReply::request_changes("handle the timeout")]);
    open(&sim, change());
    assert!(sim.run_until(STEPS, |s| {
        s.externals().iter().any(|t| !t.reviews.is_empty())
    }));
    let polls = |sim: &Sim, op: &str| sim.world.scm_calls().iter().filter(|c| c.op == op).count();
    let before = polls(&sim, "change_summary");
    for _ in 0..400 {
        sim.step();
    }
    let task = only(&sim);
    assert!(!task.is_at_stage(Stage::Merge));
    assert!(polls(&sim, "change_summary") > before);
    assert_eq!(polls(&sim, "commit_checks"), 0);
}
