#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::task::MERGED_ON_HOST;
use saka_core::vocab::{
    BlockerCode, ChangeState, SessionState, Stage, StagePhase, StageRunState, WorkItemState,
};
use saka_testkit::{AgentScript, CiScript, Sim, SimConfig};
use serde_json::json;

const STEPS: usize = 8_000;

fn sim(ci: CiScript) -> Sim {
    let mut cfg = SimConfig {
        ci,
        ..SimConfig::default()
    };
    cfg.operator.merge = false;
    cfg.operator.retry = vec![];
    let mut sim = Sim::new(cfg);
    sim.approve();
    sim
}

fn blocked(s: &Sim) -> bool {
    s.task(1).item.state == WorkItemState::Blocked
}

fn done(s: &Sim) -> bool {
    s.task(1).item.state == WorkItemState::Done
}

fn merge_on_host(sim: &Sim) {
    let number = sim.task(1).change_request.unwrap().number;
    sim.world.merge_outside(number);
}

fn rows(sim: &Sim, sql: &str) -> Vec<Vec<Option<String>>> {
    let id = sim.task(1).item.id.0.to_string();
    sim.engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(sql)?;
            let n = stmt.column_count();
            let mut out = Vec::new();
            let mut found = stmt.query([id])?;
            while let Some(row) = found.next()? {
                let mut cells = Vec::new();
                for i in 0..n {
                    cells.push(row.get::<_, Option<String>>(i)?);
                }
                out.push(cells);
            }
            Ok(out)
        })
        .unwrap()
}

fn text(v: &str) -> Option<String> {
    Some(v.to_string())
}

fn count(sim: &Sim, sql: &str) -> i64 {
    rows(sim, sql)[0][0].as_deref().unwrap().parse().unwrap()
}

#[test]
fn a_change_merged_while_the_task_is_blocked_finishes_the_task_and_admits_the_next_step() {
    let mut sim = sim(CiScript::Fail(vec!["test".into()]));
    assert!(sim.run_until(STEPS, blocked));
    assert_eq!(sim.task(1).open_blocker().unwrap().code, BlockerCode::CiRed);

    merge_on_host(&sim);
    let at = sim.engine().now();
    let interval = SimConfig::default().loop_cfg.poll_interval_ms;
    assert!(sim.run_until(STEPS, |s| !blocked(s)));
    assert!(sim.engine().now().since(at) <= 2 * interval);
    assert!(sim.run_until(STEPS, done));

    let blockers = rows(
        &sim,
        "SELECT code, resolution FROM blocker WHERE work_item_id = ?1",
    );
    assert_eq!(blockers, vec![vec![text("ci_red"), text("auto")]]);
    let merged = rows(
        &sim,
        "SELECT CAST(json_extract(payload, '$.merged_by_saka') AS TEXT) FROM event
         WHERE kind = 'scm.change_request.merged' AND stream_id = ?1",
    );
    assert_eq!(merged, vec![vec![text("0")]]);
    let sync = rows(
        &sim,
        "SELECT reason FROM stage_run WHERE work_item_id = ?1 AND stage = 'sync'",
    );
    assert_eq!(sync, vec![vec![text(MERGED_ON_HOST)]]);
    assert_eq!(sim.stats().merges, 1, "only the merge on the host");
    assert!(sim.run_until(STEPS, |s| { s.task(2).item.state != WorkItemState::Queued }));
}

#[test]
fn a_change_merged_while_the_agent_works_ends_the_session_without_another_push() {
    let mut sim = sim(CiScript::Fail(vec!["test".into()]));
    assert!(sim.run_until(STEPS, blocked));
    sim.world.set_script(AgentScript {
        ask: Some(("Bash".into(), json!({ "command": "make deploy" }))),
        ..AgentScript::default()
    });
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry(sim.task(1).item.id, "fix the test", &cause)
        .unwrap();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Dev, StagePhase::RunningSession)
            && s.task(1)
                .session
                .is_some_and(|x| x.state == SessionState::Waiting)
    }));
    let pushes = sim.stats().pushes;
    let dev_run = sim.task(1).stage_run.unwrap().id;

    merge_on_host(&sim);
    assert!(sim.run_until(STEPS, done));

    assert_eq!(sim.stats().pushes, pushes);
    let session = sim.task(1).session.unwrap();
    assert_eq!(session.state, SessionState::Ended);
    let ended = rows(
        &sim,
        "SELECT json_extract(payload, '$.end_reason.code') FROM event
         WHERE kind = 'runtime.session.ended' AND json_extract(payload, '$.owner.work_item_id') = ?1",
    );
    assert_eq!(ended.last().unwrap(), &vec![text("change_merged")]);
    let stopped = count(
        &sim,
        "SELECT CAST(COUNT(*) AS TEXT) FROM job WHERE kind = 'session.stop'
         AND json_extract(payload, '$.owner.work_item_id') = ?1",
    );
    assert!(stopped >= 1);
    let dev = rows(
        &sim,
        "SELECT id, state FROM stage_run WHERE work_item_id = ?1 AND stage = 'dev' ORDER BY attempt",
    );
    let last = dev.last().unwrap();
    assert_eq!(last[0], Some(dev_run.0.to_string()));
    assert_eq!(last[1], text(StageRunState::Cancelled.as_str()));
    assert_eq!(
        sim.task(1).change_request.unwrap().state,
        ChangeState::Merged
    );
}

#[test]
fn a_poll_that_keeps_failing_while_blocked_leaves_the_blocker_alone() {
    let mut sim = sim(CiScript::Fail(vec!["test".into()]));
    assert!(sim.run_until(STEPS, blocked));
    let blocker = sim.task(1).open_blocker().cloned().unwrap();
    sim.world.fail_change_reads(true);
    let at = sim.engine().now();
    assert!(
        !sim.run_until(STEPS, |s| s.engine().now().since(at) > 1_800_000
            && s.task(1).open_blocker().map(|b| b.id) != Some(blocker.id))
    );
    assert_eq!(sim.task(1).open_blocker().unwrap().id, blocker.id);
    let exhausted = count(
        &sim,
        "SELECT CAST(COUNT(*) AS TEXT) FROM job WHERE kind = 'scm.poll_change_request' AND state = 'dead'
         AND json_extract(payload, '$.work_item_id') = ?1",
    );
    assert!(exhausted >= 1);
    let blocks = count(
        &sim,
        "SELECT CAST(COUNT(*) AS TEXT) FROM event WHERE kind = 'delivery.work_item.blocked' AND stream_id = ?1",
    );
    assert_eq!(blocks, 1);

    sim.world.fail_change_reads(false);
    merge_on_host(&sim);
    assert!(sim.run_until(STEPS, done));
}

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

#[test]
fn the_time_from_merge_to_the_next_admission_is_measured() {
    let buffer = Buffer::default();
    let out = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || out.clone())
        .finish();
    let sim = tracing::subscriber::with_default(subscriber, || {
        let mut sim = sim(CiScript::Pass);
        sim.cfg.operator.merge = true;
        assert!(sim.run_until(STEPS, |s| s.task(2).item.admitted_at.is_some()));
        sim
    });
    let merged_at = sim.task(1).change_request.unwrap().merged_at.unwrap();
    let admitted_at = sim.task(2).item.admitted_at.unwrap();
    let waited = u64::try_from(admitted_at.since(merged_at)).unwrap();
    let expected = std::time::Duration::from_millis(waited).as_secs_f64();

    let h = sim.engine().metrics().unwrap().advance_seconds;
    assert_eq!(h.count, 1);
    assert!((h.sum - expected).abs() < 1e-9, "{} vs {expected}", h.sum);

    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    let line: serde_json::Value = logs
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|l| l["fields"]["message"] == "delivery.advance")
        .unwrap();
    assert_eq!(line["fields"]["seconds"], json!(expected));
    assert_eq!(
        line["fields"]["after_work_item_id"],
        json!(sim.task(1).item.id.to_string())
    );
}
