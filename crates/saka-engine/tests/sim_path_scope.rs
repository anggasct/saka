#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::vocab::{BlockerCode, SessionState, SpecState, Stage, StagePhase, WorkItemState};
use saka_testkit::{DOCS_REPO, FileEdit, Layout, SHOP_REPO, Sim, SimConfig, repo_entry, saka_toml};
use serde_json::{Value, json};

const STEPS: usize = 5_000;
const API_ONLY: &str = "path_scope = [\"services/api/**\"]\n";
const SCM_GATE: &str = "[gate]\nsource = \"scm\"\n";

fn write(path: &str) -> FileEdit {
    (path.to_string(), Some(format!("{path}\n")))
}

fn delete(path: &str) -> FileEdit {
    (path.to_string(), None)
}

fn shop_scoped(extra: &str) -> Layout {
    Layout {
        saka_toml: saka_toml("shop", &[repo_entry("shop", SHOP_REPO, extra)]),
        ..Layout::default()
    }
}

fn one_at_a_time() -> SimConfig {
    SimConfig {
        max_active_tasks: 1,
        ..SimConfig::default()
    }
}

fn started(layout: &Layout, edits: Vec<Vec<FileEdit>>) -> Sim {
    let mut sim = Sim::multi_repo(one_at_a_time(), layout);
    sim.world.script_edits(edits);
    sim.approve();
    sim
}

fn blocked(sim: &Sim) -> Option<(BlockerCode, Value)> {
    let t = sim.task(1);
    (t.item.state == WorkItemState::Blocked)
        .then(|| t.open_blocker().map(|b| (b.code, b.params.clone())))
        .flatten()
}

fn run_to_block(sim: &mut Sim) -> (BlockerCode, Value) {
    assert!(
        sim.run_until(STEPS, |s| blocked(s).is_some()),
        "S1 never blocked: {:?}",
        sim.task(1).item.stage_phase
    );
    blocked(sim).unwrap()
}

fn out_of_scope_prompts(sim: &Sim) -> Vec<Value> {
    sim.events_of("runtime.session.turn_requested")
        .into_iter()
        .map(|e| e.payload["prompt"].clone())
        .filter(|p| p["kind"] == "out_of_scope")
        .collect()
}

#[test]
fn a_commit_outside_the_scope_gets_a_follow_up_and_a_fix_is_pushed() {
    let mut sim = started(
        &shop_scoped(API_ONLY),
        vec![
            vec![write("services/api/x.rs"), write("services/web/a.ts")],
            vec![delete("services/web/a.ts")],
        ],
    );
    assert!(sim.run_until(STEPS, |s| s.task(1).change_request.is_some()));
    assert_eq!(
        out_of_scope_prompts(&sim),
        [json!({
            "kind": "out_of_scope",
            "paths": ["services/web/a.ts"],
            "count": 1,
            "scope": ["services/api/**"],
        })]
    );
    let prompts = sim.world.prompts();
    assert!(prompts.iter().any(|p| p
        == "Your commits change files outside the paths this task may change: services/web/a.ts. \
            Allowed paths: services/api/**. Remove those changes so the branch differs from its base \
            only inside the allowed paths, then commit again."));
    let t = sim.task(1);
    let session = t.session.unwrap();
    assert_eq!((session.turns, session.followups), (2, 1));
    assert_eq!(sim.stats().pushes, 1);
    let checks = sim.world.scope_checks();
    assert_eq!(checks.len(), 2, "one check per inspected turn");
    assert!(
        checks
            .iter()
            .all(|c| c.repo == SHOP_REPO && !c.fetch && c.base == checks[0].base)
    );
}

#[test]
fn a_commit_kept_outside_the_scope_blocks_without_a_push_and_a_retry_resumes_the_session() {
    let mut sim = started(
        &shop_scoped(API_ONLY),
        vec![vec![write("services/web/a.ts")]],
    );
    let (code, params) = run_to_block(&mut sim);
    assert_eq!(code, BlockerCode::PathScopeViolation);
    let t = sim.task(1);
    let head = t.stage_run.as_ref().unwrap().head_sha.clone();
    assert_eq!(head, None, "the run never reached pushing");
    let session = t.session.clone().unwrap();
    let head_sha = params["head_sha"].as_str().unwrap().to_string();
    assert_eq!(
        params,
        json!({
            "stage": "dev",
            "head_sha": head_sha,
            "reason": "outside_scope",
            "paths": ["services/web/a.ts"],
            "count": 1,
            "scope": ["services/api/**"],
        })
    );
    assert_eq!(session.state, SessionState::Ended);
    assert_eq!(session.end_reason.unwrap().code, "path_scope_violation");
    assert_eq!(session.followups, 1);
    assert_eq!(sim.stats().pushes, 0);
    assert!(
        !sim.world
            .branches_of(SHOP_REPO)
            .contains_key(&t.item.branch)
    );

    sim.world
        .script_edits(vec![vec![delete("services/web/a.ts")]]);
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry(t.item.id, "drop the web change", &cause)
        .unwrap();
    let t = sim.task(1);
    let run = t.stage_run.clone().unwrap();
    assert_eq!(
        (run.stage, run.attempt, run.entry_phase),
        (Stage::Dev, 2, StagePhase::RunningSession)
    );
    let created = sim.events_of("delivery.stage_run.created");
    assert_eq!(
        created.last().unwrap().payload["retry_of"]["code"],
        "path_scope_violation"
    );
    let prompts_before = sim.world.prompts().len();
    assert!(sim.run_until(STEPS, |s| s.task(1).change_request.is_some()));
    let first = &sim.world.prompts()[prompts_before];
    assert!(
        first.contains(
            "Your commits change files outside the paths this task may change: services/web/a.ts. Allowed paths: services/api/**."
        ),
        "the first turn of the retry names the paths and the scope: {first}"
    );
    assert_eq!(sim.stats().pushes, 1);
}

#[test]
fn a_head_pushed_from_outside_the_scope_is_never_merged() {
    let mut sim = started(
        &shop_scoped(API_ONLY),
        vec![vec![write("services/api/x.rs")]],
    );
    sim.world.set_recipe(Some(SCM_GATE));
    assert!(sim.run_until(STEPS, |s| s.task(1).change_request.is_some()));
    let branch = sim.task(1).item.branch.clone();
    let outside = sim
        .world
        .push_outside(SHOP_REPO, &branch, &[write("services/web/x.ts")]);
    let (code, params) = run_to_block(&mut sim);
    assert_eq!(code, BlockerCode::PathScopeViolation, "{params}");
    assert_eq!(
        params,
        json!({
            "stage": "merge",
            "head_sha": outside,
            "reason": "outside_scope",
            "paths": ["services/web/x.ts"],
            "count": 1,
            "scope": ["services/api/**"],
        })
    );
    assert_eq!(sim.task(1).stage_run.unwrap().stage, Stage::Merge);
    assert_eq!(sim.stats().merges, 0);
    assert_eq!(
        sim.world.prs()[0].state,
        saka_core::vocab::ChangeState::Open
    );
    let merge_check = sim.world.scope_checks().into_iter().last().unwrap();
    assert_eq!(
        (
            merge_check.base.as_str(),
            merge_check.head_sha.as_str(),
            merge_check.fetch
        ),
        ("refs/remotes/origin/main", outside.as_str(), true)
    );
}

#[test]
fn a_scope_of_everything_never_blocks_and_never_asks_git() {
    let mut sim = Sim::new(one_at_a_time());
    sim.world.set_recipe(Some(SCM_GATE));
    sim.world.script_edits(vec![vec![
        write("anything/at/all.rs"),
        write(".github/workflows/ci.yml"),
    ]]);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).change_request.is_some()));
    let branch = sim.task(1).item.branch.clone();
    sim.world.push_outside(
        saka_testkit::HOME_REPO,
        &branch,
        &[write("somewhere/else.txt")],
    );
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.spec().state, SpecState::Shipped);
    assert!(sim.world.scope_checks().is_empty());
}

fn docs_in_code(edits: Vec<Vec<FileEdit>>) -> Sim {
    let layout = Layout {
        saka_toml: saka_toml("docs", &[repo_entry("docs", DOCS_REPO, API_ONLY)]),
        ..Layout::default()
    };
    started(&layout, edits)
}

#[test]
fn the_docs_folder_counts_as_in_scope_in_the_workspace_repo() {
    let mut sim = docs_in_code(vec![vec![write("docs/api.md"), write("services/api/x.rs")]]);
    assert_eq!(
        sim.project().project.repos[0].path_scope,
        ["services/api/**"]
    );
    assert!(sim.run_until(STEPS, |s| s.task(1).change_request.is_some()));
    assert!(out_of_scope_prompts(&sim).is_empty());
    assert_eq!(sim.stats().pushes, 1);

    let mut sim = docs_in_code(vec![vec![write("services/web/x.ts")]]);
    let (code, params) = run_to_block(&mut sim);
    assert_eq!(code, BlockerCode::PathScopeViolation);
    assert_eq!(params["paths"], json!(["services/web/x.ts"]));
    assert_eq!(params["scope"], json!(["services/api/**", "docs/**"]));
    assert_eq!(sim.stats().pushes, 0);
}

fn environment_lines(text: &str) -> Vec<String> {
    let (_, rest) = text.split_once("\nEnvironment:\n").unwrap();
    rest.lines().take(2).map(String::from).collect()
}

#[test]
fn the_work_packet_names_the_repo_and_keeps_the_scope_it_was_prepared_with() {
    let mut cfg = one_at_a_time();
    cfg.agent.outcome = saka_testkit::AgentOutcome::Hang;
    let mut sim = Sim::multi_repo(cfg, &shop_scoped(API_ONLY));
    sim.approve();
    assert!(sim.run_until(STEPS, |s| !s.world.prompts().is_empty()));
    let sent = sim.world.prompts()[0].clone();
    assert_eq!(
        environment_lines(&sent),
        [
            "- Repository: shop (acme/shop), base branch main",
            "- Paths you may change: services/api/**",
        ]
    );
    let prepared = sim.events_of("runtime.environment.prepared");
    assert_eq!(
        prepared[0].payload["path_scope"],
        json!(["services/api/**"])
    );

    let widened = saka_toml(
        "shop",
        &[repo_entry(
            "shop",
            SHOP_REPO,
            "path_scope = [\"services/**\"]\n",
        )],
    );
    sim.world.put_doc("docs/saka.toml", widened.as_bytes());
    sim.reload().unwrap();
    assert_eq!(sim.project().project.repos[0].path_scope, ["services/**"]);
    let (_, packet) = sim.engine().work_packet(sim.task(1).item.id).unwrap();
    assert_eq!(packet.text, sent, "the packet is the one turn 1 sent");

    let mut cfg = one_at_a_time();
    cfg.agent.outcome = saka_testkit::AgentOutcome::Hang;
    let mut sim = Sim::multi_repo(cfg, &shop_scoped(""));
    sim.approve();
    assert!(sim.run_until(STEPS, |s| !s.world.prompts().is_empty()));
    assert_eq!(
        environment_lines(&sim.world.prompts()[0])[1],
        "- Paths you may change: all"
    );
}

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
fn violations_and_refused_configs_are_logged_and_counted() {
    saka_testkit::logs::check_callsites_per_thread();
    let buffer = Buffer::default();
    let out = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || out.clone())
        .finish();
    let (work_item, violations) = tracing::subscriber::with_default(subscriber, || {
        let mut sim = started(
            &shop_scoped(API_ONLY),
            vec![vec![write("services/web/a.ts")]],
        );
        run_to_block(&mut sim);
        sim.world
            .put_doc("docs/saka.toml", b"[project\nname = \"shop\"\n");
        assert!(sim.reload().is_err());
        let metrics = sim.engine().metrics().unwrap();
        (sim.task(1).item.id, metrics.path_scope_violations)
    });
    assert_eq!(violations, [("dev".to_string(), 2)]);
    let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let named = |message: &str| -> Vec<Value> {
        lines
            .iter()
            .filter(|l| l["fields"]["message"] == message)
            .cloned()
            .collect()
    };
    let violated = named("delivery.path_scope.violated");
    assert_eq!(violated.len(), 2);
    assert_eq!(violated[0]["level"], "WARN");
    assert_eq!(violated[0]["fields"]["work_item_id"], work_item.to_string());
    assert_eq!(violated[0]["fields"]["stage"], "dev");
    assert_eq!(violated[0]["fields"]["count"], 1);
    assert!(violated[0]["fields"]["head_sha"].is_string());
    let rejected = named("workspace.config.rejected");
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0]["level"], "WARN");
    assert_eq!(rejected[0]["fields"]["code"], "project_config_invalid");
    assert_eq!(rejected[0]["fields"]["trigger"], "operator");
}
