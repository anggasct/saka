#![allow(
    clippy::unwrap_used,
    reason = "simulations fail loudly on a broken setup"
)]

use saka_core::policy::{Effect, RuleSpec};
use saka_engine::Page;
use saka_testkit::{AgentScript, Operator, Sim, SimConfig};
use serde_json::json;

const STEPS: usize = 8_000;

fn allow_bash(id: &str, command: &str) -> RuleSpec {
    RuleSpec {
        id: id.into(),
        effect: Effect::Allow,
        tools: vec!["Bash".into()],
        paths: vec![],
        commands: vec![command.into()],
    }
}

fn asking(command: &str) -> AgentScript {
    AgentScript {
        ask: Some(("Bash".into(), json!({ "command": command }))),
        ..AgentScript::default()
    }
}

fn sim(agent: AgentScript, rules: Vec<RuleSpec>) -> Sim {
    let mut sim = Sim::new(SimConfig {
        agent,
        rules,
        operator: Operator {
            start: true,
            merge: true,
            retry: vec![],
            answer: None,
        },
        ..SimConfig::default()
    });
    sim.approve();
    sim
}

fn answer_sources(sim: &Sim) -> Vec<Option<String>> {
    sim.engine()
        .permission_requests(
            &Default::default(),
            &Page {
                after: None,
                limit: 100,
            },
        )
        .unwrap()
        .items
        .into_iter()
        .map(|p| p.request.answer_source)
        .collect()
}

#[test]
fn a_compound_command_whose_parts_are_all_allowed_never_reaches_the_operator() {
    let mut sim = sim(
        asking("git status && ls"),
        vec![allow_bash("status", "git status"), allow_bash("list", "ls")],
    );
    assert!(sim.run_to_shipped(STEPS));
    let sources = answer_sources(&sim);
    assert!(!sources.is_empty());
    assert!(
        sources
            .iter()
            .all(|s| s.as_deref().is_some_and(|s| s.starts_with("policy:"))),
        "{sources:?}"
    );
    assert!(sources.contains(&Some("policy:status".to_string())));
}

#[test]
fn a_compound_command_with_one_unknown_part_waits_for_the_operator() {
    let mut sim = sim(asking("ls && rm -rf build"), vec![allow_bash("list", "ls")]);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .permissions
            .iter()
            .any(|p| p.answer_source.is_none())
    }));
    assert_eq!(
        sim.task(1).permissions[0].summary,
        "Bash: ls && rm -rf build"
    );
}

#[test]
fn always_allow_on_a_compound_command_remembers_each_part_and_the_next_run_does_not_ask() {
    let mut sim = sim(asking("git status && ls"), vec![]);
    assert!(sim.run_until(STEPS, |s| !s.task(1).permissions.is_empty()));
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let first = sim.task(1).permissions[0].id;
    engine
        .answer_permission_with(first, true, None, true, &cause, None)
        .unwrap();
    let rules = engine.permission_rules(sim.project_id).unwrap().unwrap();
    let commands: Vec<_> = rules.iter().map(|r| r.command.as_deref()).collect();
    assert_eq!(commands, vec![Some("git status"), Some("ls")]);

    sim.world.set_script(asking("ls && git status"));
    assert!(sim.run_to_shipped(STEPS));
    let sources = answer_sources(&sim);
    let human = sources
        .iter()
        .flatten()
        .filter(|s| s.starts_with("human:"))
        .count();
    assert_eq!(human, 1, "{sources:?}");
    assert!(
        sources.iter().all(Option::is_some),
        "nothing is left pending: {sources:?}"
    );
}
