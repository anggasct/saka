#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::EnvironmentState;
use saka_testkit::{Sim, SimConfig};

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

#[test]
fn an_exhausted_job_logs_the_params_it_stored() {
    let mut sim = Sim::new(SimConfig::default());
    sim.world.refuse_teardowns(true);
    sim.approve();
    let buffer = Buffer::default();
    let out = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_writer(move || out.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        assert!(sim.run_until(STEPS, |s| {
            s.task(1).environment.map(|e| e.state) == Some(EnvironmentState::TeardownFailed)
        }));
    });

    let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    let exhausted: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|l| {
            l["fields"]["message"] == "job exhausted" && l["fields"]["kind"] == "env.teardown"
        })
        .map(|l| l["fields"].clone())
        .collect();
    assert_eq!(exhausted.len(), 1, "{exhausted:?}");
    let logged: serde_json::Value =
        serde_json::from_str(exhausted[0]["params"].as_str().unwrap()).unwrap();
    assert_eq!(exhausted[0]["code"], "env_io_failed");
    assert_eq!(logged["op"], "remove_env_root");

    let stored: String = sim
        .engine()
        .store()
        .read(|c| {
            Ok(c.query_row(
                "SELECT last_error FROM job WHERE kind = 'env.teardown' AND state = 'dead'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let stored: serde_json::Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["params"], logged);
}
