#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::secret::SecretText;
use saka_core::vocab::{BlockerCode, WorkItemState};
use saka_engine::Cause;
use saka_testkit::{Operator, Sim, SimConfig};
use serde_json::json;

const STEPS: usize = 8_000;

const RECIPE: &str = r#"[setup]
commands = ["pnpm install --frozen-lockfile"]

[secrets]
NPM_TOKEN = {}
TEST_API_KEY = { to = ["setup", "session"] }
"#;

fn sim(retry: &[BlockerCode]) -> Sim {
    let mut operator = Operator::default();
    operator.retry.extend_from_slice(retry);
    Sim::new(SimConfig {
        operator,
        ..SimConfig::default()
    })
}

fn set(sim: &Sim, name: &str, value: &str) {
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

#[test]
fn each_secret_reaches_only_the_purposes_the_recipe_declares() {
    let mut sim = sim(&[]);
    sim.world.set_recipe(Some(RECIPE));
    set(&sim, "NPM_TOKEN", "npm-token-abcdef");
    set(&sim, "TEST_API_KEY", "test-api-key-123");
    set(&sim, "UNUSED_TOKEN", "unused-token-456");
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));

    let setup = sim.world.recipe_runs();
    assert!(!setup.is_empty());
    for run in &setup {
        assert_eq!(run.secrets["NPM_TOKEN"], "npm-token-abcdef");
        assert_eq!(run.secrets["TEST_API_KEY"], "test-api-key-123");
        assert!(!run.secrets.contains_key("UNUSED_TOKEN"));
        assert!(!run.vars.values().any(|v| v.contains("token")));
        assert_eq!(run.redacted, ["NPM_TOKEN", "TEST_API_KEY"]);
    }
    let launches = sim.world.launches();
    assert!(!launches.is_empty());
    for launch in &launches {
        assert_eq!(
            launch.secrets.keys().collect::<Vec<_>>(),
            ["TEST_API_KEY"],
            "a setup-only secret never reaches the session"
        );
        assert_eq!(launch.redacted, ["NPM_TOKEN", "TEST_API_KEY"]);
    }
    let env = sim.task(1).environment.unwrap();
    let recipe = env.recipe.unwrap();
    assert_eq!(
        recipe["secrets"],
        json!({ "NPM_TOKEN": ["setup"], "TEST_API_KEY": ["setup", "session"] })
    );
    let events = sim.engine().events_after(0, 10_000).unwrap();
    for e in &events {
        let text = e.payload.to_string();
        assert!(
            !text.contains("npm-token-abcdef") && !text.contains("test-api-key-123"),
            "{} carries a value",
            e.kind
        );
    }
}

#[test]
fn a_missing_secret_blocks_before_any_command_and_ships_once_it_is_set() {
    let mut sim = sim(&[BlockerCode::SecretMissing]);
    sim.world.set_recipe(Some(RECIPE));
    set(&sim, "TEST_API_KEY", "test-api-key-123");
    sim.approve();

    assert!(sim.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));
    let blocker = sim.task(1).open_blocker().unwrap().clone();
    assert_eq!(blocker.code, BlockerCode::SecretMissing);
    assert_eq!(blocker.params, json!({ "names": ["NPM_TOKEN"] }));
    assert!(sim.world.recipe_runs().is_empty());
    assert!(sim.world.toolchain_installs().is_empty());
    assert_eq!(sim.stats().teardowns, 1);

    set(&sim, "NPM_TOKEN", "npm-token-abcdef");
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.task(1).item.state, WorkItemState::Done);
}

#[test]
fn a_secret_that_cannot_be_opened_blocks_with_its_name() {
    let mut sim = sim(&[]);
    sim.world.set_recipe(Some(RECIPE));
    set(&sim, "NPM_TOKEN", "npm-token-abcdef");
    set(&sim, "TEST_API_KEY", "test-api-key-123");
    sim.world.break_secret_key(true);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));
    let blocker = sim.task(1).open_blocker().unwrap().clone();
    assert_eq!(blocker.code, BlockerCode::SecretUnreadable);
    assert_eq!(blocker.params, json!({ "name": "NPM_TOKEN" }));
    assert!(sim.world.recipe_runs().is_empty());
}

#[test]
fn a_recipe_without_secrets_injects_none() {
    let mut sim = sim(&[]);
    set(&sim, "NPM_TOKEN", "npm-token-abcdef");
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    for launch in sim.world.launches() {
        assert!(launch.secrets.is_empty() && launch.redacted.is_empty());
    }
}
