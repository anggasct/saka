#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test fails loudly"
)]

use std::sync::Arc;

use saka_core::decision::{DecisionOption, Question};
use saka_core::ids::SessionId;
use saka_core::secret::SecretText;
use saka_core::vocab::SessionState;
use saka_engine::{Cause, InboxFilter, Page};
use saka_mcp::{Client, Tokens};
use saka_testkit::{AgentOutcome, AgentScript, CiScript, Sim, SimConfig};

const TOKEN: &str = "decision-secrets-token-0123456789";
const STEPS: usize = 8_000;
const FIXTURE: &str = "fixture-secret-value-7f3a91c2";
const OTHER: &str = "other-fixture-secret-b81d4e60";

struct Fixed;

impl Tokens for Fixed {
    fn token(&self, _session: SessionId) -> Option<SecretText> {
        Some(SecretText::new(TOKEN.to_string()))
    }
}

fn set_secret(sim: &Sim, name: &str, value: &str) {
    let cause = Cause {
        actor: "operator".into(),
        causation_id: None,
        correlation_id: uuid::Uuid::from_u128(42),
    };
    sim.engine()
        .set_secret(
            sim.project_id,
            name,
            &SecretText::new(value.into()),
            &cause,
            None,
        )
        .unwrap();
}

fn running(sim: &Sim) -> bool {
    sim.task(1)
        .session
        .as_ref()
        .is_some_and(|s| s.state == SessionState::Running)
}

#[test]
fn a_project_secret_pasted_into_a_question_never_reaches_the_store_the_events_or_the_inbox() {
    let mut sim = Sim::new(SimConfig {
        ci: CiScript::Pass,
        agent: AgentScript {
            outcome: AgentOutcome::Hang,
            ..AgentScript::default()
        },
        ..SimConfig::default()
    });
    set_secret(&sim, "STORAGE_KEY", FIXTURE);
    set_secret(&sim, "OTHER_KEY", OTHER);
    sim.approve();
    assert!(sim.run_until(STEPS, running));
    let session = sim.task(1).session.unwrap().id;

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run").join("mcp.sock");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let listener = {
        let _guard = rt.enter();
        saka_mcp::bind(&socket).unwrap()
    };
    let decisions: Arc<dyn saka_ports::Decisions> = sim.engine();
    rt.spawn(saka_mcp::serve(listener, Arc::new(Fixed), decisions));

    let option = |label: String| DecisionOption {
        label,
        detail: Some(format!("detail {OTHER}")),
    };
    let question = Question {
        question: format!("Upload with {FIXTURE}?"),
        options: vec![option(format!("Use {FIXTURE}")), option("R2".into())],
        context: Some(format!("The key is {FIXTURE}")),
    };
    let client = Client::new(socket, session, TOKEN.into());
    rt.block_on(client.ask(question)).unwrap();

    let stored = sim.task(1).decisions[0].clone();
    assert_eq!(stored.question, "Upload with «secret:STORAGE_KEY»?");
    assert_eq!(
        stored.context.as_deref(),
        Some("The key is «secret:STORAGE_KEY»")
    );
    assert_eq!(stored.options[0].label, "Use «secret:STORAGE_KEY»");
    assert_eq!(
        stored.options[0].detail.as_deref(),
        Some("detail «secret:OTHER_KEY»")
    );

    let row: String = sim
        .engine()
        .store()
        .read(|c| {
            Ok(c.query_row(
                "SELECT question || context || options FROM decision_request",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let all = sim.engine().events_after(0, 10_000).unwrap();
    let asked: Vec<_> = all
        .iter()
        .filter(|e| e.kind == "runtime.decision.asked")
        .collect();
    assert_eq!(asked.len(), 1);
    assert!(
        asked[0]
            .payload
            .to_string()
            .contains("«secret:STORAGE_KEY»")
    );
    let events: String = all.iter().map(|e| e.payload.to_string()).collect();
    let inbox = format!(
        "{:?}",
        sim.engine()
            .inbox(
                &InboxFilter::default(),
                &Page {
                    after: None,
                    limit: 100
                }
            )
            .unwrap()
    );
    for (place, text) in [("row", &row), ("events", &events), ("inbox", &inbox)] {
        assert!(!text.contains(FIXTURE), "{place} leaked the secret");
        assert!(!text.contains(OTHER), "{place} leaked the other secret");
    }
}
