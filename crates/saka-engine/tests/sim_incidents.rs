#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::blocker::primary_action;
use saka_core::model::Task;
use saka_core::vocab::{
    BlockerAction, BlockerCode, SessionState, Stage, StagePhase, StageRunState, Trigger,
    WorkItemState,
};
use saka_core::workflow::Workflow;
use saka_engine::{EngineError, SessionCommand};
use saka_ports::Clock;
use saka_testkit::{
    AgentOutcome, AgentScript, ChangeSpec, CiScript, FileEdit, HOME_REPO, Layout, Operator,
    ReviewReply, SHOP_REPO, Sim, SimConfig, repo_entry, saka_toml,
};

const STEPS: usize = 20_000;
const MINUTE_MS: i64 = 60_000;

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

fn head(task: &Task) -> String {
    task.change_request.as_ref().unwrap().head_sha.clone()
}

fn stage_runs(sim: &Sim) -> Vec<(String, String)> {
    let id = sim.task(1).item.id.0.to_string();
    sim.engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT stage, state FROM stage_run WHERE work_item_id = ?1 ORDER BY created_at, id",
            )?;
            let rows = stmt.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.map(Result::unwrap).collect())
        })
        .unwrap()
}

fn hanging() -> AgentScript {
    AgentScript {
        outcome: AgentOutcome::Hang,
        ..AgentScript::default()
    }
}

fn wait_minutes(sim: &mut Sim, minutes: i64) {
    let until = sim.clock.now().0 + minutes * MINUTE_MS;
    assert!(sim.run_until(STEPS, |s| s.clock.now().0 >= until));
}

fn live_and_running(sim: &Sim) -> bool {
    sim.task(1)
        .live_session()
        .is_some_and(|s| s.state == SessionState::Running && s.cli_session_id.is_some())
}

#[test]
fn a_fix_round_that_starts_on_a_dirty_worktree_blocks_the_task_with_the_reason() {
    let mut sim = Sim::new(hand_merge());
    fresh_commits(&sim);
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::approve(),
    ]);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Fix, StagePhase::RunningSession)
    }));
    let rejected = head(&sim.task(1));
    let pushes = sim.stats().pushes;

    sim.world.dirty_worktrees();
    sim.world.set_script(AgentScript {
        outcome: AgentOutcome::NoCommit,
        ..AgentScript::default()
    });
    assert!(sim.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));

    let task = sim.task(1);
    assert_eq!(task.item.state, WorkItemState::Blocked);
    let run = task.stage_run.as_ref().unwrap();
    assert_eq!((run.stage, run.state), (Stage::Fix, StageRunState::Blocked));
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::NoChanges);
    assert_eq!(blocker.params["dirty"], true);
    assert!(task.live_session().is_none(), "no agent is left running");
    assert_eq!(head(&task), rejected, "nothing new was committed");
    assert_eq!(sim.stats().pushes, pushes, "nothing was pushed");
    assert_eq!(
        primary_action(&task).unwrap().kind,
        BlockerAction::Retry,
        "the operator is offered the retry"
    );

    let waiting = sim.task(1).item.version;
    sim.run_until(600, |_| false);
    assert_eq!(sim.task(1).item.version, waiting, "the task stays put");
}

#[test]
fn a_model_that_thinks_longer_than_the_runner_silence_window_is_neither_killed_nor_retried() {
    let mut sim = Sim::new(SimConfig {
        agent: hanging(),
        operator: Operator {
            start: true,
            merge: true,
            retry: vec![],
            answer: Some(true),
            decide: None,
        },
        ..SimConfig::without_review()
    });
    sim.approve();
    assert!(sim.run_until(STEPS, live_and_running));
    let session = sim.task(1).live_session().unwrap().clone();

    for minute in 1..=10 {
        wait_minutes(&mut sim, 1);
        let task = sim.task(1);
        let live = task.live_session().unwrap();
        assert_eq!(
            (live.id, live.state),
            (session.id, SessionState::Running),
            "minute {minute}"
        );
        assert!(task.open_blocker().is_none(), "minute {minute}");
        assert_eq!(task.item.state, WorkItemState::InStage, "minute {minute}");
    }
    assert!(sim.events_of("runtime.session.lost").is_empty());
    assert!(task_retries(&sim).is_empty(), "no retry was spent");

    sim.world.set_script(AgentScript::default());
    sim.world.finish_turn(session.id);
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(
        sim.sessions(1).len(),
        1,
        "the same session finished the work"
    );
    assert_eq!(sim.sessions(1)[0].resumed_from_session_id, None);
    assert!(sim.events_of("runtime.session.lost").is_empty());
    assert!(task_retries(&sim).is_empty());
    let dev_runs = stage_runs(&sim).iter().filter(|r| r.0 == "dev").count();
    assert_eq!(dev_runs, 1, "the dev stage ran once");
}

fn task_retries(sim: &Sim) -> Vec<(Stage, u32, u32)> {
    sim.task(1)
        .retries
        .iter()
        .map(|(stage, r)| (*stage, r.manual, r.auto))
        .filter(|(_, manual, auto)| manual + auto > 0)
        .collect()
}

fn deliveries(sim: &Sim) -> i64 {
    sim.engine()
        .store()
        .read(|c| Ok(c.query_row("SELECT COUNT(*) FROM webhook_delivery", [], |r| r.get(0))?))
        .unwrap()
}

const FIX_MANUAL: &str = "[workflow]\npreset = \"custom\"\n\n[workflow.change_request]\n\n[workflow.stages]\nreview = \"auto\"\nfix = \"manual\"\n";

#[test]
fn a_pull_request_whose_opened_webhook_was_lost_is_bound_by_polling_and_still_takes_comments() {
    let mut text = saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]);
    text.push('\n');
    text.push_str(FIX_MANUAL);
    let layout = Layout {
        saka_toml: text,
        ..Layout::default()
    };
    let mut sim = Sim::multi_repo(SimConfig::default(), &layout);
    sim.world.script_edits(
        (1..=4)
            .map(|n| vec![(format!("src/fix{n}.rs"), Some("fixed\n".to_string()))])
            .collect(),
    );
    sim.world.script_reviews(vec![
        ReviewReply::request_changes("Cover the empty list."),
        ReviewReply::approve(),
    ]);
    let number = sim.world.open_external_change(
        SHOP_REPO,
        ChangeSpec::new("Add retries", "alice", "feature/retries"),
    );
    assert_eq!(deliveries(&sim), 0, "no webhook ever arrived for it");

    assert!(sim.run_until(STEPS, |s| {
        s.externals()
            .iter()
            .any(|t| t.is_at(Stage::Review, StagePhase::Reviewed))
    }));
    assert_eq!(deliveries(&sim), 0, "polling alone bound it");
    let task = sim.externals().remove(0);
    assert_eq!(task.item.task_key, format!("shop#{number}"));
    assert_eq!(task.change_request.as_ref().unwrap().number, number);
    assert_eq!(
        task.reviews.len(),
        1,
        "the polled pull request was reviewed"
    );

    sim.world.set_write_access("bob", true);
    sim.world
        .comment_on_change(SHOP_REPO, number, "bob", "Looks fixable.\n/saka fix\n");
    assert!(sim.run_until(STEPS, |s| {
        s.externals()
            .iter()
            .any(|t| t.reviews.len() == 2 && t.is_at(Stage::Review, StagePhase::Reviewed))
    }));
    let task = sim.externals().remove(0);
    assert_eq!(task.fix_rounds_used(), 1, "the later comment started a fix");
    assert!(task.approved_at(&head(&task)));
    assert_eq!(sim.externals().len(), 1, "one task, never a duplicate");
}

#[test]
fn an_operator_stop_is_not_a_model_error_and_is_never_retried() {
    let mut sim = Sim::new(SimConfig {
        agent: hanging(),
        ..SimConfig::without_review()
    });
    sim.approve();
    assert!(sim.run_until(STEPS, live_and_running));
    let session = sim.task(1).live_session().unwrap().clone();
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");

    engine
        .session_command(session.id, SessionCommand::Interrupt, &cause, None)
        .unwrap();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .live_session()
            .is_some_and(|x| x.state == SessionState::Idle)
    }));
    let task = sim.task(1);
    assert!(
        task.open_blocker().is_none(),
        "an interrupt is not a failure"
    );
    assert_eq!(task.item.state, WorkItemState::InStage);

    engine
        .session_command(
            session.id,
            SessionCommand::Stop {
                reason: "wrong approach".into(),
            },
            &cause,
            None,
        )
        .unwrap();
    assert!(sim.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));
    let task = sim.task(1);
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::SessionFailed);
    assert_eq!(blocker.params["cause"], "operator_stop");
    assert_eq!(blocker.params["reason"], "wrong approach");
    assert!(
        blocker.params.get("subtype").is_none(),
        "no model error is recorded"
    );
    let ended = &sim.sessions(1)[0];
    assert_eq!(
        ended.end_reason.as_ref().unwrap().code,
        "operator_stop",
        "the session ended by the operator, not by an error"
    );

    let (runs, sessions) = (stage_runs(&sim), sim.sessions(1).len());
    sim.world.set_script(AgentScript::default());
    wait_minutes(&mut sim, 20);
    let task = sim.task(1);
    assert_eq!(task.item.state, WorkItemState::Blocked, "it stays blocked");
    assert_eq!(stage_runs(&sim), runs, "no run was started again");
    assert_eq!(
        sim.sessions(1).len(),
        sessions,
        "no session was started again"
    );
    assert!(task_retries(&sim).is_empty(), "no automatic retry");
    assert!(sim.events_of("runtime.session.lost").is_empty());
}

#[test]
fn a_stage_unblocked_by_hand_shows_its_real_stage_and_never_the_old_failed_state() {
    let mut sim = Sim::new(SimConfig {
        ci: CiScript::Fail(vec!["lint".into()]),
        ..hand_merge()
    });
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));
    let blocked = sim.task(1);
    assert_eq!(blocked.open_blocker().unwrap().code, BlockerCode::CiRed);
    assert_eq!(blocked.item.state, WorkItemState::Blocked);
    assert_eq!(
        blocked.stage_run.as_ref().map(|r| (r.stage, r.state)),
        Some((Stage::Merge, StageRunState::Blocked)),
        "a blocked task still says which stage stopped"
    );

    sim.world.set_ci(CiScript::Pass);
    let engine = sim.engine();
    engine
        .retry(
            blocked.item.id,
            "fixed the lint",
            &engine.cause("human:operator:local"),
        )
        .unwrap();

    let task = sim.task(1);
    assert!(task.open_blocker().is_none(), "the blocker is closed");
    assert_eq!(task.item.state, WorkItemState::InStage);
    let run = task.stage_run.as_ref().unwrap();
    assert_eq!((run.stage, run.attempt), (Stage::Dev, 2));
    assert_eq!(run.state, StageRunState::Running);
    assert_eq!(run.reason.as_deref(), Some("fixed the lint"));
    assert_eq!(
        (task.item.stage, task.item.stage_phase),
        (Some(run.stage), Some(run.entry_phase)),
        "the task shows the stage that runs, not the one that failed"
    );
    let view = engine.work_item(task.item.id).unwrap().unwrap();
    let listed: Vec<_> = engine
        .work_items(
            &Default::default(),
            &saka_store::list::Page {
                after: None,
                limit: 10,
            },
        )
        .unwrap()
        .items
        .into_iter()
        .filter(|v| v.task.item.id == task.item.id)
        .collect();
    assert_eq!(view.task.item, task.item);
    assert_eq!(listed[0].task.item, task.item, "the list says the same");
    let attempts: Vec<_> = view
        .stage_runs
        .iter()
        .map(|r| (r.stage, r.attempt, r.state))
        .collect();
    assert_eq!(
        attempts.last(),
        Some(&(Stage::Dev, 2, StageRunState::Running)),
        "the newest run is the one that runs"
    );

    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    let task = sim.task(1);
    assert_eq!(task.item.state, WorkItemState::InStage);
    assert_eq!(task.item.stage, Some(Stage::Merge));
    assert!(task.open_blocker().is_none());
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

fn not_ready(result: Result<(), EngineError>) -> serde_json::Value {
    match result {
        Err(EngineError::Rejected(rejection)) => {
            assert_eq!(rejection.code(), "merge_not_ready", "{rejection:?}");
            rejection.params()
        }
        other => panic!("expected a refused merge, got {other:?}"),
    }
}

#[test]
fn a_blocking_verdict_never_applies_to_a_moved_head_and_the_refused_merge_says_why() {
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
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .open_blocker()
            .is_some_and(|b| b.code == BlockerCode::VerdictFailed)
    }));
    let judged = head(&sim.task(1));
    let number = sim.task(1).change_request.unwrap().number;

    let human = sim.world.push_to_change(HOME_REPO, number);
    assert!(sim.run_until(STEPS, |s| head(&s.task(1)) == human));
    let task = sim.task(1);
    assert!(
        task.rejected_at(&human).is_none(),
        "the verdict is for the old head"
    );
    assert!(!task.approved_at(&human));

    let params = not_ready(request_merge(&sim));
    assert_eq!(params["reason"], "review");
    assert_eq!(params["head_sha"], human.as_str());
    assert_eq!(params["review_head_sha"], judged.as_str());
    assert_eq!(params["review_outcome"], "changes_requested");
    assert!(sim.world.merge_calls().is_empty());

    let engine = sim.engine();
    engine
        .retry(
            task.item.id,
            "one more round",
            &engine.cause("human:operator:local"),
        )
        .unwrap();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    let task = sim.task(1);
    assert!(task.approved_at(&head(&task)));
    assert_ne!(
        head(&task),
        human,
        "the review judged the head after the fix"
    );
    request_merge(&sim).unwrap();
}
