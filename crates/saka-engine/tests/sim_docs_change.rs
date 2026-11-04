#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::job::JobKind;
use saka_core::model::Task;
use saka_core::secret::SecretText;
use saka_core::vocab::{
    AuthorshipMode, BlockerCode, ChangeState, Stage, StagePhase, WorkItemState,
};
use saka_engine::OPERATOR;
use saka_store::Batch;
use saka_testkit::{
    AgentOutcome, CiScript, DOCS_REPO, DocsTurn, FileEdit, Layout, Operator, Pr, SHOP_REPO, Sim,
    SimConfig, repo_entry, saka_toml,
};
use serde_json::{Value, json};

const STEPS: usize = 5_000;
const DOCS_BRANCH: &str = "saka/tasks-done/s1-docs";

fn write(path: &str) -> FileEdit {
    (path.to_string(), Some(format!("{path}\n")))
}

fn config(merge: bool, outcome: AgentOutcome) -> SimConfig {
    let mut cfg = SimConfig {
        max_active_tasks: 1,
        operator: Operator {
            merge,
            ..Operator::default()
        },
        ..SimConfig::default()
    };
    cfg.agent.outcome = outcome;
    cfg
}

fn unapproved(cfg: SimConfig, docs: Vec<DocsTurn>) -> Sim {
    let layout = Layout {
        saka_toml: saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]),
        ..Layout::default()
    };
    let sim = Sim::multi_repo(cfg, &layout);
    sim.world
        .script_edits(vec![vec![write("services/web/a.ts")]]);
    sim.world.script_docs(docs);
    sim
}

fn sim_with(cfg: SimConfig, docs: Vec<DocsTurn>) -> Sim {
    let mut sim = unapproved(cfg, docs);
    sim.approve();
    sim
}

fn api_docs() -> Vec<DocsTurn> {
    vec![DocsTurn::Commit(vec![write("docs/api.md")])]
}

fn docs_pr(sim: &Sim) -> Option<Pr> {
    sim.world.prs().into_iter().find(|p| p.repo == DOCS_REPO)
}

fn code_pr(sim: &Sim) -> Pr {
    sim.world
        .prs()
        .into_iter()
        .find(|p| p.repo == SHOP_REPO)
        .unwrap()
}

fn blocker(sim: &Sim) -> Option<(BlockerCode, Value)> {
    let t = sim.task(1);
    (t.item.state == WorkItemState::Blocked)
        .then(|| t.open_blocker().map(|b| (b.code, b.params.clone())))
        .flatten()
}

fn run_to_block(sim: &mut Sim) -> (BlockerCode, Value) {
    assert!(
        sim.run_until(STEPS, |s| blocker(s).is_some()),
        "S1 never blocked: {:?}",
        sim.task(1).item.stage_phase
    );
    blocker(sim).unwrap()
}

fn run_to_done(sim: &mut Sim) {
    assert!(
        sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Done),
        "S1 never finished: {:?} {:?}",
        sim.task(1).item.stage_phase,
        sim.task(1)
            .open_blocker()
            .map(|b| (b.code, b.params.clone()))
    );
}

fn awaiting_merge(sim: &mut Sim) {
    assert!(sim.run_until(
        STEPS,
        |s| s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
            && s.task(1).docs_change.is_some()
    ));
}

fn request_merge(sim: &Sim) {
    let engine = sim.engine();
    let task = sim.task(1);
    engine
        .request_merge(
            task.item.id,
            task.item.version,
            None,
            &engine.cause("human:operator:local"),
        )
        .unwrap();
}

fn retry(sim: &Sim) {
    let engine = sim.engine();
    engine
        .retry(
            sim.task(1).item.id,
            "operator retry",
            &engine.cause("human:operator:local"),
        )
        .unwrap();
}

fn pushes(sim: &Sim) -> Vec<(String, String)> {
    sim.events_of("scm.branch.pushed")
        .into_iter()
        .filter(|e| e.payload["work_item_id"] == sim.task(1).item.id.to_string())
        .map(|e| {
            (
                e.payload["target"].as_str().unwrap().to_string(),
                e.payload["sha"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn docs_merges(sim: &Sim) -> usize {
    let number = docs_pr(sim).map(|p| p.number);
    sim.world
        .merge_calls()
        .iter()
        .filter(|(repo, n)| repo == DOCS_REPO && Some(*n) == number)
        .count()
}

fn prompts_of(sim: &Sim, kind: &str) -> Vec<Value> {
    sim.events_of("runtime.session.turn_requested")
        .into_iter()
        .map(|e| e.payload["prompt"].clone())
        .filter(|p| p["kind"] == kind)
        .collect()
}

fn docs_path(task: &Task) -> String {
    task.environment
        .as_ref()
        .and_then(|e| e.docs.as_ref())
        .unwrap()
        .path
        .clone()
}

#[test]
fn docs_commits_open_a_docs_pr_after_the_code_pr_and_merge_after_the_code_merge() {
    let mut sim = sim_with(config(true, AgentOutcome::Commit), api_docs());
    run_to_done(&mut sim);

    let targets: Vec<String> = pushes(&sim).into_iter().map(|(t, _)| t).collect();
    assert_eq!(targets, ["docs", "code"], "the docs branch is pushed first");

    let code = code_pr(&sim);
    let docs = docs_pr(&sim).unwrap();
    assert!(code.number < docs.number, "the code PR opens first");
    assert_eq!(docs.head_branch, DOCS_BRANCH);
    assert_eq!(docs.base_branch, "main");
    assert_eq!(docs.title, format!("docs: {}", code.title));
    let task = sim.task(1);
    assert!(docs.body.contains(&format!(
        "https://scm.test/{SHOP_REPO}/pull/{}",
        code.number
    )));
    assert!(
        docs.body
            .contains(&format!("Saka-Task: {}", task.item.task_key))
    );
    assert_eq!(docs.state, ChangeState::Merged);

    let calls = sim.world.merge_calls();
    let at = |n: u64| calls.iter().position(|(_, m)| *m == n).unwrap();
    assert!(at(code.number) < at(docs.number), "code merges first");
    assert_eq!(docs_merges(&sim), 1);

    let checks = sim.world.merge_checks();
    let checked = |repo: &str, sha: &Option<String>| {
        checks
            .iter()
            .any(|(r, c, b)| r == repo && Some(c) == sha.as_ref() && b == "main")
    };
    assert!(checked(SHOP_REPO, &code.merge_commit_sha));
    assert!(checked(DOCS_REPO, &docs.merge_commit_sha));

    let sync: Vec<Value> = sim
        .events_of("delivery.stage_run.created")
        .into_iter()
        .filter(|e| e.payload["stage"] == "sync")
        .map(|e| e.payload["entry_phase"].clone())
        .collect();
    assert_eq!(sync, ["merging_docs"]);

    let cr = task.docs_change.as_ref().unwrap();
    assert_eq!(cr.state, ChangeState::Merged);
    assert_eq!(cr.merge_commit_sha, docs.merge_commit_sha);
    assert!(
        sim.run_until(STEPS, |s| {
            !s.world.branches_of(DOCS_REPO).contains_key(DOCS_BRANCH)
                && !s
                    .world
                    .branches_of(SHOP_REPO)
                    .contains_key(&code.head_branch)
        }),
        "both branches are deleted"
    );
}

#[test]
fn a_code_pr_merged_outside_saka_still_merges_the_docs_pr_after_it() {
    let mut sim = sim_with(config(false, AgentOutcome::Commit), api_docs());
    awaiting_merge(&mut sim);
    assert!(sim.world.merge_calls().is_empty());
    sim.world.merge_outside(code_pr(&sim).number);
    run_to_done(&mut sim);
    let docs = docs_pr(&sim).unwrap();
    assert_eq!(
        sim.world.merge_calls(),
        [(DOCS_REPO.to_string(), docs.number)]
    );
    assert_eq!(docs.state, ChangeState::Merged);
}

#[test]
fn uncommitted_docs_get_a_follow_up_naming_the_docs_checkout() {
    let mut sim = sim_with(
        config(true, AgentOutcome::Commit),
        vec![
            DocsTurn::Dirty,
            DocsTurn::Commit(vec![write("docs/api.md")]),
        ],
    );
    run_to_done(&mut sim);
    let follow_ups = prompts_of(&sim, "docs_uncommitted");
    assert_eq!(follow_ups.len(), 1);
    assert_eq!(follow_ups[0]["path"], docs_path(&sim.task(1)));
    assert_eq!(docs_merges(&sim), 1);
}

#[test]
fn docs_outside_their_folder_block_after_the_follow_ups_without_any_push() {
    let mut sim = sim_with(
        config(true, AgentOutcome::Commit),
        vec![DocsTurn::Commit(vec![write("other/x.md")])],
    );
    let (code, params) = run_to_block(&mut sim);
    assert_eq!(code, BlockerCode::PathScopeViolation);
    assert_eq!(params["target"], "docs");
    assert_eq!(params["stage"], "dev");
    assert_eq!(params["paths"], json!(["other/x.md"]));
    let follow_ups = prompts_of(&sim, "docs_out_of_scope");
    assert!(!follow_ups.is_empty());
    assert_eq!(follow_ups[0]["paths"], json!(["other/x.md"]));
    assert_eq!(follow_ups[0]["path"], docs_path(&sim.task(1)));
    assert!(pushes(&sim).is_empty(), "nothing is pushed");
    assert!(sim.world.prs().is_empty());
}

#[test]
fn docs_commits_without_code_commits_are_no_changes() {
    let mut sim = sim_with(config(true, AgentOutcome::NoCommit), api_docs());
    let (code, params) = run_to_block(&mut sim);
    assert_eq!(code, BlockerCode::NoChanges);
    assert_eq!(params["commits"], 0);
    assert!(pushes(&sim).is_empty());
}

#[test]
fn a_refused_docs_merge_blocks_and_a_retry_merges_it() {
    let mut sim = sim_with(config(false, AgentOutcome::Commit), api_docs());
    awaiting_merge(&mut sim);
    let docs = docs_pr(&sim).unwrap();
    sim.world.script_merges(docs.number, vec!["not_mergeable"]);
    request_merge(&sim);
    let (code, params) = run_to_block(&mut sim);
    assert_eq!(code, BlockerCode::DocsMergeFailed);
    assert_eq!(params["reason"], "not_mergeable");
    assert_eq!(params["status"], 405);
    assert_eq!(params["number"], docs.number);
    assert_eq!(code_pr(&sim).state, ChangeState::Merged);

    retry(&sim);
    run_to_done(&mut sim);
    assert_eq!(docs_merges(&sim), 2);
    assert_eq!(docs_pr(&sim).unwrap().state, ChangeState::Merged);
}

#[test]
fn a_docs_merge_lost_to_a_moved_head_is_retried_on_its_own() {
    let mut sim = sim_with(config(false, AgentOutcome::Commit), api_docs());
    awaiting_merge(&mut sim);
    let docs = docs_pr(&sim).unwrap();
    sim.world.script_merges(docs.number, vec!["head_changed"]);
    request_merge(&sim);
    run_to_done(&mut sim);
    let blocked: Vec<Value> = sim
        .events_of("delivery.work_item.blocked")
        .into_iter()
        .map(|e| e.payload.clone())
        .collect();
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0]["code"], "docs_merge_failed");
    assert_eq!(blocked[0]["params"]["status"], 409);
    assert_eq!(docs_merges(&sim), 2);
}

#[test]
fn a_docs_head_pushed_from_outside_with_paths_out_of_scope_blocks_without_a_merge() {
    let mut sim = sim_with(config(false, AgentOutcome::Commit), api_docs());
    awaiting_merge(&mut sim);
    let head = sim
        .world
        .push_outside(DOCS_REPO, DOCS_BRANCH, &[write("other/x.md")]);
    request_merge(&sim);
    let (code, params) = run_to_block(&mut sim);
    assert_eq!(code, BlockerCode::PathScopeViolation);
    assert_eq!(params["target"], "docs");
    assert_eq!(params["stage"], "sync");
    assert_eq!(params["head_sha"], head);
    assert_eq!(docs_merges(&sim), 0, "no merge of the docs PR");
    assert_eq!(sim.task(1).docs_change.as_ref().unwrap().head_sha, head);
}

#[test]
fn a_docs_pr_closed_before_sync_is_skipped_once_and_the_task_finishes() {
    let mut sim = sim_with(config(false, AgentOutcome::Commit), api_docs());
    awaiting_merge(&mut sim);
    let docs = docs_pr(&sim).unwrap();
    sim.world.close_outside(docs.number);
    request_merge(&sim);
    run_to_done(&mut sim);
    let skipped = sim.events_of("delivery.docs_change.skipped");
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].payload["number"], docs.number);
    assert_eq!(skipped[0].payload["reason"], "closed");
    assert_eq!(docs_merges(&sim), 0);
    let cr = sim.task(1).docs_change.clone().unwrap();
    assert_eq!(cr.state, ChangeState::Closed);
    assert!(cr.skipped_at.is_some());
    let metrics = sim.engine().metrics().unwrap();
    assert!(metrics.docs_changes.contains(&("skipped".to_string(), 1)));
}

#[test]
fn a_retry_in_a_new_environment_resumes_the_pushed_docs_branch_and_its_pr() {
    let mut sim = sim_with(
        config(false, AgentOutcome::Commit),
        vec![
            DocsTurn::Commit(vec![write("docs/api.md")]),
            DocsTurn::Commit(vec![write("docs/more.md")]),
        ],
    );
    sim.world.set_ci(CiScript::Fail(vec!["test".into()]));
    let (code, _) = run_to_block(&mut sim);
    assert_eq!(code, BlockerCode::CiRed);
    let first = docs_pr(&sim).unwrap();
    let pushed = sim.task(1).docs_change.clone().unwrap().head_sha;
    let old_env = sim.task(1).environment.clone().unwrap().id;

    // The worktrees are lost while the task is blocked, so the retry prepares a new environment.
    let mut task = sim.task(1);
    if let Some(env) = task.environment.as_mut() {
        env.state = saka_core::vocab::EnvironmentState::TornDown;
    }
    let now = saka_ports::Clock::now(&*sim.clock);
    sim.engine()
        .store()
        .commit(&Batch {
            now,
            tasks: vec![task],
            ..Batch::default()
        })
        .unwrap();
    sim.restart();
    sim.world.set_ci(CiScript::Pass);
    retry(&sim);
    assert!(sim.run_until(
        STEPS,
        |s| s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
            && s.task(1).docs_change.as_ref().unwrap().head_sha != pushed
    ));

    let env = sim.task(1).environment.clone().unwrap();
    assert_ne!(env.id, old_env);
    let prepared = sim.events_of("runtime.environment.prepared");
    assert_eq!(prepared.len(), 2);
    let docs = &prepared[1].payload["docs"];
    assert_eq!(
        docs["base_sha"], pushed,
        "the checkout starts at the pushed head"
    );
    assert_eq!(docs["pushed_sha"], pushed, "the next push leases that head");
    assert_eq!(docs["commit"], prepared[0].payload["docs"]["commit"]);
    let checkout = &sim.world.docs_checkouts()[&env.id];
    assert!(sim.world.is_ancestor(&pushed, &checkout.head));
    assert_eq!(checkout.branch.as_deref(), Some(DOCS_BRANCH));

    let docs_pushes: Vec<String> = pushes(&sim)
        .into_iter()
        .filter(|(t, _)| t == "docs")
        .map(|(_, sha)| sha)
        .collect();
    assert_eq!(docs_pushes.len(), 2);
    assert_eq!(docs_pushes[0], pushed);
    assert_eq!(
        sim.world.branches_of(DOCS_REPO)[DOCS_BRANCH],
        docs_pushes[1]
    );
    let prs: Vec<Pr> = sim
        .world
        .prs()
        .into_iter()
        .filter(|p| p.repo == DOCS_REPO)
        .collect();
    assert_eq!(prs.len(), 1, "the same docs PR");
    assert_eq!(prs[0].number, first.number);
    assert_eq!(
        sim.task(1).docs_change.as_ref().unwrap().head_sha,
        docs_pushes[1]
    );
}

#[test]
fn a_docs_merge_commit_missing_from_the_base_fails_the_definition_of_done() {
    let mut sim = sim_with(config(true, AgentOutcome::Commit), api_docs());
    sim.world.lose_merges_in(DOCS_REPO);
    let (code, params) = run_to_block(&mut sim);
    assert_eq!(code, BlockerCode::DodFailed);
    assert_eq!(params["reason"], "docs_not_in_base");
    assert_eq!(
        params["merge_commit_sha"],
        json!(docs_pr(&sim).unwrap().merge_commit_sha)
    );
    assert_eq!(params["base_branch"], "main");
}

fn with_user_pat(sim: &Sim) -> String {
    sim.world.accounts().set_user(7, "ann", Some("Ann Lee"));
    let pat = sim.world.accounts().issue_pat(None);
    sim.clock.advance(1);
    let engine = sim.engine();
    let cause = engine.cause(OPERATOR);
    sim.block_on(engine.set_user_token(
        sim.project_id,
        &SecretText::new(pat.clone()),
        &cause,
        None,
    ))
    .unwrap();
    sim.clock.advance(1);
    engine
        .set_authorship(sim.project_id, AuthorshipMode::UserPat, &cause, None)
        .unwrap();
    pat
}

#[test]
fn the_docs_pr_is_opened_with_the_same_credential_as_the_code_pr() {
    let mut sim = unapproved(config(true, AgentOutcome::Commit), api_docs());
    let pat = with_user_pat(&sim);
    sim.approve();
    run_to_done(&mut sim);

    assert_eq!(code_pr(&sim).opened_with, pat);
    assert_eq!(docs_pr(&sim).unwrap().opened_with, pat);
    let opened: Vec<Value> = sim
        .events_of("scm.change_request.opened")
        .into_iter()
        .filter(|e| e.payload["work_item_id"] == sim.task(1).item.id.to_string())
        .map(|e| json!([e.payload["role"], e.payload["opened_with"]]))
        .collect();
    assert_eq!(
        opened,
        [json!(["code", "user_token"]), json!(["docs", "user_token"])]
    );
}

#[test]
fn a_docs_pr_refused_to_the_user_blocks_on_the_docs_target_and_a_retry_opens_only_it() {
    let mut sim = unapproved(config(true, AgentOutcome::Commit), api_docs());
    with_user_pat(&sim);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).change_request.is_some()));
    sim.world.accounts().deny_user_pulls(Some(403));
    let (code, params) = run_to_block(&mut sim);
    assert_eq!(code, BlockerCode::UserAccessDenied);
    assert_eq!(params["target"], "docs");
    assert!(docs_pr(&sim).is_none());

    sim.world.accounts().deny_user_pulls(None);
    retry(&sim);
    run_to_done(&mut sim);
    let prs = sim.world.prs();
    assert_eq!(prs.iter().filter(|p| p.repo == SHOP_REPO).count(), 1);
    assert_eq!(prs.iter().filter(|p| p.repo == DOCS_REPO).count(), 1);
    assert_eq!(pushes(&sim).len(), 2, "nothing is pushed again");
}

fn is_docs_job(kind: JobKind) -> bool {
    matches!(
        kind,
        JobKind::ScmPushDocsBranch | JobKind::ScmOpenDocsChangeRequest | JobKind::ScmMergeDocs
    )
}

#[test]
fn a_crash_around_each_docs_job_never_opens_or_merges_the_docs_pr_twice() {
    let mut sim = sim_with(config(true, AgentOutcome::Commit), api_docs());
    let mut crashed = Vec::new();
    for _ in 0..STEPS {
        if sim.task(1).item.state == WorkItemState::Done {
            break;
        }
        sim.operate();
        let engine = sim.engine();
        let job = if sim.world.has_events() {
            None
        } else {
            engine.claim_job().unwrap()
        };
        let Some(job) = job else {
            drop(engine);
            sim.step();
            continue;
        };
        let execution = sim.block_on(engine.execute(&job));
        if is_docs_job(job.kind) && !crashed.contains(&job.kind) {
            crashed.push(job.kind);
            drop(engine);
            sim.restart();
        } else {
            engine.complete_job(&job, execution).unwrap();
        }
    }
    assert_eq!(sim.task(1).item.state, WorkItemState::Done);
    assert_eq!(
        crashed,
        [
            JobKind::ScmPushDocsBranch,
            JobKind::ScmOpenDocsChangeRequest,
            JobKind::ScmMergeDocs
        ]
    );
    let prs: Vec<Pr> = sim
        .world
        .prs()
        .into_iter()
        .filter(|p| p.repo == DOCS_REPO)
        .collect();
    assert_eq!(prs.len(), 1);
    assert_eq!(prs[0].state, ChangeState::Merged);
    assert_eq!(docs_merges(&sim), 1, "one merge of the docs PR");
    let opened = sim.events_of("scm.change_request.opened");
    assert_eq!(
        opened
            .iter()
            .filter(|e| e.payload["role"] == "docs")
            .count(),
        1
    );
    let merged = sim.events_of("scm.change_request.merged");
    let docs: Vec<&Value> = merged
        .iter()
        .map(|e| &e.payload)
        .filter(|p| p["role"] == "docs")
        .collect();
    assert_eq!(docs.len(), 1);
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
fn a_merged_docs_pr_is_counted_and_logged() {
    saka_testkit::logs::check_callsites_per_thread();
    let buffer = Buffer::default();
    let out = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || out.clone())
        .finish();
    let (work_item, docs, metrics) = tracing::subscriber::with_default(subscriber, || {
        let mut sim = sim_with(config(true, AgentOutcome::Commit), api_docs());
        run_to_done(&mut sim);
        (
            sim.task(1).item.id,
            docs_pr(&sim).unwrap(),
            sim.engine().metrics().unwrap(),
        )
    });
    assert_eq!(
        metrics.docs_changes,
        [("merged".to_string(), 1), ("skipped".to_string(), 0)]
    );
    let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    let merged: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|l| l["fields"]["message"] == "delivery.docs_change.merged")
        .collect();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0]["fields"]["work_item_id"], work_item.to_string());
    assert_eq!(
        merged[0]["fields"]["merge_commit_sha"],
        json!(docs.merge_commit_sha)
    );
    assert_eq!(merged[0]["fields"]["merged_by_saka"], true);
    assert_eq!(merged[0]["fields"]["number"], docs.number);
}
