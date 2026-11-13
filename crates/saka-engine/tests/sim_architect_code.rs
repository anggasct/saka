#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::ids::SpecDraftId;
use saka_core::model::Draft;
use saka_core::vocab::{
    DraftMode, EnvironmentState, PermissionState, SessionState, SpecDraftState,
};
use saka_testkit::{
    AgentOutcome, DraftTurn, Layout, Operator, SHOP_REPO, Sim, SimConfig, WEB_REPO, repo_entry,
    saka_toml,
};
use serde_json::json;

const STEPS: usize = 4_000;
const PATH: &str = "docs/specs/export.md";
const IDEA: &str = "Let people export their task list as a CSV file.";

fn spec() -> Vec<u8> {
    b"---
saka: spec
---
# Export tasks

## Acceptance criteria

- **AC-01**: `tasks export` writes a CSV file.

## Done when

- `make test`
"
    .to_vec()
}

fn config(tune: impl FnOnce(&mut SimConfig)) -> SimConfig {
    let mut cfg = SimConfig {
        operator: Operator {
            start: true,
            merge: true,
            retry: vec![],
            answer: None,
            decide: None,
        },
        ..SimConfig::default()
    };
    tune(&mut cfg);
    cfg
}

fn separate(tune: impl FnOnce(&mut SimConfig)) -> Sim {
    let layout = Layout {
        saka_toml: saka_toml(
            "shop",
            &[
                repo_entry("shop", SHOP_REPO, "default = true\n"),
                repo_entry("web", WEB_REPO, ""),
            ],
        ),
        ..Layout::default()
    };
    Sim::multi_repo(config(tune), &layout)
}

fn drafted(sim: &Sim, turns: Vec<DraftTurn>) -> SpecDraftId {
    sim.world.script_draft(turns);
    sim.create_draft(PATH, IDEA, &[]).unwrap().draft.id
}

fn write() -> DraftTurn {
    DraftTurn::Write {
        path: PATH.into(),
        bytes: spec(),
    }
}

fn run_to(sim: &mut Sim, id: SpecDraftId, done: impl Fn(&Draft) -> bool) -> Draft {
    assert!(
        sim.run_until(STEPS, |s| done(&s.draft(id))),
        "draft never reached the state: {:?}",
        sim.draft(id).draft
    );
    sim.draft(id)
}

fn torn_down(sim: &mut Sim, id: SpecDraftId) {
    run_to(sim, id, |d| {
        d.environment
            .as_ref()
            .is_some_and(|e| e.state == EnvironmentState::TornDown)
    });
}

#[test]
fn a_draft_in_a_separate_docs_repo_reads_every_code_repo_at_its_base_tip() {
    let mut sim = separate(|cfg| cfg.agent.outcome = AgentOutcome::Hang);
    let shop = sim.world.push_to_base_of(SHOP_REPO);
    let web = sim.world.push_to_base_of(WEB_REPO);
    let id = drafted(&sim, vec![write()]);
    let d = run_to(&mut sim, id, |d| d.draft.state == SpecDraftState::Drafting);
    let attempt = d.attempt.clone().unwrap();
    let env = d.environment.clone().unwrap();
    let dir = |name: &str| format!("/sim/envs/{}/code/{name}", env.id);

    let checkouts = sim.world.code_checkouts();
    assert_eq!(checkouts.len(), 2);
    let shop_checkout = &checkouts[&(env.id, "shop".to_string())];
    let web_checkout = &checkouts[&(env.id, "web".to_string())];
    assert_eq!(
        (
            shop_checkout.repo.as_str(),
            shop_checkout.base_sha.as_str(),
            shop_checkout.base_branch.as_str()
        ),
        (SHOP_REPO, shop.as_str(), "main")
    );
    assert_eq!(
        (web_checkout.repo.as_str(), web_checkout.base_sha.as_str()),
        (WEB_REPO, web.as_str())
    );
    assert!(!checkouts.contains_key(&(env.id, "docs".to_string())));

    assert_eq!(
        attempt.code_commits,
        [
            ("shop".to_string(), shop.clone()),
            ("web".to_string(), web.clone())
        ]
        .into()
    );
    let rendered = sim.events_of("spec.draft.packet_rendered");
    assert_eq!(
        rendered[0].payload["code_commits"],
        json!({ "shop": shop, "web": web })
    );

    run_to(&mut sim, id, |d| {
        d.session
            .as_ref()
            .is_some_and(|s| s.state == SessionState::Running)
    });
    let launch = sim.world.launches()[0].clone();
    assert_eq!(launch.extra_dirs, [dir("shop"), dir("web")]);

    let packet = sim.world.prompts()[0].clone();
    assert!(packet.contains(&format!(
        "- Code (read-only): {} ({SHOP_REPO} at {}, base main)\n",
        dir("shop"),
        &shop[..12]
    )));
    assert!(packet.contains(&format!(
        "- Code (read-only): {} ({WEB_REPO} at {}, base main)\n",
        dir("web"),
        &web[..12]
    )));
    assert!(packet.contains("quote file:line from them and name the commit you read"));
    assert!(!packet.contains("code_checkout_skipped"));
    let metrics = sim.engine().metrics().unwrap();
    assert_eq!(metrics.architect_code_checkouts, [("ready".to_string(), 2)]);
}

#[test]
fn a_draft_in_the_code_repo_gets_no_code_checkout_and_the_same_packet() {
    let mut sim = Sim::new(config(|cfg| cfg.agent.outcome = AgentOutcome::Hang));
    let id = drafted(&sim, vec![write()]);
    run_to(&mut sim, id, |d| {
        d.session
            .as_ref()
            .is_some_and(|s| s.state == SessionState::Running)
    });
    assert!(sim.world.code_checkouts().is_empty());
    assert!(sim.world.launches()[0].extra_dirs.is_empty());
    let packet = sim.world.prompts()[0].clone();
    assert!(!packet.contains("Project code"));
    assert!(!packet.contains("Code (read-only)"));
    assert!(!packet.contains("code checkouts"));
    assert!(
        sim.events_of("spec.draft.packet_rendered")[0].payload["code_commits"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_conversation_with_the_architect_reads_the_code_repos_too() {
    let mut sim = separate(|cfg| cfg.agent.outcome = AgentOutcome::Hang);
    sim.world.script_draft(vec![write()]);
    let id = sim.create_session(PATH, IDEA, &[]).unwrap().draft.id;
    let d = run_to(&mut sim, id, |d| {
        d.draft.state == SpecDraftState::Drafting && d.live_session().is_some()
    });
    assert_eq!(d.draft.mode, DraftMode::Session);
    assert_eq!(sim.world.code_checkouts().len(), 2);
    assert_eq!(sim.world.launches()[0].extra_dirs.len(), 2);
}

fn architect_asks(
    tool: &str,
    input: impl FnOnce(&str) -> serde_json::Value,
) -> (PermissionState, String) {
    let mut sim = separate(|cfg| cfg.agent.outcome = AgentOutcome::Hang);
    let id = drafted(&sim, vec![write()]);
    let d = run_to(&mut sim, id, |d| d.attempt.is_some());
    let file = format!("/sim/envs/{}/code/shop/src/lib.rs", d.attempt.unwrap().id.0);
    sim.world.script_ask(Some((tool.to_string(), input(&file))));
    assert!(sim.run_until(STEPS, |s| s.stats().answers_to_cli == 1));
    let requests = sim
        .engine()
        .permission_requests(
            &saka_engine::PermissionFilter {
                spec_draft: Some(id),
                ..Default::default()
            },
            &saka_engine::Page {
                after: None,
                limit: 10,
            },
        )
        .unwrap()
        .items;
    assert_eq!(requests.len(), 1);
    (
        requests[0].request.state,
        requests[0].request.answer_source.clone().unwrap(),
    )
}

#[test]
fn the_architect_reads_a_code_checkout() {
    for tool in ["Read", "Grep", "Glob", "LS"] {
        let key = if tool == "Read" { "file_path" } else { "path" };
        let (state, source) = architect_asks(tool, |file| json!({ key: file }));
        assert_eq!(state, PermissionState::Allowed, "{tool}");
        assert_eq!(source, "policy:architect:read", "{tool}");
    }
}

#[test]
fn the_architect_cannot_write_a_code_checkout() {
    for tool in ["Write", "Edit", "MultiEdit"] {
        let (state, _) = architect_asks(tool, |file| json!({ "file_path": file }));
        assert_eq!(state, PermissionState::Denied, "{tool}");
    }
}

#[test]
fn a_repo_over_the_size_limit_is_skipped_with_a_note_and_the_draft_still_succeeds() {
    let mut sim = separate(|_| {});
    sim.world.script_code_size(WEB_REPO, 50_001, 10);
    let id = drafted(&sim, vec![write()]);
    let d = run_to(&mut sim, id, |d| d.draft.state.is_finished());
    assert_eq!(d.draft.state, SpecDraftState::Succeeded, "{:?}", d.draft);
    let checkouts = sim.world.code_checkouts();
    assert_eq!(checkouts.len(), 1);
    assert!(checkouts.keys().all(|(_, name)| name == "shop"));
    let packet = sim.world.prompts()[0].clone();
    assert!(packet.contains(&format!(
        "- Code not checked out: web ({WEB_REPO}), code_checkout_skipped: too_large\n"
    )));
    assert!(packet.contains("- Code (read-only): "));
    assert_eq!(
        sim.draft(id)
            .attempt
            .unwrap()
            .code_commits
            .keys()
            .collect::<Vec<_>>(),
        ["shop"]
    );
    let metrics = sim.engine().metrics().unwrap();
    assert!(
        metrics
            .architect_code_checkouts
            .contains(&("too_large".to_string(), 1))
    );
}

#[test]
fn a_repo_over_the_byte_limit_is_skipped_too() {
    let mut sim = separate(|_| {});
    sim.world
        .script_code_size(SHOP_REPO, 10, saka_ports::CODE_MAX_BYTES + 1);
    let id = drafted(&sim, vec![write()]);
    let d = run_to(&mut sim, id, |d| d.draft.state.is_finished());
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    assert!(sim.world.prompts()[0].contains("shop (acme/shop), code_checkout_skipped: too_large"));
}

#[test]
fn the_code_checkouts_are_torn_down_after_a_success() {
    let mut sim = separate(|_| {});
    let id = drafted(&sim, vec![write()]);
    let d = run_to(&mut sim, id, |d| d.draft.state.is_finished());
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    torn_down(&mut sim, id);
    let checkouts = sim.world.code_checkouts();
    assert_eq!(checkouts.len(), 2);
    assert!(checkouts.values().all(|c| c.torn_down));
}

#[test]
fn the_code_checkouts_are_torn_down_after_a_failure() {
    let mut sim = separate(|_| {});
    let id = drafted(&sim, vec![DraftTurn::Nothing]);
    let d = run_to(&mut sim, id, |d| d.draft.state.is_finished());
    assert_eq!(d.draft.state, SpecDraftState::Failed);
    torn_down(&mut sim, id);
    let checkouts = sim.world.code_checkouts();
    assert_eq!(checkouts.len(), 2);
    assert!(checkouts.values().all(|c| c.torn_down));
}

#[test]
fn the_code_checkouts_are_torn_down_after_a_cancel() {
    let mut sim = separate(|cfg| cfg.agent.outcome = AgentOutcome::Hang);
    let id = drafted(&sim, vec![write()]);
    run_to(&mut sim, id, |d| d.draft.state == SpecDraftState::Drafting);
    assert!(sim.world.code_checkouts().values().all(|c| !c.torn_down));
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .cancel_spec_draft(id, "not needed", &cause, None)
        .unwrap();
    torn_down(&mut sim, id);
    let checkouts = sim.world.code_checkouts();
    assert_eq!(checkouts.len(), 2);
    assert!(checkouts.values().all(|c| c.torn_down));
}
