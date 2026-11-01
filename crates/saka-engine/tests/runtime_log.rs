#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::model::Budget;
use saka_core::vocab::{SessionState, Stage};
use saka_engine::SessionCommand;
use saka_testkit::{AgentOutcome, AgentScript, Operator, Sim, SimConfig};
use serde_json::{Value, json};

const STEPS: usize = 5_000;

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

fn sim(agent: AgentScript, tune: impl FnOnce(&mut SimConfig)) -> Sim {
    let mut cfg = SimConfig {
        agent,
        operator: Operator {
            merge: true,
            retry: vec![],
            answer: None,
        },
        ..SimConfig::default()
    };
    tune(&mut cfg);
    let mut sim = Sim::new(cfg);
    sim.approve();
    sim
}

fn running(sim: &mut Sim) {
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .session
            .is_some_and(|x| x.state == SessionState::Running && x.cli_session_id.is_some())
    }));
}

#[test]
fn runtime_decisions_are_logged_at_info() {
    let buffer = Buffer::default();
    let out = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || out.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let hang = AgentScript {
            outcome: AgentOutcome::Hang,
            ask: Some(("Bash".into(), json!({ "command": "npm install" }))),
            ..AgentScript::default()
        };
        let mut a = sim(hang.clone(), |_| {});
        assert!(a.run_until(STEPS, |s| !s.task(1).permissions.is_empty()));
        let engine = a.engine();
        let cause = engine.cause("human:operator:local");
        let request = a.task(1).permissions[0].id;
        engine
            .answer_permission_with(request, true, None, true, &cause, None)
            .unwrap();
        let rule = engine.permission_rules(a.project_id).unwrap().unwrap()[0].id;
        engine.revoke_rule(rule, "too broad", &cause, None).unwrap();
        running(&mut a);
        let sid = a.task(1).session.unwrap().id;
        let command = |c| engine.session_command(sid, c, &cause, None).unwrap();
        command(SessionCommand::Steer {
            text: "keep going".into(),
        });
        command(SessionCommand::Interrupt);
        a.restart();
        assert!(a.run_until(STEPS, |s| s.stats().reattaches >= 1));
        let engine = a.engine();
        let cause = engine.cause("human:operator:local");
        engine
            .session_command(
                sid,
                SessionCommand::Stop {
                    reason: "wrong file".into(),
                },
                &cause,
                None,
            )
            .ok();

        let mut b = sim(
            AgentScript {
                outcome: AgentOutcome::Hang,
                ..AgentScript::default()
            },
            |c| {
                c.loop_cfg.budgets.insert(
                    Stage::Dev,
                    Budget {
                        running_minutes: 10,
                        turns: 10,
                        tokens: 0,
                        cost_usd_micros: 0,
                    },
                );
            },
        );
        running(&mut b);
        let first = b.task(1).session.unwrap().id;
        b.world.kill_runner(first);
        assert!(b.run_until(STEPS, |s| s.sessions(1).len() == 2));
        assert!(b.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));

        let mut c = sim(hang, |c| c.loop_cfg.permission_wait_ms = 600_000);
        assert!(c.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));
    });

    let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|l| l["level"] == "INFO")
        .collect();
    let messages: Vec<&str> = lines
        .iter()
        .filter_map(|l| l["fields"]["message"].as_str())
        .collect();
    for wanted in [
        "permission rule added",
        "permission rule revoked",
        "session steered",
        "session interrupted",
        "session reattached",
        "session stopped",
        "session resumed",
        "budget warning",
        "budget exhausted",
        "permission request expired",
    ] {
        assert!(
            messages.contains(&wanted),
            "{wanted} not logged: {messages:?}"
        );
    }
    let exhausted = lines
        .iter()
        .find(|l| l["fields"]["message"] == "budget exhausted")
        .unwrap();
    assert_eq!(exhausted["fields"]["dimension"], "running_minutes");
}
