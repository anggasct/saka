#![allow(
    clippy::unwrap_used,
    reason = "simulations fail loudly on a broken setup"
)]

use saka_core::policy::RuleSpec;
use saka_core::rule::NewRule;
use saka_core::vocab::{Effect, MatchKind};
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
            decide: None,
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
fn every_permission_request_is_counted_by_tool_and_outcome_and_on_the_task() {
    let mut sim = sim(asking("ls"), vec![allow_bash("list", "ls")]);
    assert!(sim.run_until(STEPS, |s| !s.task(1).permissions.is_empty()));
    let asked = sim.task(1).permissions.len();
    let counted: u64 = sim
        .engine()
        .metrics()
        .unwrap()
        .permission_requests
        .iter()
        .filter(|(tool, outcome, _)| tool == "Bash" && outcome == "allowed")
        .map(|(_, _, n)| *n)
        .sum();
    assert_eq!(counted, asked as u64);

    let mut waiting = self::sim(asking("rm -rf build"), vec![]);
    assert!(waiting.run_until(STEPS, |s| !s.task(1).permissions.is_empty()));
    assert!(
        waiting
            .engine()
            .metrics()
            .unwrap()
            .permission_requests
            .contains(&("Bash".to_string(), "pending".to_string(), 1))
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

fn add_rule(
    sim: &Sim,
    effect: Effect,
    kind: MatchKind,
    command: &str,
) -> saka_core::model::PermissionRule {
    let engine = sim.engine();
    engine
        .add_rule(
            sim.project_id,
            NewRule {
                effect,
                tool: "Bash".into(),
                match_kind: kind,
                command: Some(command.into()),
                path: None,
                note: None,
            },
            &engine.cause("human:operator:local"),
            None,
        )
        .unwrap()
}

#[test]
fn a_project_allow_rule_serves_the_developer_and_never_the_reviewer() {
    let agent = AgentScript {
        reviewer_ask: Some(("Bash".into(), json!({ "command": "ls" }))),
        ..asking("ls")
    };
    let mut sim = sim(agent, vec![]);
    let rule = add_rule(&sim, Effect::Allow, MatchKind::Glob, "ls*");
    assert!(sim.run_to_shipped(STEPS));
    let sources: Vec<String> = answer_sources(&sim).into_iter().flatten().collect();
    assert!(
        sources.contains(&format!("policy:project:{}", rule.id)),
        "{sources:?}"
    );
    assert!(
        sources.contains(&"policy:reviewer:deny".to_string()),
        "the reviewer asked and was refused: {sources:?}"
    );
    assert!(
        sources
            .iter()
            .all(|s| s.starts_with("policy:project:") || s == "policy:reviewer:deny"),
        "{sources:?}"
    );
}

#[test]
fn a_project_deny_rule_refuses_without_asking_and_beats_the_operators_allow() {
    let mut sim = sim(
        asking("ls && rm -rf build"),
        vec![allow_bash("anything", "ls*"), allow_bash("rm", "rm *")],
    );
    let rule = add_rule(&sim, Effect::Deny, MatchKind::Glob, "rm *");
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .permissions
            .iter()
            .any(|p| p.answer_source.is_some())
    }));
    let denied = &sim.task(1).permissions[0];
    assert_eq!(
        denied.answer_source.as_deref(),
        Some(format!("policy:project:{}", rule.id).as_str())
    );
    assert_eq!(denied.state, saka_core::vocab::PermissionState::Denied);
}

#[test]
fn a_new_rule_does_not_answer_a_request_that_is_already_waiting() {
    let mut sim = sim(asking("make deploy"), vec![]);
    assert!(sim.run_until(STEPS, |s| {
        s.task(1)
            .permissions
            .iter()
            .any(|p| p.answer_source.is_none())
    }));
    add_rule(&sim, Effect::Allow, MatchKind::Exact, "make deploy");
    assert!(sim.task(1).permissions[0].answer_source.is_none());
    assert_eq!(
        sim.task(1).permissions[0].state,
        saka_core::vocab::PermissionState::Pending
    );
}
