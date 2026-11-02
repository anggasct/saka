#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use proptest::prelude::*;
use saka_core::admission::Waiting;
use saka_core::vocab::{BlockerCode, EnvironmentState, SpecState, WorkItemState};
use saka_engine::EnvConfig;
use saka_testkit::{
    AFTER_GRAPH_PATH, AFTER_GRAPH_SPEC, CommandScript, Operator, RecipeRun, Sim, SimConfig,
};
use serde_json::{Value, json};

const STEPS: usize = 8_000;

const RECIPE: &str = "ports = 3

[env]
APP_ENV = \"test\"

[setup]
commands = [\"make deps\", \"make db-up\", \"make seed\"]

[teardown]
commands = [\"make db-down\"]
";

fn sim_with(operator: Operator) -> Sim {
    let sim = Sim::new(SimConfig {
        operator,
        ..SimConfig::default()
    });
    sim.world.set_recipe(Some(RECIPE));
    sim
}

fn runs_of(sim: &Sim, step: u32, phase: &str) -> Vec<RecipeRun> {
    let Some(env) = sim.task(step).environment else {
        return Vec::new();
    };
    sim.world
        .recipe_runs()
        .into_iter()
        .filter(|r| r.environment_id == env.id && r.phase == phase)
        .collect()
}

fn commands(runs: &[RecipeRun]) -> Vec<&str> {
    runs.iter().map(|r| r.command.as_str()).collect()
}

fn events(sim: &Sim, kind: &str) -> Vec<Value> {
    sim.engine()
        .events_after(0, 10_000)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind)
        .map(|e| e.payload)
        .collect()
}

#[test]
fn setup_runs_in_order_and_the_session_gets_the_same_ports_and_env() {
    let mut sim = sim_with(Operator::default());
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));

    let env = sim.task(1).environment.unwrap();
    let start = env.port_start.unwrap();
    assert_eq!(env.port_count, Some(10));
    assert_eq!(
        env.recipe_sha256.as_deref(),
        Some(saka_engine::recipe::sha256(RECIPE).as_str())
    );
    let setup = runs_of(&sim, 1, "setup");
    assert_eq!(commands(&setup), ["make deps", "make db-up", "make seed"]);
    let vars = &setup[0].vars;
    assert_eq!(vars["APP_ENV"], "test");
    assert_eq!(vars["SAKA_PORT_BASE"], start.to_string());
    assert_eq!(vars["SAKA_PORT_COUNT"], "3");
    assert_eq!(vars["PORT"], start.to_string());
    assert_eq!(vars["SAKA_ENVIRONMENT_ID"], env.id.to_string());
    assert_eq!(vars["SAKA_TASK_KEY"], sim.task(1).item.task_key);

    let launched = sim.world.launch_vars();
    assert!(launched.iter().any(|l| l == vars), "{launched:?}");
    assert_eq!(commands(&runs_of(&sim, 1, "teardown")), ["make db-down"]);

    let prepared = events(&sim, "runtime.environment.prepared");
    let first = &prepared[0];
    assert_eq!(first["port_start"], json!(start));
    assert_eq!(first["setup"].as_array().unwrap().len(), 3);
    assert_eq!(first["setup"][0]["exit_code"], json!(0));
}

#[test]
fn a_failed_setup_command_stops_setup_tears_down_and_blocks_until_retried() {
    let mut sim = sim_with(Operator {
        retry: vec![BlockerCode::SessionLost, BlockerCode::EnvSetupFailed],
        ..Operator::default()
    });
    sim.world
        .script_command("make db-up", CommandScript::Exit(2));
    sim.approve();

    assert!(sim.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));
    let task = sim.task(1);
    let blocker = task.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::EnvSetupFailed);
    assert_eq!(blocker.params["error"]["code"], "setup_command_failed");
    let failed = &blocker.params["error"]["params"];
    assert_eq!(failed["command_index"], json!(1));
    assert_eq!(failed["exit_code"], json!(2));
    assert!(failed["log"].as_str().unwrap().ends_with("/setup-1.log"));
    assert!(task.environment.is_none());

    let runs = sim.world.recipe_runs();
    assert_eq!(
        commands(&runs),
        ["make deps", "make db-up", "make db-down"],
        "the third setup command must not run; teardown must"
    );
    assert_eq!(sim.stats().teardowns, 1);

    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.task(1).item.state, WorkItemState::Done);
    assert_eq!(
        commands(&runs_of(&sim, 1, "setup")),
        ["make deps", "make db-up", "make seed"]
    );
}

#[test]
fn a_crash_in_the_middle_of_setup_reruns_setup_from_the_start_in_the_same_environment() {
    let mut sim = sim_with(Operator::default());
    sim.world.script_command("make db-up", CommandScript::Crash);
    sim.approve();

    assert!(sim.run_to_shipped(STEPS));
    assert!(sim.task(1).open_blocker().is_none());
    let env = sim.task(1).environment.unwrap().id;
    let setup: Vec<_> = sim
        .world
        .recipe_runs()
        .into_iter()
        .filter(|r| r.phase == "setup" && r.environment_id == env)
        .collect();
    assert_eq!(
        commands(&setup),
        ["make deps", "make deps", "make db-up", "make seed"]
    );
    assert_eq!(sim.stats().prepares, 2, "one worktree per step");
}

#[test]
fn an_invalid_recipe_blocks_before_any_command_and_names_the_field() {
    let mut sim = sim_with(Operator::default());
    sim.world.set_recipe(Some(
        "[env]\nPATH = \"/opt\"\n\n[setup]\ncommands = [\"x\"]\n",
    ));
    sim.approve();

    assert!(sim.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));
    let blocker = sim.task(1).open_blocker().unwrap().clone();
    assert_eq!(blocker.code, BlockerCode::EnvRecipeInvalid);
    assert_eq!(blocker.params["code"], "env_name_reserved");
    assert_eq!(blocker.params["field"], "env.PATH");
    assert_eq!(blocker.params["line"], json!(2));
    assert!(sim.world.recipe_runs().is_empty());
    assert_eq!(sim.stats().teardowns, 1);
}

#[test]
fn teardown_runs_the_recipe_stored_at_setup_and_records_failures() {
    let mut sim = sim_with(Operator::default());
    sim.world
        .script_command("make db-down", CommandScript::Exit(1));
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .environment
            .is_some_and(|e| e.state == EnvironmentState::Ready)
    }));
    sim.world
        .set_recipe(Some("[teardown]\ncommands = [\"make other-down\"]\n"));

    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .environment
            .is_some_and(|e| e.state == EnvironmentState::TornDown)
    }));
    assert_eq!(commands(&runs_of(&sim, 1, "teardown")), ["make db-down"]);
    let env = sim.task(1).environment.unwrap().id;
    let torn = events(&sim, "runtime.environment.torn_down");
    let mine = torn
        .iter()
        .find(|e| e["environment_id"] == json!(env))
        .unwrap();
    assert_eq!(
        mine["teardown_failures"],
        json!([{ "command_index": 0, "exit_code": 1 }])
    );
    assert!(sim.run_to_shipped(STEPS));
}

#[test]
fn a_repo_without_a_recipe_still_gets_a_port_block_and_no_commands() {
    let mut sim = sim_with(Operator::default());
    sim.world.set_recipe(None);
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    let env = sim.task(1).environment.unwrap();
    assert!(env.port_start.is_some());
    assert!(env.recipe.is_none() && env.recipe_sha256.is_none());
    assert!(sim.world.recipe_runs().iter().all(|r| r.phase == "gate"));
    let vars = &sim.world.launch_vars()[0];
    assert_eq!(vars["SAKA_PORT_COUNT"], "10");
}

#[test]
fn parallel_steps_never_share_a_port_block_and_wait_when_none_is_free() {
    let mut sim = Sim::new(SimConfig {
        max_active_tasks: 2,
        env: EnvConfig {
            port_first: 41000,
            port_last: 41009,
            ports_per_env: 10,
        },
        ..SimConfig::default()
    });
    let spec = sim.register(AFTER_GRAPH_PATH, AFTER_GRAPH_SPEC);
    sim.approve_spec(spec.spec_id, spec.revision_id).unwrap();

    let mut waited = false;
    let mut overlap = false;
    let shipped = sim.run_until(STEPS, |s| {
        let tasks = s.tasks_of(spec.spec_id);
        let live: Vec<u16> = tasks
            .iter()
            .filter_map(|t| t.environment.as_ref())
            .filter(|e| {
                matches!(
                    e.state,
                    EnvironmentState::Preparing
                        | EnvironmentState::Ready
                        | EnvironmentState::TeardownFailed
                )
            })
            .filter_map(|e| e.port_start)
            .collect();
        if live.len() > 1 {
            overlap = true;
        }
        for t in &tasks {
            let detail = s.engine().work_item(t.item.id).unwrap().unwrap();
            if detail.waiting_on.contains(&Waiting::PortBlock) {
                waited = true;
            }
        }
        s.spec_of(spec.spec_id).state == SpecState::Shipped
    });
    assert!(shipped, "not shipped");
    assert!(!overlap, "two live environments held the only block");
    assert!(waited, "no task waited on a port block");
}

fn live_port_starts(sim: &Sim, spec: saka_core::ids::SpecId) -> Vec<u16> {
    sim.tasks_of(spec)
        .iter()
        .filter_map(|t| t.environment.as_ref())
        .filter(|e| {
            matches!(
                e.state,
                EnvironmentState::Preparing
                    | EnvironmentState::Ready
                    | EnvironmentState::TeardownFailed
            )
        })
        .filter_map(|e| e.port_start)
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn live_environments_never_overlap_under_random_capacity_and_crashes(
        blocks in 1u16..=3,
        max_active in 1usize..=3,
        raw in prop::collection::vec((0usize..600, any::<bool>()), 0..3),
    ) {
        let mut sim = Sim::new(SimConfig {
            max_active_tasks: max_active,
            env: EnvConfig {
                port_first: 41000,
                port_last: 41000 + blocks * 10 - 1,
                ports_per_env: 10,
            },
            ..SimConfig::default()
        });
        sim.world.set_recipe(Some(RECIPE));
        let spec = sim.register(AFTER_GRAPH_PATH, AFTER_GRAPH_SPEC);
        sim.approve_spec(spec.spec_id, spec.revision_id).unwrap();
        let mut points = raw;
        points.sort_unstable();
        let mut next = points.into_iter().peekable();
        let mut steps = 0;
        while sim.spec_of(spec.spec_id).state != SpecState::Shipped {
            while next.peek().is_some_and(|(at, _)| *at == steps) {
                let (_, mid_job) = next.next().unwrap();
                if !(mid_job && sim.crash_mid_job()) {
                    sim.restart();
                }
            }
            sim.operate();
            sim.step();
            steps += 1;
            let mut live = live_port_starts(&sim, spec.spec_id);
            prop_assert!(live.iter().all(|s| (*s - 41000) % 10 == 0 && *s < 41000 + blocks * 10), "{live:?}");
            let held = live.len();
            live.dedup();
            prop_assert_eq!(live.len(), held, "two live environments share a block");
            prop_assert!(held <= usize::from(blocks));
            prop_assert!(steps < STEPS, "stuck: {:?}", sim.tasks_of(spec.spec_id).iter().map(|t| (t.item.state, t.open_blocker().map(|b| b.code))).collect::<Vec<_>>());
        }
        for t in sim.tasks_of(spec.spec_id) {
            prop_assert_eq!(t.item.state, WorkItemState::Done);
        }
    }
}
