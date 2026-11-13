#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::autonomy::Ceilings;
use saka_core::event::Event;
use saka_core::ids::WorkItemId;
use saka_core::model::Task;
use saka_core::vocab::{
    Autonomy, BlockerCode, ChangeState, Stage, StagePhase, Trigger, WorkItemOrigin, WorkItemState,
};
use saka_engine::EngineError;
use saka_ports::Reverted;
use saka_store::read;
use saka_testkit::{FileEdit, Operator, Sim, SimConfig};

const STEPS: usize = 20_000;

fn config(level: Autonomy) -> SimConfig {
    let mut cfg = SimConfig {
        operator: Operator {
            merge: false,
            retry: vec![],
            ..Operator::default()
        },
        ..SimConfig::default()
    };
    cfg.loop_cfg.autonomy = Ceilings {
        default: level,
        repos: Vec::new(),
    };
    cfg.workflow.set_stage(Stage::Merge, Trigger::Auto);
    cfg
}

fn shipped(cfg: SimConfig) -> Sim {
    let mut sim = Sim::new(cfg);
    sim.world.script_edits(
        (1..=2)
            .map(|n| {
                let edit: FileEdit = (format!("src/change{n}.rs"), Some(format!("change {n}\n")));
                vec![edit]
            })
            .collect(),
    );
    sim.approve();
    if sim.cfg.loop_cfg.autonomy.default != Autonomy::Autonomous {
        sim.cfg.operator.merge = true;
    }
    assert!(sim.run_to_shipped(STEPS));
    sim.cfg.operator.merge = false;
    sim
}

fn load(sim: &Sim, id: WorkItemId) -> Task {
    sim.engine()
        .store()
        .read(|c| read::load_task(c, id, &sim.cfg.workflow_policy))
        .unwrap()
        .unwrap()
}

fn request(sim: &Sim, step: u32, reason: &str) -> Result<Vec<Event>, EngineError> {
    let origin = sim.task(step);
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine.revert_with(origin.item.id, origin.item.version, reason, &cause, None)
}

fn revert_of(sim: &Sim, step: u32) -> Task {
    let origin = sim.task(step).item.id;
    let link = sim
        .engine()
        .store()
        .read(|c| read::reverts_of(c, origin))
        .unwrap()
        .pop()
        .unwrap();
    load(sim, link.id)
}

fn tap(sim: &Sim, id: WorkItemId) {
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let task = load(sim, id);
    engine
        .request_merge(id, task.item.version, None, &cause)
        .unwrap();
}

#[test]
fn a_revert_request_creates_a_queued_revert_task_linked_to_the_origin() {
    let mut sim = shipped(config(Autonomy::Strict));
    let origin = sim.task(1);
    request(&sim, 1, "it broke the build").unwrap();

    let revert = revert_of(&sim, 1);
    assert_eq!(revert.item.origin, WorkItemOrigin::Revert);
    assert_eq!(revert.item.task_key, format!("{}-R1", origin.item.task_key));
    assert_eq!(revert.item.state, WorkItemState::Queued);
    assert_eq!(revert.revert.as_ref().unwrap().of, origin.item.id);
    assert_eq!(revert.revert.as_ref().unwrap().reason, "it broke the build");
    assert!(revert.item.spec_id.is_none() && revert.item.kind.is_none());
    assert_eq!(
        revert.item.branch,
        format!("saka/revert/{}-r1", origin.item.task_key)
    );
    assert_eq!(
        sim.events_of("delivery.work_item.revert_requested").len(),
        1
    );
    assert_eq!(sim.events_of("delivery.work_item.revert_created").len(), 1);
    let reverted_by = sim.task(1).reverted_by.unwrap();
    assert_eq!(reverted_by.id, revert.item.id);
    assert!(sim.run_until(STEPS, |s| {
        load(s, revert.item.id).item.state != WorkItemState::Queued
    }));
    assert!(
        sim.events_of("delivery.work_item.awaiting_start")
            .iter()
            .all(|e| e.stream_id != revert.item.id.0),
        "the operator's action is the approval, at any autonomy"
    );
}

#[test]
fn a_revert_in_an_autonomous_repo_runs_without_a_session_a_review_or_a_tap() {
    let mut sim = shipped(config(Autonomy::Autonomous));
    let sessions = sim.events_of("runtime.session.started").len();
    let merged_before = sim.stats().merges;
    request(&sim, 1, "pull it back").unwrap();
    let id = revert_of(&sim, 1).item.id;

    assert!(sim.run_until(STEPS, |s| {
        let t = load(s, id);
        t.item.stage == Some(Stage::Dev) && t.item.stage_phase.is_some()
            || t.item.state == WorkItemState::Done
    }));
    assert!(sim.run_until(STEPS, |s| load(s, id).item.state == WorkItemState::Done));

    let done = load(&sim, id);
    assert!(done.reviews.is_empty() && done.session.is_none());
    assert_eq!(
        sim.events_of("runtime.session.started").len(),
        sessions,
        "no agent session for a revert"
    );
    assert_eq!(sim.stats().merges, merged_before + 1);
    let pr = sim.world.prs().pop().unwrap();
    assert_eq!(pr.state, ChangeState::Merged);
    assert!(pr.title.starts_with("Revert \""), "{}", pr.title);
    assert!(!pr.body.contains("Closes"));
    assert!(pr.body.contains("Reason: pull it back"));
    assert!(
        pr.body
            .contains(&format!("saka-task: {}", done.item.task_key))
    );
    let requests = sim.world.reverts();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].message.starts_with(&pr.title));
    assert!(requests[0].message.contains("Reason: pull it back"));
    assert_eq!(
        sim.events_of("delivery.merge.requested")
            .iter()
            .filter(|e| e.payload["work_item_id"] == id.0.to_string()
                && e.payload["reason"] == "autonomy")
            .count(),
        1
    );
    assert_eq!(sim.task(1).reverted_by.unwrap().state, WorkItemState::Done);
    assert_eq!(sim.events_of("delivery.work_item.reverted").len(), 1);
}

#[test]
fn a_revert_in_a_standard_repo_waits_for_the_tap_before_merging() {
    let mut sim = shipped(config(Autonomy::Standard));
    request(&sim, 1, "revert it").unwrap();
    let id = revert_of(&sim, 1).item.id;
    assert!(sim.run_until(STEPS, |s| {
        load(s, id).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    let merges = sim.stats().merges;
    for _ in 0..300 {
        sim.step();
    }
    assert!(load(&sim, id).is_at(Stage::Merge, StagePhase::AwaitingMerge));
    assert_eq!(sim.stats().merges, merges);

    tap(&sim, id);
    assert!(sim.run_until(STEPS, |s| load(s, id).item.state == WorkItemState::Done));
}

#[test]
fn a_revert_task_is_refused_when_the_origin_cannot_be_reverted() {
    let mut sim = shipped(config(Autonomy::Autonomous));
    let rejected = |sim: &Sim, step: u32, reason: &str| -> String {
        match request(sim, step, reason) {
            Err(EngineError::Rejected(r)) => r.code().to_string(),
            other => panic!("{other:?}"),
        }
    };
    request(&sim, 1, "first").unwrap();
    assert_eq!(rejected(&sim, 1, "second"), "revert_in_progress");
    let id = revert_of(&sim, 1).item.id;
    assert!(sim.run_until(STEPS, |s| load(s, id).item.state == WorkItemState::Done));
    assert_eq!(rejected(&sim, 1, "third"), "already_reverted");

    let cause = sim.engine().cause("human:operator:local");
    let revert = load(&sim, id);
    let refused =
        sim.engine()
            .revert_with(id, revert.item.version, "revert the revert", &cause, None);
    assert!(
        matches!(&refused, Err(EngineError::Rejected(r)) if r.code() == "revert_not_allowed"),
        "{refused:?}"
    );
    for text in ["", "   ", &"x".repeat(501)] {
        let engine = sim.engine();
        let origin = sim.task(2);
        let r = engine.revert_with(origin.item.id, origin.item.version, text, &cause, None);
        assert!(
            matches!(
                r,
                Err(EngineError::Invalid {
                    field: "reason",
                    ..
                })
            ),
            "{r:?}"
        );
    }
    let origin = sim.task(2);
    let stale = sim.engine().revert_with(
        origin.item.id,
        origin.item.version - 1,
        "late",
        &cause,
        None,
    );
    assert!(matches!(&stale, Err(EngineError::Rejected(r)) if r.code() == "version_stale"));
    sim.cfg.operator.merge = true;
}

#[test]
fn a_task_that_never_merged_cannot_be_reverted() {
    let mut sim = Sim::new(config(Autonomy::Standard));
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    let r = request(&sim, 1, "too early");
    assert!(
        matches!(&r, Err(EngineError::Rejected(e)) if e.code() == "invalid_state"),
        "{r:?}"
    );
}

#[test]
fn a_reverted_task_that_is_done_stays_done_and_a_cancelled_revert_changes_nothing() {
    let mut sim = shipped(config(Autonomy::Strict));
    let before = sim.task(2);
    request(&sim, 2, "back out").unwrap();
    let id = revert_of(&sim, 2).item.id;
    assert!(sim.run_until(STEPS, |s| {
        load(s, id).item.state == WorkItemState::Admitted
            || load(s, id).item.state == WorkItemState::InStage
    }));
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine.cancel(id, "changed my mind", &cause).unwrap();
    assert!(sim.run_until(STEPS, |s| {
        load(s, id).item.state == WorkItemState::Cancelled
    }));
    let after = sim.task(2);
    assert_eq!(after.item.state, WorkItemState::Done);
    assert!(after.reverted_by.is_none());
    assert_eq!(
        after.item.done_at, before.item.done_at,
        "the origin keeps its own history"
    );
    assert!(sim.events_of("delivery.work_item.reverted").is_empty());
    request(&sim, 2, "again").unwrap();
    assert_eq!(
        revert_of(&sim, 2).item.task_key,
        format!("{}-R2", before.item.task_key)
    );
}

fn conflicting() -> Sim {
    let mut sim = shipped(config(Autonomy::Autonomous));
    sim.world.script_revert(Reverted::Conflict {
        paths: vec!["src/lib.rs".into()],
        count: 1,
    });
    request(&sim, 1, "conflicts").unwrap();
    let id = revert_of(&sim, 1).item.id;
    assert!(sim.run_until(STEPS, |s| load(s, id).item.state == WorkItemState::Blocked));
    sim
}

#[test]
fn a_conflicting_revert_blocks_with_the_paths_and_takeover_as_the_primary_action() {
    let sim = conflicting();
    let task = revert_of(&sim, 1);
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::RevertConflict);
    assert_eq!(blocker.params["reason"], "conflict");
    assert_eq!(blocker.params["paths"][0], "src/lib.rs");
    assert_eq!(blocker.params["count"], 1);
    assert_eq!(
        saka_core::blocker::primary_action(&task).unwrap().kind,
        saka_core::vocab::BlockerAction::Takeover
    );
    assert!(sim.events_of("delivery.work_item.reverted").is_empty());
}

#[test]
fn retrying_a_conflicting_revert_reverts_again_on_a_fresh_environment() {
    let mut sim = conflicting();
    let id = revert_of(&sim, 1).item.id;
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine.retry(id, "the base moved", &cause).unwrap();
    assert!(sim.run_until(STEPS, |s| load(s, id).item.state == WorkItemState::Done));
    assert_eq!(sim.world.reverts().len(), 2);
    assert_eq!(sim.world.prepared().len(), 4);
}

#[test]
fn an_empty_revert_offers_cancel_and_leaves_the_origin_alone() {
    let mut sim = shipped(config(Autonomy::Autonomous));
    sim.world.script_revert(Reverted::Empty);
    request(&sim, 1, "already gone").unwrap();
    let id = revert_of(&sim, 1).item.id;
    assert!(sim.run_until(STEPS, |s| load(s, id).item.state == WorkItemState::Blocked));
    let task = load(&sim, id);
    assert_eq!(task.open_blocker().unwrap().params["reason"], "empty");
    assert_eq!(
        saka_core::blocker::primary_action(&task).unwrap().kind,
        saka_core::vocab::BlockerAction::Cancel
    );
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let blocker = task.open_blocker().unwrap().id;
    engine
        .resolve_blocker_with(id, blocker, None, "nothing to revert", &cause, None)
        .unwrap();
    assert!(sim.run_until(STEPS, |s| {
        load(s, id).item.state == WorkItemState::Cancelled
    }));
    assert_eq!(sim.task(1).item.state, WorkItemState::Done);
    assert!(sim.task(1).reverted_by.is_none());
}

#[test]
fn a_revert_whose_commit_is_not_in_the_base_blocks_with_not_in_base() {
    let mut sim = shipped(config(Autonomy::Autonomous));
    sim.world.script_revert(Reverted::NotInBase);
    request(&sim, 1, "lost").unwrap();
    let id = revert_of(&sim, 1).item.id;
    assert!(sim.run_until(STEPS, |s| load(s, id).item.state == WorkItemState::Blocked));
    assert_eq!(
        load(&sim, id).open_blocker().unwrap().params["reason"],
        "not_in_base"
    );
}

#[test]
fn a_taken_over_conflict_returns_to_the_merge_stage_through_a_pull_request() {
    let mut sim = conflicting();
    let id = revert_of(&sim, 1).item.id;
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .takeover_with(id, "I will resolve it", &cause, None)
        .unwrap();
    assert_eq!(load(&sim, id).item.state, WorkItemState::TakenOver);
    let branch = load(&sim, id).item.branch;
    let head = sim.world.create_branch(saka_testkit::HOME_REPO, &branch);
    assert!(!head.is_empty());

    sim.block_on(engine.return_with(id, None, "resolved by hand", &cause, None))
        .unwrap();
    assert!(sim.run_until(STEPS, |s| {
        let t = load(s, id);
        t.change_request.is_some() && t.item.stage == Some(Stage::Merge)
            || t.item.state == WorkItemState::Done
    }));
    assert!(sim.run_until(STEPS, |s| load(s, id).item.state == WorkItemState::Done));
    assert_eq!(sim.task(1).reverted_by.unwrap().state, WorkItemState::Done);
}

#[test]
fn the_merge_and_done_hooks_of_a_revert_name_the_task_it_reverts() {
    let raw = |name: &str, on: &str| saka_core::hook::RawHook {
        name: name.into(),
        project: "tasks".into(),
        on: on.into(),
        action: "notify".into(),
        channel: Some("ops".into()),
        ..saka_core::hook::RawHook::default()
    };
    let mut cfg = config(Autonomy::Autonomous);
    cfg.hooks =
        saka_core::hook::parse_all(&[raw("landed", "merged"), raw("finished", "task_done")])
            .unwrap();
    let mut sim = shipped(cfg);
    request(&sim, 1, "hooked").unwrap();
    let id = revert_of(&sim, 1).item.id;
    assert!(sim.run_until(STEPS, |s| load(s, id).item.state == WorkItemState::Done));

    let runs = |sim: &Sim| {
        sim.engine()
            .hook_runs(
                &saka_engine::HookRunFilter {
                    project: Some(sim.project_id),
                    ..saka_engine::HookRunFilter::default()
                },
                &saka_engine::Page {
                    after: None,
                    limit: 200,
                },
            )
            .unwrap()
            .items
    };
    assert!(sim.run_until(STEPS, |s| runs(s).len() == 6));
    let all = runs(&sim);
    let revert_key = load(&sim, id).item.task_key;
    let of_revert: Vec<_> = all
        .iter()
        .filter(|r| r.context.task.as_ref().is_some_and(|t| t.key == revert_key))
        .collect();
    assert_eq!(of_revert.len(), 2);
    for run in &of_revert {
        let origin = run.context.revert_of.as_ref().unwrap();
        assert_eq!(origin.key, sim.task(1).item.task_key);
        assert_eq!(origin.id, sim.task(1).item.id);
    }
    let others = all.iter().filter(|r| r.context.revert_of.is_none()).count();
    assert_eq!(others, 4, "the two shipped steps keep a null revert_of");
}
