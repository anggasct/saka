#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{Stage, StagePhase, WorkItemState};
use saka_engine::webhooks::{AcceptedWebhook, WEBHOOK_RETENTION_MS, WebhookOutcome};
use saka_ports::Clock;
use saka_testkit::{CiScript, FAKE_SIGNATURE_HEADER, Sim, SimConfig, fake_signature};
use serde_json::{Value, json};

const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";
const STEPS: usize = 5_000;
const SLOW_POLL_MS: i64 = 300_000;

fn sim() -> Sim {
    let mut cfg = SimConfig {
        ci: CiScript::Silent,
        ..SimConfig::without_review()
    };
    cfg.loop_cfg.poll_interval_ms = SLOW_POLL_MS;
    cfg.loop_cfg.ci_appear_timeout_ms = 3_600_000;
    let sim = Sim::new(cfg);
    sim.world.set_webhook_secret(SECRET);
    sim.approve_spec(sim.spec_id, sim.revision_id).unwrap();
    sim
}

fn deliver(sim: &Sim, payload: &Value) -> AcceptedWebhook {
    let body = serde_json::to_vec(payload).unwrap();
    let headers = vec![(
        FAKE_SIGNATURE_HEADER.to_string(),
        fake_signature(SECRET, &body),
    )];
    let engine = sim.engine();
    let delivery = engine.parse_webhook("github", &headers, &body).unwrap();
    engine.accept_webhook("github", &delivery).unwrap()
}

fn count(sim: &Sim, sql: &str) -> i64 {
    sim.engine()
        .store()
        .read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

fn awaiting_ci(sim: &mut Sim) -> (u64, String) {
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingCi)
    }));
    let cr = sim.task(1).change_request.unwrap();
    (cr.number, cr.head_sha)
}

#[test]
fn a_check_event_moves_the_gate_before_the_next_poll() {
    let mut sim = sim();
    let (_, head) = awaiting_ci(&mut sim);
    while sim
        .engine()
        .store()
        .oldest_ready_job_age_ms(sim.clock.now())
        .unwrap()
        .is_some()
    {
        sim.step();
    }
    sim.world.set_ci(CiScript::Pass);
    let started = sim.clock.now();

    let accepted = deliver(
        &sim,
        &json!({ "delivery_id": "d-1", "event": "check_run", "repo": "acme/tasks", "targets": [{ "head": head }] }),
    );
    assert_eq!(accepted.outcome, WebhookOutcome::Refreshed);
    assert_eq!(accepted.change_requests, 1);
    let mut steps = 0;
    while !sim.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge) {
        assert!(steps < 3, "the gate did not move within three worker steps");
        sim.step();
        steps += 1;
    }
    assert!(sim.clock.now().since(started) < SLOW_POLL_MS);
    assert_eq!(
        count(
            &sim,
            "SELECT COUNT(*) FROM webhook_delivery WHERE outcome = 'refreshed' AND change_requests = 1"
        ),
        1
    );
}

#[test]
fn repeats_and_unrelated_deliveries_schedule_nothing() {
    let mut sim = sim();
    let (number, head) = awaiting_ci(&mut sim);
    let hook_jobs = |s: &Sim| {
        count(
            s,
            "SELECT COUNT(*) FROM job WHERE idempotency_key LIKE '%:hook:%'",
        )
    };

    let payload = json!({ "delivery_id": "d-2", "event": "pull_request", "repo": "acme/tasks", "targets": [{ "change": number }] });
    assert_eq!(deliver(&sim, &payload).outcome, WebhookOutcome::Refreshed);
    let again = deliver(&sim, &payload);
    assert_eq!(again.outcome, WebhookOutcome::Duplicate);
    assert_eq!(again.change_requests, 1);
    assert_eq!(hook_jobs(&sim), 1);
    assert_eq!(count(&sim, "SELECT COUNT(*) FROM webhook_delivery"), 1);

    let other_head = "f".repeat(40);
    for (id, payload) in [
        (
            "d-3",
            json!({ "event": "check_run", "repo": "acme/unknown", "targets": [{ "head": head }] }),
        ),
        ("d-4", json!({ "event": "ping", "repo": "acme/tasks" })),
        (
            "d-5",
            json!({ "event": "status", "repo": "acme/tasks", "targets": [{ "head": other_head }] }),
        ),
        (
            "d-6",
            json!({ "event": "pull_request", "repo": "acme/tasks", "targets": [{ "change": number + 100 }] }),
        ),
    ] {
        let mut payload = payload;
        payload["delivery_id"] = json!(id);
        let accepted = deliver(&sim, &payload);
        assert_eq!(accepted.outcome, WebhookOutcome::Ignored, "{id}");
        assert_eq!(accepted.change_requests, 0, "{id}");
    }
    assert_eq!(hook_jobs(&sim), 1);
    assert_eq!(
        count(
            &sim,
            "SELECT COUNT(*) FROM webhook_delivery WHERE outcome = 'ignored' AND repo_id IS NULL"
        ),
        1
    );
}

#[test]
fn a_change_merged_on_the_host_reaches_sync_through_its_webhook() {
    let mut sim = sim();
    let (number, _) = awaiting_ci(&mut sim);
    while sim
        .engine()
        .store()
        .oldest_ready_job_age_ms(sim.clock.now())
        .unwrap()
        .is_some()
    {
        sim.step();
    }
    sim.world.merge_outside(number);
    let accepted = deliver(
        &sim,
        &json!({ "delivery_id": "d-7", "event": "pull_request", "action": "closed", "repo": "acme/tasks", "targets": [{ "change": number }] }),
    );
    assert_eq!(accepted.outcome, WebhookOutcome::Refreshed);
    assert!(
        sim.run_until(20, |s| s.task(1).item.stage == Some(Stage::Sync)
            || s.task(1).item.state == WorkItemState::Done)
    );
}

#[test]
fn a_blocked_task_whose_change_is_merged_reaches_sync_through_its_webhook() {
    let mut sim = sim();
    let (number, _) = awaiting_ci(&mut sim);
    sim.cfg.operator.retry = vec![];
    sim.world.set_ci(CiScript::Fail(vec!["test".into()]));
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Blocked));
    while sim
        .engine()
        .store()
        .oldest_ready_job_age_ms(sim.clock.now())
        .unwrap()
        .is_some()
    {
        sim.step();
    }
    sim.world.merge_outside(number);
    let accepted = deliver(
        &sim,
        &json!({ "delivery_id": "d-9", "event": "pull_request", "action": "closed", "repo": "acme/tasks", "targets": [{ "change": number }] }),
    );
    assert_eq!(accepted.outcome, WebhookOutcome::Refreshed);
    assert!(
        sim.run_until(20, |s| s.task(1).item.stage == Some(Stage::Sync)
            || s.task(1).item.state == WorkItemState::Done)
    );
}

#[test]
fn a_task_without_a_change_ignores_webhooks() {
    let sim = sim();
    let accepted = deliver(
        &sim,
        &json!({ "delivery_id": "d-8", "event": "pull_request", "repo": "acme/tasks", "targets": [{ "change": 1 }] }),
    );
    assert_eq!(accepted.outcome, WebhookOutcome::Ignored);
}

#[test]
fn old_deliveries_are_pruned_once_per_hour() {
    let mut sim = sim();
    deliver(
        &sim,
        &json!({ "delivery_id": "old", "event": "ping", "repo": "acme/tasks" }),
    );
    sim.clock.advance(WEBHOOK_RETENTION_MS - 3_600_000);
    deliver(
        &sim,
        &json!({ "delivery_id": "young", "event": "ping", "repo": "acme/tasks" }),
    );
    sim.clock.advance(3_600_000 + 60_000);
    assert!(sim.run_until(50, |s| count(
        s,
        "SELECT COUNT(*) FROM webhook_delivery WHERE delivery_id = 'old'"
    ) == 0));
    assert_eq!(
        count(
            &sim,
            "SELECT COUNT(*) FROM webhook_delivery WHERE delivery_id = 'young'"
        ),
        1
    );
    let hour = sim.clock.now().0.div_euclid(3_600_000);
    assert_eq!(
        count(
            &sim,
            &format!("SELECT COUNT(*) FROM job WHERE idempotency_key = 'scm.webhook_prune:{hour}'")
        ),
        1
    );
    for _ in 0..5 {
        sim.engine().reconcile().unwrap();
    }
    assert_eq!(
        count(
            &sim,
            &format!(
                "SELECT COUNT(*) FROM job WHERE idempotency_key LIKE 'scm.webhook_prune:{hour}%'"
            )
        ),
        1
    );
}
