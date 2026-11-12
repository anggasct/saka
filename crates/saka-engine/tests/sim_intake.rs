#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::model::Task;
use saka_core::vocab::{
    ExternalCause, Stage, StagePhase, StageRunState, Trigger, WorkItemOrigin, WorkItemState,
};
use saka_ports::Clock;
use saka_store::{TaskSet, read};
use saka_testkit::{ChangeSpec, Layout, SHOP_REPO, Sim, SimConfig, repo_entry, saka_toml};

const STEPS: usize = 8_000;
const POLL_MS: i64 = 15_000;

const REVIEW_ONLY: &str = "[workflow]\npreset = \"review_only\"\n";
const MANUAL_ENTRY: &str =
    "[workflow]\npreset = \"review_only\"\n\n[workflow.change_request]\ntrigger = \"manual\"\n";
const MANUAL_REVIEW: &str =
    "[workflow]\npreset = \"review_only\"\n\n[workflow.stages]\nreview = \"manual\"\n";
const BOTH_ENTRIES: &str = "[workflow]\npreset = \"custom\"\nspec = true\n\n[workflow.change_request]\n\n[workflow.stages]\ndev = \"auto\"\nreview = \"auto\"\nmerge = \"manual\"\n";

fn layout(workflow: &str) -> Layout {
    let mut text = saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]);
    text.push('\n');
    text.push_str(workflow);
    Layout {
        saka_toml: text,
        ..Layout::default()
    }
}

fn sim(workflow: &str) -> Sim {
    let cfg = SimConfig {
        max_active_tasks: 0,
        ..SimConfig::default()
    };
    Sim::multi_repo(cfg, &layout(workflow))
}

fn sim_with_slots(workflow: &str) -> Sim {
    Sim::multi_repo(SimConfig::default(), &layout(workflow))
}

fn externals(sim: &Sim) -> Vec<Task> {
    let engine = sim.engine();
    engine
        .store()
        .read(|c| {
            let ids: Vec<saka_core::ids::WorkItemId> = {
                let mut stmt =
                    c.prepare("SELECT id FROM work_item WHERE origin = 'external_change' ORDER BY created_at, id")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                rows.map(|r| {
                    saka_core::ids::WorkItemId(r.unwrap().parse::<uuid::Uuid>().unwrap())
                })
                .collect()
            };
            read::load_tasks(c, TaskSet::Ids(&ids), &saka_core::workflow::WorkflowPolicy::default())
        })
        .unwrap()
}

fn runs(sim: &Sim, task: &Task) -> Vec<(u32, String, String, Option<String>)> {
    let id = task.item.id.0.to_string();
    sim.engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT attempt, state, trigger, head_sha FROM stage_run WHERE work_item_id = ?1 ORDER BY attempt",
            )?;
            let rows = stmt.query_map([id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            Ok(rows.map(Result::unwrap).collect())
        })
        .unwrap()
}

fn wait(sim: &mut Sim, intervals: i64) {
    let until = sim.clock.now().0 + intervals * POLL_MS;
    assert!(sim.run_until(STEPS, |s| s.clock.now().0 >= until));
}

fn head(sim: &Sim, repo_number: u64) -> String {
    sim.world.branches_of(SHOP_REPO)[&sim.world.prs()[repo_number as usize - 1].head_branch].clone()
}

fn outside(title: &str, branch: &str) -> ChangeSpec {
    ChangeSpec::new(title, "alice", branch)
}

#[test]
fn an_outside_pull_request_becomes_one_task_waiting_for_the_reviewer() {
    let mut sim = sim(REVIEW_ONLY);
    let number = sim
        .world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    assert!(sim.run_until(STEPS, |s| !externals(s).is_empty()));
    wait(&mut sim, 6);

    let tasks = externals(&sim);
    assert_eq!(tasks.len(), 1, "scans repeat without duplicating the task");
    let task = &tasks[0];
    assert_eq!(task.item.origin, WorkItemOrigin::ExternalChange);
    assert_eq!(task.item.task_key, format!("shop#{number}"));
    assert_eq!(task.item.work, "Add retries");
    assert_eq!(task.item.branch, "feature/retries");
    assert!(task.is_at(Stage::Review, StagePhase::AwaitingReview));
    let external = task.external.as_ref().unwrap();
    assert_eq!(external.author, "alice");
    assert_eq!(external.cause, ExternalCause::Auto);
    assert_eq!(external.spec, None, "no spec behind this pull request");
    let cr = task.change_request.as_ref().unwrap();
    assert_eq!(
        (cr.number, cr.head_sha.as_str()),
        (number, head(&sim, number).as_str())
    );

    let run = task.stage_run.as_ref().unwrap();
    assert_eq!(run.stage, Stage::Review);
    assert_eq!(run.state, StageRunState::Pending);
    assert_eq!(run.trigger, Trigger::Auto);
    assert_eq!(run.head_sha.as_deref(), Some(cr.head_sha.as_str()));
    assert_eq!(runs(&sim, task).len(), 1);

    let stats = sim.stats();
    assert_eq!(
        (stats.prepares, stats.session_starts),
        (0, 0),
        "nothing runs yet"
    );
    assert_eq!(sim.events_of("delivery.external_change.accepted").len(), 1);
}

#[test]
fn filters_keep_out_what_does_not_match_and_paths_are_read_last() {
    let mut sim = sim(
        "[workflow]\npreset = \"review_only\"\n\n[workflow.change_request]\nlabels = [\"needs-review\"]\nauthors = [\"alice\"]\nbranches = [\"feature/**\"]\npaths = [\"services/api/**\"]\n",
    );
    let good = ChangeSpec::new("Good", "alice", "feature/good")
        .labels(&["needs-review"])
        .files(&["services/api/lib.rs"]);
    let wrong_label =
        ChangeSpec::new("No label", "alice", "feature/nolabel").files(&["services/api/a.rs"]);
    let wrong_author = ChangeSpec::new("Wrong author", "bob", "feature/author")
        .labels(&["needs-review"])
        .files(&["services/api/a.rs"]);
    let wrong_branch = ChangeSpec::new("Wrong branch", "alice", "hotfix/x")
        .labels(&["needs-review"])
        .files(&["services/api/a.rs"]);
    let wrong_path = ChangeSpec::new("Docs only", "alice", "feature/docs")
        .labels(&["needs-review"])
        .files(&["README.md"]);
    let fork = ChangeSpec::new("Fork", "alice", "feature/fork")
        .labels(&["needs-review"])
        .files(&["services/api/a.rs"])
        .fork();
    let draft = ChangeSpec::new("Draft", "alice", "feature/draft")
        .labels(&["needs-review"])
        .files(&["services/api/a.rs"])
        .draft();
    let good_number = sim.world.open_external_change(SHOP_REPO, good);
    for spec in [
        wrong_label,
        wrong_author,
        wrong_branch,
        wrong_path,
        fork,
        draft,
    ] {
        sim.world.open_external_change(SHOP_REPO, spec);
    }
    assert!(sim.run_until(STEPS, |s| !externals(s).is_empty()));
    wait(&mut sim, 4);
    let tasks = externals(&sim);
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        tasks[0].change_request.as_ref().unwrap().number,
        good_number
    );
    assert_eq!(
        sim.world.change_file_reads(),
        2,
        "files are read once for the pull request that passed and once for the docs-only one, never for the rest or again"
    );
}

#[test]
fn forks_and_drafts_come_in_only_when_the_entry_allows_them() {
    let mut sim = sim(
        "[workflow]\npreset = \"review_only\"\n\n[workflow.change_request]\ninclude_forks = true\ninclude_drafts = true\n",
    );
    sim.world
        .open_external_change(SHOP_REPO, outside("Fork", "f").fork());
    sim.world
        .open_external_change(SHOP_REPO, outside("Draft", "d").draft());
    assert!(sim.run_until(STEPS, |s| externals(s).len() == 2));
    let tasks = externals(&sim);
    assert!(tasks.iter().any(|t| t.external.as_ref().unwrap().from_fork));
    assert!(tasks.iter().any(|t| t.external.as_ref().unwrap().draft));
}

#[test]
fn a_pull_request_opened_by_saka_is_never_taken_as_outside_work() {
    let mut sim = sim_with_slots(BOTH_ENTRIES);
    let number = sim
        .world
        .open_external_change(SHOP_REPO, outside("Outside", "feature/outside"));
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        !s.world.prs().iter().all(|p| p.opened_with == "alice")
    }));
    wait(&mut sim, 6);
    let tasks = externals(&sim);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].change_request.as_ref().unwrap().number, number);
    assert!(
        sim.world.prs().len() >= 2,
        "saka opened its own pull request for step 1"
    );
}

#[test]
fn a_manual_entry_waits_for_a_command_from_someone_who_can_write() {
    let mut sim = sim(MANUAL_ENTRY);
    sim.world.set_write_access("bob", true);
    let number = sim
        .world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    wait(&mut sim, 4);
    assert!(externals(&sim).is_empty(), "no command, no task");

    sim.world
        .comment_on_change(SHOP_REPO, number, "eve", "/saka review");
    wait(&mut sim, 4);
    assert!(
        externals(&sim).is_empty(),
        "a stranger's command is ignored"
    );
    assert!(
        sim.events_of("delivery.external_change.accepted")
            .is_empty()
    );

    sim.world
        .comment_on_change(SHOP_REPO, number, "bob", "thanks\n/saka review please");
    assert!(sim.run_until(STEPS, |s| !externals(s).is_empty()));
    wait(&mut sim, 4);
    let tasks = externals(&sim);
    assert_eq!(tasks.len(), 1);
    let external = tasks[0].external.as_ref().unwrap();
    assert_eq!(external.cause, ExternalCause::Comment);
    assert_eq!(external.actor.as_deref(), Some("bob"));
    let run = tasks[0].stage_run.as_ref().unwrap();
    assert_eq!(run.trigger, Trigger::Manual);
    assert_eq!(run.reason.as_deref(), Some("comment by bob"));
    assert_eq!(runs(&sim, &tasks[0]).len(), 1);
}

#[test]
fn a_label_from_a_writer_is_a_command_and_a_quote_or_code_block_is_not() {
    let mut sim = sim(MANUAL_ENTRY);
    sim.world.set_write_access("carol", true);
    let quoted = sim
        .world
        .open_external_change(SHOP_REPO, outside("Quoted", "feature/quoted"));
    sim.world.comment_on_change(
        SHOP_REPO,
        quoted,
        "carol",
        "> /saka review\n```\n/saka review\n```",
    );
    let labeled = sim
        .world
        .open_external_change(SHOP_REPO, outside("Labeled", "feature/labeled"));
    sim.world
        .label_change(SHOP_REPO, labeled, "carol", "saka:review");
    assert!(sim.run_until(STEPS, |s| !externals(s).is_empty()));
    wait(&mut sim, 4);
    let tasks = externals(&sim);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].change_request.as_ref().unwrap().number, labeled);
    let run = tasks[0].stage_run.as_ref().unwrap();
    assert_eq!(run.reason.as_deref(), Some("label by carol"));
}

#[test]
fn an_old_command_never_asks_for_a_review_of_a_newer_head() {
    let mut sim = sim(MANUAL_REVIEW);
    sim.world.set_write_access("bob", true);
    let number = sim
        .world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    assert!(sim.run_until(STEPS, |s| !externals(s).is_empty()));
    let waiting = &externals(&sim)[0];
    assert!(waiting.is_at(Stage::Review, StagePhase::AwaitingTrigger));
    assert!(waiting.stage_run.is_none());

    sim.world
        .comment_on_change(SHOP_REPO, number, "bob", "/saka review");
    assert!(sim.run_until(STEPS, |s| {
        externals(s)[0].is_at(Stage::Review, StagePhase::AwaitingReview)
    }));
    let first_head = externals(&sim)[0]
        .change_request
        .as_ref()
        .unwrap()
        .head_sha
        .clone();

    sim.world.push_to_change(SHOP_REPO, number);
    assert!(sim.run_until(STEPS, |s| {
        externals(s)[0].is_at(Stage::Review, StagePhase::AwaitingTrigger)
    }));
    wait(&mut sim, 6);
    let task = &externals(&sim)[0];
    assert!(
        task.is_at(Stage::Review, StagePhase::AwaitingTrigger),
        "the earlier comment does not review the new head"
    );
    let history = runs(&sim, task);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].1, "cancelled");
    assert_eq!(history[0].3.as_deref(), Some(first_head.as_str()));

    sim.world
        .comment_on_change(SHOP_REPO, number, "bob", "/saka review");
    assert!(sim.run_until(STEPS, |s| {
        externals(s)[0].is_at(Stage::Review, StagePhase::AwaitingReview)
    }));
    let again = runs(&sim, &externals(&sim)[0]);
    assert_eq!(again.len(), 2);
    assert_eq!(again[1].1, "pending");
    assert_ne!(again[1].3.as_deref(), Some(first_head.as_str()));
}

#[test]
fn a_push_replaces_the_pending_request_and_merge_or_close_ends_the_task() {
    let mut sim = sim(REVIEW_ONLY);
    let number = sim
        .world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    let closed = sim
        .world
        .open_external_change(SHOP_REPO, outside("Other", "feature/other"));
    assert!(sim.run_until(STEPS, |s| externals(s).len() == 2));

    let moved = sim.world.push_to_change(SHOP_REPO, number);
    assert!(sim.run_until(STEPS, |s| {
        externals(s).iter().any(|t| {
            t.change_request
                .as_ref()
                .is_some_and(|c| c.head_sha == moved)
                && t.stage_run
                    .as_ref()
                    .is_some_and(|r| r.head_sha.as_deref() == Some(moved.as_str()))
        })
    }));
    let task = externals(&sim)
        .into_iter()
        .find(|t| t.change_request.as_ref().unwrap().number == number)
        .unwrap();
    let history = runs(&sim, &task);
    assert_eq!(
        history
            .iter()
            .map(|r| (r.0, r.1.as_str()))
            .collect::<Vec<_>>(),
        [(1, "cancelled"), (2, "pending")]
    );
    assert_eq!(task.item.stage_phase, Some(StagePhase::AwaitingReview));

    sim.world.merge_outside(number);
    sim.world.close_outside(closed);
    assert!(sim.run_until(STEPS, |s| {
        externals(s).iter().all(|t| t.item.state.is_terminal())
    }));
    let tasks = externals(&sim);
    let merged = tasks
        .iter()
        .find(|t| t.change_request.as_ref().unwrap().number == number)
        .unwrap();
    assert_eq!(merged.item.state, WorkItemState::Done);
    assert!(runs(&sim, merged).iter().all(|r| r.1 != "pending"));
    let shut = tasks
        .iter()
        .find(|t| t.change_request.as_ref().unwrap().number == closed)
        .unwrap();
    assert_eq!(shut.item.state, WorkItemState::Cancelled);
    assert!(runs(&sim, shut).iter().all(|r| r.1 != "pending"));

    let before = sim.events_of("delivery.external_change.accepted").len();
    wait(&mut sim, 6);
    assert_eq!(
        sim.events_of("delivery.external_change.accepted").len(),
        before
    );
    assert_eq!(
        sim.engine()
            .store()
            .job_counts()
            .unwrap()
            .iter()
            .filter(|(k, s, _)| k == "scm.poll_change_request" && s == "queued")
            .count(),
        0,
        "finished tasks are not polled"
    );
}

#[test]
fn a_pull_request_that_names_a_registered_spec_step_is_linked_to_it() {
    let mut sim = sim(REVIEW_ONLY);
    let n = sim.world.open_external_change(
        SHOP_REPO,
        outside("Implements tasks-done/s2", "feature/s2").body("Closes tasks-done/s9 too"),
    );
    let other = sim.world.open_external_change(
        SHOP_REPO,
        outside("Unrelated", "feature/other").body("See nothing/s1"),
    );
    assert!(sim.run_until(STEPS, |s| externals(s).len() == 2));
    for t in externals(&sim) {
        let number = t.change_request.as_ref().unwrap().number;
        let link = t.external.as_ref().unwrap().spec.clone();
        if number == n {
            let link = link.unwrap();
            assert_eq!(link.spec_id, sim.spec_id);
            assert_eq!(link.step_id.as_deref(), Some("S2"));
        } else {
            assert_eq!(number, other);
            assert_eq!(link, None);
        }
    }
}

#[test]
fn waiting_reviews_beside_a_spec_do_not_stop_it_from_shipping() {
    let cfg = SimConfig {
        max_active_tasks: 1,
        ..SimConfig::default()
    };
    let mut sim = Sim::multi_repo(cfg, &layout(BOTH_ENTRIES));
    for n in 0..6 {
        sim.world.open_external_change(
            SHOP_REPO,
            outside(&format!("Change {n}"), &format!("feature/{n}")),
        );
    }
    assert!(sim.run_until(STEPS, |s| externals(s).len() == 6));
    sim.approve();
    assert!(
        sim.run_to_shipped(STEPS),
        "the spec ships beside six waiting reviews"
    );
    assert!(
        externals(&sim)
            .iter()
            .all(|t| t.item.state == WorkItemState::InStage)
    );
}

#[test]
fn a_restart_between_scans_does_not_repeat_a_task_or_a_request() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    assert!(sim.run_until(STEPS, |s| !externals(s).is_empty()));
    sim.restart();
    wait(&mut sim, 3);
    sim.crash_mid_job();
    wait(&mut sim, 3);
    let tasks = externals(&sim);
    assert_eq!(tasks.len(), 1);
    assert_eq!(runs(&sim, &tasks[0]).len(), 1);
}

#[test]
fn an_operator_cancel_ends_the_task_for_good() {
    let mut sim = sim(REVIEW_ONLY);
    sim.world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    assert!(sim.run_until(STEPS, |s| !externals(s).is_empty()));
    let id = externals(&sim)[0].item.id;
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine.cancel(id, "not needed", &cause).unwrap();
    wait(&mut sim, 6);
    let tasks = externals(&sim);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].item.state, WorkItemState::Cancelled);
    assert!(runs(&sim, &tasks[0]).iter().all(|r| r.1 != "pending"));
}

fn deliver(sim: &Sim, payload: &serde_json::Value) -> saka_engine::webhooks::AcceptedWebhook {
    let body = serde_json::to_vec(payload).unwrap();
    let headers = vec![(
        saka_testkit::FAKE_SIGNATURE_HEADER.to_string(),
        saka_testkit::fake_signature(b"0123456789abcdef0123456789abcdef", &body),
    )];
    let engine = sim.engine();
    let delivery = engine.parse_webhook("github", &headers, &body).unwrap();
    engine.accept_webhook("github", &delivery).unwrap()
}

fn scan_keys(sim: &Sim) -> Vec<String> {
    sim.engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT idempotency_key FROM job WHERE kind = 'scm.scan_changes' ORDER BY idempotency_key",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.map(Result::unwrap).collect())
        })
        .unwrap()
}

#[test]
fn an_intake_delivery_schedules_one_scan_for_a_repo_with_the_entry_and_nothing_otherwise() {
    use saka_engine::webhooks::WebhookOutcome;
    let sim = sim(REVIEW_ONLY);
    sim.world
        .set_webhook_secret(b"0123456789abcdef0123456789abcdef");
    let payload = serde_json::json!({
        "delivery_id": "d-1", "event": "pull_request", "repo": SHOP_REPO, "targets": [{ "intake": true }],
    });
    let first = deliver(&sim, &payload);
    assert_eq!(
        (first.outcome, first.change_requests),
        (WebhookOutcome::Refreshed, 1)
    );
    assert_eq!(
        deliver(&sim, &payload).outcome,
        WebhookOutcome::Duplicate,
        "a delivery counts once"
    );
    let hooked: Vec<_> = scan_keys(&sim)
        .into_iter()
        .filter(|k| k.contains(":hook:"))
        .collect();
    assert_eq!(hooked.len(), 1, "{hooked:?}");

    let unknown = serde_json::json!({
        "delivery_id": "d-2", "event": "pull_request", "repo": "acme/unknown", "targets": [{ "intake": true }],
    });
    assert_eq!(deliver(&sim, &unknown).outcome, WebhookOutcome::Ignored);
    let plain = serde_json::json!({ "delivery_id": "d-3", "event": "ping", "repo": SHOP_REPO });
    assert_eq!(deliver(&sim, &plain).outcome, WebhookOutcome::Ignored);

    let full = self::sim("");
    full.world
        .set_webhook_secret(b"0123456789abcdef0123456789abcdef");
    let ignored = deliver(
        &full,
        &serde_json::json!({
            "delivery_id": "d-4", "event": "pull_request", "repo": SHOP_REPO, "targets": [{ "intake": true }],
        }),
    );
    assert_eq!(
        ignored.outcome,
        WebhookOutcome::Ignored,
        "a project without the entry scans nothing"
    );
}

#[test]
fn a_scan_job_without_the_entry_reads_nothing() {
    let mut sim = sim("");
    sim.world
        .open_external_change(SHOP_REPO, outside("Add retries", "feature/retries"));
    wait(&mut sim, 4);
    assert!(externals(&sim).is_empty());
    assert_eq!(sim.world.change_listings(), 0);
    assert!(scan_keys(&sim).is_empty());
}
