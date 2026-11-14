#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{BlockerCode, WorkItemState};
use saka_testkit::{CiScript, Sim, SimConfig};
use serde_json::{Value, json};

const STEPS: usize = 8_000;

#[derive(Clone, Default)]
struct Buffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn sim(retry: Vec<BlockerCode>) -> Sim {
    let mut cfg = SimConfig {
        ci: CiScript::Pass,
        ..SimConfig::default()
    };
    cfg.operator.retry = retry;
    let mut sim = Sim::new(cfg);
    sim.approve();
    sim
}

fn blocked(s: &Sim) -> bool {
    s.task(1).item.state == WorkItemState::Blocked
}

fn dead_jobs(sim: &Sim, kind: &str) -> Vec<Value> {
    sim.engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(
                "SELECT last_error FROM job WHERE kind = ?1 AND state = 'dead' ORDER BY created_at",
            )?;
            let rows = stmt.query_map([kind], |r| r.get::<_, String>(0))?;
            Ok(rows
                .map(|r| serde_json::from_str(&r.unwrap()).unwrap())
                .collect())
        })
        .unwrap()
}

#[test]
fn a_job_that_panics_fails_for_good_with_the_panic_message_and_the_queue_goes_on() {
    let mut sim = sim(vec![]);
    sim.world
        .panic_on("open_change_request", 1, "the fake host broke");
    assert!(sim.run_until(STEPS, blocked));

    let blocker = sim.task(1).open_blocker().cloned().unwrap();
    assert_eq!(blocker.code, BlockerCode::ScmUnavailable);
    assert_eq!(blocker.params["phase"], "opening_change");
    let dead = dead_jobs(&sim, "scm.open_change_request");
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0]["code"], "job_panicked");
    assert_eq!(
        dead[0]["params"],
        json!({ "message": "the fake host broke" })
    );

    sim.cfg.operator.retry = vec![BlockerCode::ScmUnavailable];
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(dead_jobs(&sim, "scm.open_change_request").len(), 1);
}

#[test]
fn three_jobs_in_a_row_that_panic_do_not_stop_the_queue() {
    let mut sim = sim(vec![BlockerCode::ScmUnavailable]);
    sim.world.panic_on("open_change_request", 3, "boom");
    assert!(sim.run_to_shipped(STEPS));

    let dead = dead_jobs(&sim, "scm.open_change_request");
    assert_eq!(dead.len(), 3);
    assert!(dead.iter().all(|e| e["code"] == "job_panicked"));
}

#[test]
fn every_panic_is_counted_per_job_kind() {
    let mut sim = sim(vec![BlockerCode::ScmUnavailable]);
    sim.world.panic_on("open_change_request", 2, "boom");
    sim.world.panic_on("push", 1, "boom");
    assert!(sim.run_to_shipped(STEPS));

    let metrics = sim.engine().metrics().unwrap();
    assert_eq!(
        metrics.job_panics,
        [
            ("scm.open_change_request".to_string(), 2),
            ("scm.push_branch".to_string(), 1),
        ]
    );
}

#[test]
fn a_panic_with_a_long_message_keeps_the_first_kilobyte() {
    let mut sim = sim(vec![]);
    sim.world.panic_on("merge", 1, &"é".repeat(1500));
    assert!(sim.run_until(STEPS, blocked));

    let dead = dead_jobs(&sim, "scm.merge");
    let message = dead[0]["params"]["message"].as_str().unwrap();
    assert_eq!(message.len(), 1024);
    assert!(message.chars().all(|c| c == 'é'));
}

#[test]
fn the_only_worker_keeps_claiming_jobs_after_three_panics() {
    use saka_core::vocab::SpecState;
    use std::time::Duration;

    let mut sim = sim(vec![BlockerCode::ScmUnavailable]);
    sim.world.panic_on("open_change_request", 3, "boom");
    let engine = sim.engine();
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let mut worker = Box::pin(engine.clone().run(1, shutdown));

    let mut shipped = false;
    for _ in 0..2_000 {
        sim.operate();
        while let Some(event) = sim.world.pop_event() {
            engine.on_runner_event(event).unwrap();
        }
        sim.clock.advance(1_000);
        sim.block_on(async {
            tokio::select! {
                () = &mut worker => unreachable!("the worker runs until it is stopped"),
                () = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
        });
        if sim.spec().state == SpecState::Shipped {
            shipped = true;
            break;
        }
    }
    stop.send(true).unwrap();
    sim.block_on(worker);

    assert!(shipped);
    assert_eq!(dead_jobs(&sim, "scm.open_change_request").len(), 3);
}

#[test]
fn a_panic_is_logged_with_the_job_and_the_work_item() {
    saka_testkit::logs::check_callsites_per_thread();
    let buffer = Buffer::default();
    let out = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::ERROR)
        .with_writer(move || out.clone())
        .finish();
    let sim = tracing::subscriber::with_default(subscriber, || {
        let mut sim = sim(vec![]);
        sim.world.panic_on("open_change_request", 1, "boom");
        assert!(sim.run_until(STEPS, blocked));
        sim
    });

    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    let line: Value = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|l| l["fields"]["message"] == "job.panicked")
        .unwrap();
    assert_eq!(line["level"], "ERROR");
    assert_eq!(line["fields"]["kind"], "scm.open_change_request");
    assert_eq!(line["fields"]["panic"], "boom");
    assert_eq!(
        line["fields"]["work_item_id"],
        json!(sim.task(1).item.id.to_string())
    );
    assert!(line["fields"]["job_id"].is_string());
}
