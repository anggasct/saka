#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::ids::BlockerId;
use saka_core::vocab::{BlockerCode, Stage, StagePhase, WorkItemState};
use saka_ports::{ChatAction, ChatActionKind, ChatOutcome};
use saka_testkit::{CiScript, CommandScript, Sim, SimConfig};
use uuid::Uuid;

const STEPS: usize = 8_000;

fn blocked_sim() -> Sim {
    let mut cfg = SimConfig {
        ci: CiScript::Pass,
        ..SimConfig::default()
    };
    cfg.operator.merge = false;
    cfg.operator.retry = vec![];
    let mut sim = Sim::new(cfg);
    for _ in 0..9 {
        sim.world
            .script_command("cd cli && go test ./...", CommandScript::Crash);
    }
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Blocked));
    sim
}

fn press(sim: &Sim, kind: ChatActionKind, blocker_id: BlockerId) -> ChatOutcome {
    sim.engine().chat_action(&ChatAction {
        platform: "telegram",
        user_id: "4242".into(),
        kind,
        blocker_id,
    })
}

fn actions(sim: &Sim) -> i64 {
    sim.engine()
        .metrics()
        .unwrap()
        .chat_actions
        .iter()
        .map(|(_, _, n)| i64::try_from(*n).unwrap())
        .sum()
}

#[test]
fn a_retry_from_chat_resolves_the_blocker_like_the_web_and_a_second_press_does_nothing() {
    let mut sim = blocked_sim();
    let blocker = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(blocker.code, BlockerCode::CiRed);

    assert_eq!(
        press(&sim, ChatActionKind::Retry, blocker.id),
        ChatOutcome::Done
    );
    let resolved = sim.events_of("delivery.blocker.resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].actor, "operator:telegram:4242");
    assert_eq!(resolved[0].payload["resolution"], "retried");
    assert_eq!(resolved[0].payload["reason"], "telegram chat");

    let before = sim.events_of("delivery.blocker.resolved").len();
    assert_eq!(
        press(&sim, ChatActionKind::Retry, blocker.id),
        ChatOutcome::Gone
    );
    assert_eq!(sim.events_of("delivery.blocker.resolved").len(), before);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    assert_eq!(sim.task(1).retries[&Stage::Merge].manual, 1);
    assert_eq!(actions(&sim), 2);
}

#[test]
fn a_button_for_another_action_or_another_blocker_changes_nothing() {
    let sim = blocked_sim();
    let blocker = sim.task(1).open_blocker().cloned().unwrap();
    let events = sim.engine().events_after(0, 10_000).unwrap().len();

    assert_eq!(
        press(&sim, ChatActionKind::Cancel, blocker.id),
        ChatOutcome::Changed
    );
    assert_eq!(
        press(
            &sim,
            ChatActionKind::Retry,
            BlockerId(Uuid::from_u128(0xdead))
        ),
        ChatOutcome::Gone
    );
    assert_eq!(sim.engine().events_after(0, 10_000).unwrap().len(), events);
    assert_eq!(sim.task(1).item.state, WorkItemState::Blocked);
}

#[test]
fn once_retries_run_out_the_shown_retry_button_is_stale_and_cancel_is_the_action() {
    let mut sim = blocked_sim();
    for _ in 0..40 {
        sim.world
            .script_command("cd cli && go test ./...", CommandScript::Crash);
    }
    let task = sim.task(1).item.id;
    let cause = sim.engine().cause("human:operator:local");
    for _ in 0..3 {
        sim.engine().retry(task, "again", &cause).unwrap();
        assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Blocked));
    }
    let blocker = sim.task(1).open_blocker().cloned().unwrap();

    assert_eq!(
        press(&sim, ChatActionKind::Retry, blocker.id),
        ChatOutcome::Changed
    );
    assert_eq!(sim.task(1).item.state, WorkItemState::Blocked);
    assert_eq!(
        press(&sim, ChatActionKind::Cancel, blocker.id),
        ChatOutcome::Done
    );
    assert_eq!(sim.task(1).item.state, WorkItemState::Cancelled);
}
