#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{SpecState, WorkItemState};
use saka_testkit::{APPROVAL_PATH, Sim, SimConfig, approval_spec};

const STEPS: usize = 8_000;

fn count(sim: &Sim, sql: &str) -> i64 {
    sim.engine()
        .store()
        .read(|c| Ok(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

#[test]
fn re_approval_mid_run_rebuilds_only_the_steps_that_did_not_start() {
    let mut sim = Sim::new(SimConfig {
        max_active_tasks: 1,
        ..SimConfig::default()
    });
    let first = sim.register(
        APPROVAL_PATH,
        &approval_spec(
            "",
            &[
                ("Build the base", "AC-01"),
                ("Old second step", "AC-02"),
                ("Old third step", "AC-02"),
            ],
        ),
    );
    assert!(first.lint_ok, "{:?}", first.issues);
    sim.approve_spec(first.spec_id, first.revision_id).unwrap();

    let state = |s: &Sim, n: usize| s.tasks_of(first.spec_id)[n - 1].item.state;
    assert!(sim.run_until(STEPS, |s| state(s, 1) == WorkItemState::InStage));
    let second = sim.register(
        APPROVAL_PATH,
        &approval_spec(
            "",
            &[
                ("Build the base", "AC-01"),
                ("New second step", "AC-02"),
                ("New third step", "AC-02"),
                ("Fourth step", "AC-01, AC-02"),
            ],
        ),
    );
    assert!(second.lint_ok, "{:?}", second.issues);
    sim.approve_spec(first.spec_id, second.revision_id).unwrap();
    assert_eq!(sim.tasks_of(first.spec_id).len(), 4);

    assert!(sim.run_until(STEPS, |s| state(s, 2) == WorkItemState::InStage));
    let third = sim.register(
        APPROVAL_PATH,
        &approval_spec(
            "",
            &[
                ("Build the base", "AC-01"),
                ("New second step", "AC-02"),
                ("New third step", "AC-01, AC-02"),
            ],
        ),
    );
    assert!(third.lint_ok, "{:?}", third.issues);
    sim.approve_spec(first.spec_id, third.revision_id).unwrap();

    assert!(
        sim.run_until(STEPS, |s| s.spec_of(first.spec_id).state
            == SpecState::Shipped),
        "not shipped"
    );
    let tasks = sim.tasks_of(first.spec_id);
    let pinned: Vec<_> = tasks.iter().map(|t| t.item.spec_revision_id).collect();
    assert_eq!(
        pinned,
        vec![
            Some(first.revision_id),
            Some(second.revision_id),
            Some(third.revision_id),
            Some(second.revision_id),
        ]
    );
    let states: Vec<_> = tasks.iter().map(|t| t.item.state).collect();
    assert_eq!(
        states,
        vec![
            WorkItemState::Done,
            WorkItemState::Done,
            WorkItemState::Done,
            WorkItemState::Superseded
        ]
    );
    let s4 = &tasks[3];
    assert!(s4.environment.is_none() && s4.session.is_none() && s4.change_request.is_none());
    for table in ["environment", "session", "change_request", "stage_run"] {
        assert_eq!(
            count(
                &sim,
                &format!(
                    "SELECT COUNT(*) FROM {table} WHERE work_item_id = '{}'",
                    s4.item.id.0
                )
            ),
            0,
            "{table}"
        );
    }

    let prs = sim.world.prs();
    let titles: Vec<(String, String)> = prs
        .iter()
        .map(|p| (p.head_branch.clone(), p.title.clone()))
        .collect();
    assert_eq!(
        titles,
        vec![
            ("saka/approval/s1".to_string(), "Build the base".to_string()),
            (
                "saka/approval/s2".to_string(),
                "New second step".to_string()
            ),
            ("saka/approval/s3".to_string(), "New third step".to_string()),
        ]
    );
    assert_eq!(sim.stats().merges, 3);
    let prompts = sim.world.prompts();
    let packet = |line: &str| prompts.iter().any(|p| p.contains(line));
    assert!(packet("Task: approval/s1 — Build the base"));
    assert!(packet("Task: approval/s2 — New second step"));
    assert!(packet("Task: approval/s3 — New third step"));
    assert!(!packet("Task: approval/s4"));
    assert!(!packet("— Old second step"));
    assert!(!packet("— Old third step"));
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

fn logged(buffer: &Buffer, message: &str) -> Vec<serde_json::Value> {
    let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    text.lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|l| l["fields"]["message"] == message)
        .map(|l| l["fields"].clone())
        .collect()
}

#[test]
fn re_approval_and_withdraw_log_once_with_their_counts() {
    let sim = Sim::new(SimConfig {
        max_active_tasks: 0,
        ..SimConfig::default()
    });
    let buffer = Buffer::default();
    let out = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(move || out.clone())
        .finish();
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let (spec, from, to) = tracing::subscriber::with_default(subscriber, || {
        let first = sim.register(
            APPROVAL_PATH,
            &approval_spec(
                "",
                &[("One", "AC-01"), ("Two", "AC-02"), ("Three", "AC-02")],
            ),
        );
        sim.approve_spec(first.spec_id, first.revision_id).unwrap();
        let second = sim.register(
            APPROVAL_PATH,
            &approval_spec("", &[("One", "AC-01"), ("Two again", "AC-02")]),
        );
        sim.approve_spec(first.spec_id, second.revision_id).unwrap();
        engine
            .withdraw_spec(first.spec_id, "not needed", &cause)
            .unwrap();
        (first.spec_id, first.revision_id, second.revision_id)
    });
    let approved = logged(&buffer, "spec re-approved");
    assert_eq!(approved.len(), 1, "{approved:?}");
    let a = &approved[0];
    assert_eq!(a["spec_id"], spec.0.to_string());
    assert_eq!(a["from_revision_id"], from.0.to_string());
    assert_eq!(a["to_revision_id"], to.0.to_string());
    assert_eq!(
        (
            &a["kept"],
            &a["repinned"],
            &a["restored"],
            &a["added"],
            &a["superseded"]
        ),
        (
            &serde_json::json!(0),
            &serde_json::json!(2),
            &serde_json::json!(0),
            &serde_json::json!(0),
            &serde_json::json!(1)
        )
    );
    let withdrawn = logged(&buffer, "spec withdrawn");
    assert_eq!(withdrawn.len(), 1, "{withdrawn:?}");
    assert_eq!(withdrawn[0]["spec_id"], spec.0.to_string());
    assert_eq!(withdrawn[0]["superseded"], 2);
}
