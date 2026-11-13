#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{BlockerCode, WorkItemState};
use saka_ports::PortError;
use saka_store::read;
use saka_testkit::{
    AgentOutcome, DOCS_REPO, Layout, SHOP_REPO, Sim, SimConfig, repo_entry, saka_toml,
};
use serde_json::{Value, json};

const STEPS: usize = 5_000;

fn layout(repo: (&str, &str)) -> Layout {
    Layout {
        saka_toml: saka_toml("shop", &[repo_entry(repo.0, repo.1, "")]),
        ..Layout::default()
    }
}

fn one_at_a_time(outcome: AgentOutcome) -> SimConfig {
    let mut cfg = SimConfig {
        max_active_tasks: 1,
        ..SimConfig::default()
    };
    cfg.agent.outcome = outcome;
    cfg
}

fn source_commit(sim: &Sim) -> String {
    sim.engine()
        .store()
        .read(|c| read::load_revision(c, sim.revision_id))
        .unwrap()
        .unwrap()
        .source_commit
}

fn started(repo: (&str, &str), outcome: AgentOutcome) -> Sim {
    let mut sim = Sim::multi_repo(one_at_a_time(outcome), &layout(repo));
    sim.world
        .set_recipe(Some("[setup]\ncommands = [\"make deps\"]\n"));
    sim.approve();
    assert!(sim.run_until(STEPS, |s| !s.world.prompts().is_empty()));
    sim
}

fn docs_line(text: &str) -> Option<String> {
    text.lines()
        .find(|l| l.starts_with("- Project docs"))
        .map(String::from)
}

#[test]
fn a_task_in_a_code_repo_gets_a_writable_docs_branch_at_the_spec_commit() {
    let sim = started(("shop", SHOP_REPO), AgentOutcome::Hang);
    let commit = source_commit(&sim);
    sim.world.put_doc("docs/later.md", b"after the pin\n");
    let task = sim.task(1);
    let env = task.environment.clone().unwrap();
    let path = format!("/sim/envs/{}/docs/docs", env.id);

    let prepared = sim.events_of("runtime.environment.prepared");
    assert_eq!(prepared.len(), 1);
    let docs = &prepared[0].payload["docs"];
    assert_eq!(
        *docs,
        json!({
            "repo_id": sim.project().project.workspace_repo_id,
            "full_name": DOCS_REPO,
            "commit": commit,
            "project_path": "docs",
            "path": path,
            "mode": "writable",
            "branch": "saka/tasks-done/s1-docs",
            "base_sha": commit,
            "pushed_sha": null,
        })
    );
    let checkout = &sim.world.docs_checkouts()[&env.id];
    assert_eq!(
        (checkout.repo.as_str(), checkout.commit.as_str()),
        (DOCS_REPO, commit.as_str())
    );
    assert_eq!(env.docs.as_ref().unwrap().path, path);

    let sent = sim.world.prompts()[0].clone();
    assert_eq!(
        docs_line(&sent).unwrap(),
        format!(
            "- Project docs: {path}, a checkout of {DOCS_REPO} on branch saka/tasks-done/s1-docs. Edit docs only under docs/ there and commit them in that checkout; Saka opens a separate docs pull request after the code pull request."
        )
    );
    assert!(sent.contains("- Commit docs changes only in the project docs checkout.\n"));
    let (_, packet) = sim.engine().work_packet(task.item.id).unwrap();
    assert_eq!(packet.text, sent, "the packet is the one turn 1 sent");

    let launch = sim.world.launches()[0].clone();
    assert_eq!(launch.extra_dirs, std::slice::from_ref(&path));
    assert_eq!(launch.vars["SAKA_DOCS_DIR"], path);
    let setup = sim.world.recipe_runs();
    assert_eq!(setup[0].vars["SAKA_DOCS_DIR"], path);
}

#[test]
fn a_task_in_the_workspace_repo_gets_no_docs_checkout_and_the_same_packet() {
    let sim = started(("docs", DOCS_REPO), AgentOutcome::Hang);
    let prepared = sim.events_of("runtime.environment.prepared");
    assert_eq!(prepared[0].payload["docs"], Value::Null);
    assert!(sim.world.docs_checkouts().is_empty());
    let sent = sim.world.prompts()[0].clone();
    assert_eq!(docs_line(&sent), None);
    assert!(sent.contains("may wait.\n\nRules:\n- Work only inside this repository"));
    let launch = sim.world.launches()[0].clone();
    assert!(launch.extra_dirs.is_empty());
    assert!(!launch.vars.contains_key("SAKA_DOCS_DIR"));
}

#[test]
fn the_docs_checkout_is_torn_down_with_the_environment_when_the_task_is_done() {
    let mut sim = Sim::multi_repo(
        one_at_a_time(AgentOutcome::Commit),
        &layout(("shop", SHOP_REPO)),
    );
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    let checkouts = sim.world.docs_checkouts();
    assert_eq!(checkouts.len(), 2, "one per step");
    assert!(checkouts.values().all(|c| c.torn_down));
    assert!(
        sim.tasks()
            .iter()
            .all(|t| t.item.state == WorkItemState::Done)
    );
}

fn blocked_on_docs(refusal: Option<PortError>) -> (Sim, Value) {
    let mut sim = Sim::multi_repo(
        one_at_a_time(AgentOutcome::Commit),
        &layout(("shop", SHOP_REPO)),
    );
    sim.world.refuse_docs_checkouts(refusal);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).open_blocker().is_some()));
    let blocker = sim.task(1).open_blocker().unwrap().clone();
    assert_eq!(blocker.code, BlockerCode::EnvSetupFailed);
    (sim, blocker.params)
}

#[test]
fn a_docs_folder_too_large_or_a_missing_commit_blocks_without_an_automatic_retry() {
    let too_large = PortError::permanent(
        "docs_checkout_too_large",
        json!({ "files": 20_001, "bytes": 10, "max_files": 20_000, "max_bytes": 268_435_456 }),
    );
    let (mut sim, params) = blocked_on_docs(Some(too_large));
    assert_eq!(params["error"]["code"], "docs_checkout_too_large");
    assert_eq!(params["error"]["params"]["files"], 20_001);
    let env_id = saka_core::ids::EnvironmentId(sim.task(1).stage_run.unwrap().id.0);
    assert!(
        !sim.world.worktree_live(env_id),
        "the task worktree is removed"
    );
    assert!(
        sim.world.recipe_reads().is_empty(),
        "nothing after the docs"
    );
    assert!(sim.world.prompts().is_empty());
    let until = saka_ports::Clock::now(&*sim.clock).0 + 3_600_000;
    assert!(sim.run_until(STEPS, |s| saka_ports::Clock::now(&*s.clock).0 >= until));
    assert_eq!(
        sim.task(1).open_blocker().map(|b| b.code),
        Some(BlockerCode::EnvSetupFailed),
        "not retried on its own"
    );

    let missing = PortError::permanent("docs_commit_missing", json!({ "commit": "f".repeat(40) }));
    let (_, params) = blocked_on_docs(Some(missing));
    assert_eq!(params["error"]["code"], "docs_commit_missing");
    assert_eq!(params["error"]["params"]["commit"], "f".repeat(40));
}
