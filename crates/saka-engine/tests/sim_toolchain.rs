#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{BlockerCode, Tool, WorkItemState};
use saka_testkit::{Operator, Sim, SimConfig, ToolchainScript};
use serde_json::{Value, json};

const STEPS: usize = 8_000;

const GO_MOD: &str = "module example.com/app\n\ngo 1.26\n\ntoolchain go1.26.3\n";
const PACKAGE_JSON: &str = r#"{
  "name": "app",
  "packageManager": "pnpm@12.16.1+sha512.abc",
  "devEngines": { "runtime": { "name": "node", "version": "26.13.0", "onFail": "download" } }
}"#;
const RECIPE: &str = "[setup]\ncommands = [\"pnpm install --frozen-lockfile\"]\n";

fn sim(retry: &[BlockerCode]) -> Sim {
    let mut operator = Operator::default();
    operator.retry.extend_from_slice(retry);
    Sim::new(SimConfig {
        operator,
        ..SimConfig::default()
    })
}

fn bins() -> Vec<String> {
    vec![
        "/sim/toolchains/go/1.26.3/bin".into(),
        "/sim/toolchains/node/26.13.0/bin".into(),
        "/sim/toolchains/pnpm/12.16.1/bin".into(),
    ]
}

fn prepared_events(sim: &Sim) -> Vec<Value> {
    sim.engine()
        .events_after(0, 10_000)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "runtime.environment.prepared")
        .map(|e| e.payload)
        .collect()
}

#[test]
fn declared_toolchains_lead_path_for_setup_and_session_and_are_installed_once() {
    let mut sim = sim(&[]);
    sim.world.set_repo_file("go.mod", Some(GO_MOD));
    sim.world.set_repo_file("package.json", Some(PACKAGE_JSON));
    sim.world.set_recipe(Some(RECIPE));
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));

    let setup = sim.world.recipe_runs();
    assert!(!setup.is_empty());
    assert!(setup.iter().all(|r| r.path_prefix == bins()), "{setup:?}");
    let launches = sim.world.launches();
    assert!(!launches.is_empty());
    for launch in &launches {
        assert_eq!(launch.path_prefix, bins());
        assert_eq!(launch.vars["GOTOOLCHAIN"], "local");
        assert_eq!(launch.vars["GOMODCACHE"], "/sim/caches/go/mod");
        assert_eq!(
            launch.vars["npm_config_store_dir"],
            "/sim/caches/pnpm-store"
        );
    }
    assert_eq!(setup[0].vars["GOTOOLCHAIN"], "local");

    let installs = sim.world.toolchain_installs();
    let downloads: Vec<_> = installs.iter().filter(|(_, _, d)| *d).collect();
    assert_eq!(downloads.len(), 3, "one download per tool: {installs:?}");
    assert_eq!(installs.len(), 6, "both steps ask for all three");

    let env = sim.task(1).environment.unwrap();
    let tools: Vec<(Tool, &str)> = env
        .toolchains
        .iter()
        .map(|t| (t.tool, t.version.as_str()))
        .collect();
    assert_eq!(
        tools,
        [
            (Tool::Go, "1.26.3"),
            (Tool::Node, "26.13.0"),
            (Tool::Pnpm, "12.16.1")
        ]
    );
    let events = prepared_events(&sim);
    assert_eq!(events[0]["toolchains"][0]["tool"], "go");
    assert_eq!(events[0]["toolchains"][0]["downloaded"], json!(true));
    assert_eq!(events[1]["toolchains"][0]["downloaded"], json!(false));
}

#[test]
fn a_repo_without_declarations_keeps_path_and_installs_nothing() {
    let mut sim = sim(&[]);
    sim.world
        .set_repo_file("go.mod", Some("module example.com/app\n"));
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    assert!(sim.world.toolchain_installs().is_empty());
    for launch in sim.world.launches() {
        assert!(launch.path_prefix.is_empty());
        assert!(!launch.vars.contains_key("GOTOOLCHAIN"));
    }
    assert!(sim.task(1).environment.unwrap().toolchains.is_empty());
}

#[test]
fn the_recipe_pins_a_version_over_the_repo_files() {
    let mut sim = sim(&[]);
    sim.world.set_repo_file("go.mod", Some(GO_MOD));
    sim.world.set_recipe(Some("[toolchain]\ngo = \"1.27.0\"\n"));
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(
        sim.world.launches()[0].path_prefix,
        ["/sim/toolchains/go/1.27.0/bin"]
    );
}

#[test]
fn an_unpinned_version_blocks_before_any_command_and_a_retry_after_the_fix_ships() {
    let mut sim = sim(&[BlockerCode::ToolchainUnpinned]);
    sim.world.set_repo_file(".nvmrc", Some("lts/*\n"));
    sim.world.set_recipe(Some(RECIPE));
    sim.approve();

    assert!(sim.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));
    let blocker = sim.task(1).open_blocker().unwrap().clone();
    assert_eq!(blocker.code, BlockerCode::ToolchainUnpinned);
    assert_eq!(
        blocker.params,
        json!({ "tool": "node", "source": "nvmrc", "value": "lts/*" })
    );
    assert!(sim.world.recipe_runs().is_empty());
    assert!(sim.world.toolchain_installs().is_empty());
    assert_eq!(sim.stats().teardowns, 1);

    sim.world.set_repo_file(".nvmrc", Some("26.13.0\n"));
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.task(1).item.state, WorkItemState::Done);
}

#[test]
fn a_missing_release_blocks_and_a_flaky_download_is_retried() {
    let mut sim = sim(&[]);
    sim.world.set_repo_file(".node-version", Some("26.13.0\n"));
    sim.world
        .script_toolchain(Tool::Node, ToolchainScript::DownloadFails);
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    assert!(sim.task(1).open_blocker().is_none());

    let mut sim = self::sim(&[]);
    sim.world.set_repo_file(".node-version", Some("26.13.0\n"));
    sim.world
        .script_toolchain(Tool::Node, ToolchainScript::Missing);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));
    let blocker = sim.task(1).open_blocker().unwrap().clone();
    assert_eq!(blocker.code, BlockerCode::ToolchainUnavailable);
    assert_eq!(blocker.params["cause"], "not_found");
    assert_eq!(blocker.params["version"], "26.13.0");
    assert_eq!(sim.stats().teardowns, 1);
}

#[test]
fn a_download_that_keeps_failing_ends_as_toolchain_unavailable() {
    let mut sim = sim(&[]);
    sim.world.set_repo_file(".node-version", Some("26.13.0\n"));
    for _ in 0..3 {
        sim.world
            .script_toolchain(Tool::Node, ToolchainScript::DownloadFails);
    }
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));
    let blocker = sim.task(1).open_blocker().unwrap().clone();
    assert_eq!(blocker.code, BlockerCode::ToolchainUnavailable);
    assert_eq!(blocker.params["cause"], "download_failed");
    assert_eq!(blocker.params["tool"], "node");
}
