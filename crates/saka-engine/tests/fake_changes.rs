#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::time::Timestamp;
use saka_core::vocab::ChangeState;
use saka_core::vocab::ReviewEvent;
use saka_ports::{Clock, RepoRef, ReviewPost, Scm, SignalKind, UpdateOutcome};
use saka_testkit::{BOT_AUTHOR, ChangeSpec, HOME_REPO, Sim, SimConfig};

fn repo() -> RepoRef {
    RepoRef {
        repo_id: saka_core::ids::RepoId(uuid::Uuid::from_u128(1)),
        integration_id: "github".into(),
        full_name: HOME_REPO.into(),
        url: format!("https://github.com/{HOME_REPO}"),
    }
}

fn sim() -> Sim {
    Sim::new(SimConfig::default())
}

#[test]
fn the_fake_lists_open_changes_newest_first_with_the_neutral_fields() {
    let sim = sim();
    let first = sim.world.open_external_change(
        HOME_REPO,
        ChangeSpec::new("Add retries", "alice", "feature/retries")
            .labels(&["needs-review"])
            .body("details"),
    );
    sim.clock.advance(1_000);
    let second = sim.world.open_external_change(
        HOME_REPO,
        ChangeSpec::new("Fix typo", "mallory", "patch-1")
            .draft()
            .fork(),
    );
    let listed = sim
        .block_on(sim.world.list_open_changes(&repo(), 200))
        .unwrap();
    assert_eq!(
        listed.iter().map(|c| c.number).collect::<Vec<_>>(),
        [second, first]
    );
    let retries = &listed[1];
    assert_eq!(
        (
            retries.title.as_str(),
            retries.author.as_str(),
            retries.body.as_str()
        ),
        ("Add retries", "alice", "details")
    );
    assert_eq!(retries.labels, ["needs-review"]);
    assert!(!retries.draft && !retries.from_fork);
    assert!(listed[0].draft && listed[0].from_fork);
    assert_eq!(retries.state, ChangeState::Open);
    assert_eq!(retries.head_branch, "feature/retries");
    let capped = sim
        .block_on(sim.world.list_open_changes(&repo(), 1))
        .unwrap();
    assert_eq!(capped.len(), 1);
    assert_eq!(sim.world.change_listings(), 2);
}

#[test]
fn a_push_moves_the_head_and_a_merge_or_close_leaves_the_open_list() {
    let sim = sim();
    let n = sim
        .world
        .open_external_change(HOME_REPO, ChangeSpec::new("A", "alice", "a"));
    let before = sim.block_on(sim.world.change_summary(&repo(), n)).unwrap();
    let moved = sim.world.push_to_change(HOME_REPO, n);
    let after = sim.block_on(sim.world.change_summary(&repo(), n)).unwrap();
    assert_ne!(before.head_sha, after.head_sha);
    assert_eq!(after.head_sha, moved);
    sim.world.close_outside(n);
    assert!(
        sim.block_on(sim.world.list_open_changes(&repo(), 200))
            .unwrap()
            .is_empty()
    );
    let closed = sim.block_on(sim.world.change_summary(&repo(), n)).unwrap();
    assert_eq!(closed.state, ChangeState::Closed);
    let missing = sim
        .block_on(sim.world.change_summary(&repo(), 99))
        .unwrap_err();
    assert!(!missing.retryable);
}

#[test]
fn signals_come_oldest_first_with_their_actor_and_honor_since() {
    let sim = sim();
    let n = sim
        .world
        .open_external_change(HOME_REPO, ChangeSpec::new("A", "alice", "a"));
    sim.world
        .comment_on_change(HOME_REPO, n, "bob", "/saka review");
    sim.clock.advance(1_000);
    let mid = sim.clock.now();
    sim.world.label_change(HOME_REPO, n, "carol", "saka:review");
    let all = sim
        .block_on(sim.world.change_signals(&repo(), n, Timestamp(0)))
        .unwrap();
    assert_eq!(
        all.iter().map(|s| s.actor.as_str()).collect::<Vec<_>>(),
        ["bob", "carol"]
    );
    assert_eq!(
        all[1].kind,
        SignalKind::Label {
            name: "saka:review".into()
        }
    );
    let later = sim
        .block_on(sim.world.change_signals(&repo(), n, mid))
        .unwrap();
    assert_eq!(later.len(), 1);
    let summary = sim.block_on(sim.world.change_summary(&repo(), n)).unwrap();
    assert_eq!(summary.labels, ["saka:review"]);
}

#[test]
fn write_access_files_and_failures_are_scripted() {
    let sim = sim();
    let n = sim.world.open_external_change(
        HOME_REPO,
        ChangeSpec::new("A", "alice", "a").files(&["a.rs", "a.rs", "b.rs"]),
    );
    sim.world.set_write_access("bob", true);
    assert!(sim.block_on(sim.world.can_write(&repo(), "bob")).unwrap());
    assert!(!sim.block_on(sim.world.can_write(&repo(), "eve")).unwrap());
    sim.world.set_write_access("bob", false);
    assert!(!sim.block_on(sim.world.can_write(&repo(), "bob")).unwrap());
    let files = sim
        .block_on(sim.world.change_files(&repo(), n, 3000))
        .unwrap();
    assert_eq!(files, ["a.rs", "b.rs"]);
    assert_eq!(sim.world.change_file_reads(), 1);
    sim.world.fail_change_reads(true);
    assert!(
        sim.block_on(sim.world.list_open_changes(&repo(), 1))
            .unwrap_err()
            .retryable
    );
}

#[test]
fn a_change_opened_by_saka_is_listed_under_the_bot_author() {
    let mut sim = sim();
    sim.approve();
    assert!(sim.run_until(5_000, |s| !s.world.prs().is_empty()));
    let listed = sim
        .block_on(sim.world.list_open_changes(&repo(), 200))
        .unwrap();
    assert_eq!(listed[0].author, BOT_AUTHOR);
}

#[test]
fn the_fake_records_a_review_for_a_known_change_and_refuses_an_unknown_one() {
    let sim = sim();
    let n = sim
        .world
        .open_external_change(HOME_REPO, ChangeSpec::new("A", "alice", "a"));
    let post = |number: u64, event: ReviewEvent| ReviewPost {
        repo: repo(),
        number,
        head_sha: "a".repeat(40),
        event,
        body: "ok".into(),
    };
    let posted = sim
        .block_on(sim.world.post_review(&post(n, ReviewEvent::Approve)))
        .unwrap();
    assert!(posted.url.is_some());
    let missing = sim
        .block_on(sim.world.post_review(&post(n + 9, ReviewEvent::Comment)))
        .unwrap_err();
    assert_eq!(missing.code, "review_refused");
    assert_eq!(sim.world.reviews().len(), 1);
    sim.world.fail_reviews(1);
    assert!(
        sim.block_on(sim.world.post_review(&post(n, ReviewEvent::Comment)))
            .unwrap_err()
            .retryable
    );
    sim.world.refuse_reviews();
    assert_eq!(
        sim.block_on(sim.world.post_review(&post(n, ReviewEvent::Comment)))
            .unwrap_err()
            .code,
        "review_refused"
    );
}

#[test]
fn the_fake_updates_a_branch_with_a_merge_commit_of_the_old_head_and_the_base_tip() {
    let sim = sim();
    let number = sim.world.open_external_change(
        HOME_REPO,
        ChangeSpec::new("Add retries", "alice", "feature/retries"),
    );
    let old = sim.world.branches_of(HOME_REPO)["feature/retries"].clone();
    let tip = sim.world.push_to_base();
    assert!(!sim.world.is_ancestor(&tip, &old));

    let stale = sim
        .block_on(sim.world.update_branch(&repo(), number, &tip))
        .unwrap();
    assert_eq!(stale, UpdateOutcome::HeadChanged);
    assert_eq!(
        sim.world.branches_of(HOME_REPO)["feature/retries"].clone(),
        old
    );

    sim.world
        .script_updates(number, vec![UpdateOutcome::Conflict]);
    let conflict = sim
        .block_on(sim.world.update_branch(&repo(), number, &old))
        .unwrap();
    assert_eq!(conflict, UpdateOutcome::Conflict);
    assert_eq!(
        sim.world.branches_of(HOME_REPO)["feature/retries"].clone(),
        old
    );

    let accepted = sim
        .block_on(sim.world.update_branch(&repo(), number, &old))
        .unwrap();
    assert_eq!(accepted, UpdateOutcome::Accepted);
    let new = sim.world.branches_of(HOME_REPO)["feature/retries"].clone();
    assert!(sim.world.is_ancestor(&tip, &new) && sim.world.is_ancestor(&old, &new));
    assert_eq!(sim.world.update_calls().len(), 3);
}

#[test]
fn the_fake_records_comments_for_known_changes_only() {
    let sim = sim();
    let number = sim.world.open_external_change(
        HOME_REPO,
        ChangeSpec::new("Add retries", "alice", "feature/retries"),
    );
    sim.block_on(sim.world.post_comment(&repo(), number, "please update"))
        .unwrap();
    let missing = sim
        .block_on(sim.world.post_comment(&repo(), number + 1, "hello"))
        .unwrap_err();
    assert_eq!(missing.code, "comment_refused");
    let comments = sim.world.comments();
    assert_eq!(comments.len(), 1);
    assert_eq!(
        (comments[0].number, comments[0].body.as_str()),
        (number, "please update")
    );
}
