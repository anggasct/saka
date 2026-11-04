#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::rejection::Rejection;
use saka_core::vocab::{ReloadOutcome, SpecState, WorkItemState};
use saka_engine::EngineError;
use saka_ports::Clock;
use saka_testkit::{DOCS_REPO, Layout, SHOP_REPO, Sim, SimConfig, WEB_REPO, repo_entry, saka_toml};
use serde_json::json;

const STEPS: usize = 5_000;
const POLL_SECS: u32 = 300;

fn shop(extra: &str) -> String {
    repo_entry("shop", SHOP_REPO, extra)
}

fn web(extra: &str) -> String {
    repo_entry("web", WEB_REPO, extra)
}

fn layout(repos: &[String]) -> Layout {
    Layout {
        saka_toml: saka_toml("shop", repos),
        ..Layout::default()
    }
}

fn polling() -> SimConfig {
    SimConfig {
        config_poll_secs: POLL_SECS,
        ..SimConfig::default()
    }
}

fn wait_one_poll(sim: &mut Sim) {
    let until = sim.clock.now().0 + i64::from(POLL_SECS) * 1000;
    assert!(sim.run_until(STEPS, |s| s.clock.now().0 >= until));
}

fn names(sim: &Sim) -> Vec<String> {
    sim.project()
        .project
        .repos
        .iter()
        .map(|r| r.name.clone())
        .collect()
}

fn ship(sim: &mut Sim) {
    sim.approve();
    let shipped = sim.run_to_shipped(STEPS);
    assert!(
        shipped,
        "not shipped: {:?}",
        sim.tasks()
            .iter()
            .map(|t| (
                t.item.state,
                t.item.stage_phase,
                t.open_blocker().map(|b| b.code)
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_poll_applies_a_new_saka_toml_and_records_a_broken_one_once() {
    let mut sim = Sim::multi_repo(polling(), &layout(&[shop("")]));
    assert_eq!(names(&sim), ["shop"]);
    let registered = sim.events_of("workspace.repo.registered").len();

    let one_poll_later = sim.clock.now().0 + i64::from(POLL_SECS) * 1000;
    let two = saka_toml("shop", &[shop("default = true\n"), web("")]);
    sim.world.put_doc("docs/saka.toml", two.as_bytes());
    let changed_commit = sim.world.base_head();
    assert!(sim.run_until(STEPS, |s| s.project().project.repos.len() == 2));
    assert!(
        sim.clock.now().0 <= one_poll_later,
        "applied after one poll"
    );
    assert_eq!(names(&sim), ["shop", "web"]);
    let changed = sim.events_of("workspace.project.config_changed");
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].payload["trigger"], "poll");
    assert_eq!(changed[0].payload["config_commit"], changed_commit);
    let repos = sim.events_of("workspace.repo.registered");
    assert_eq!(repos.len(), registered + 1);
    assert_eq!(repos.last().unwrap().payload["full_name"], WEB_REPO);
    let applied = sim.project().project;
    assert_eq!(applied.config_commit, changed_commit);

    sim.world
        .put_doc("docs/notes.md", b"same saka.toml, other commit\n");
    let seq = sim.engine().last_seq().unwrap();
    wait_one_poll(&mut sim);
    assert_eq!(
        sim.engine().last_seq().unwrap(),
        seq,
        "no event for the same content"
    );
    assert_eq!(sim.project().project.config_sha256, applied.config_sha256);

    sim.world
        .put_doc("docs/saka.toml", b"[project\nname = \"shop\"\n");
    let broken_commit = sim.world.base_head();
    wait_one_poll(&mut sim);
    let project = sim.project().project;
    let error = project.config_error.clone().unwrap();
    assert_eq!(
        (error.commit.as_str(), error.code.as_str()),
        (broken_commit.as_str(), "project_config_invalid")
    );
    assert_eq!(error.params["issues"][0]["code"], "toml_invalid");
    assert_eq!(
        project.repos, applied.repos,
        "the old config stays in force"
    );
    assert_eq!(project.config_sha256, applied.config_sha256);
    wait_one_poll(&mut sim);
    wait_one_poll(&mut sim);
    let rejected = sim.events_of("workspace.project.config_rejected");
    assert_eq!(rejected.len(), 1, "the same refusal is recorded once");
    assert_eq!(rejected[0].payload["trigger"], "poll");

    sim.world.put_doc("docs/saka.toml", two.as_bytes());
    let fixed_commit = sim.world.base_head();
    wait_one_poll(&mut sim);
    let cleared = sim.events_of("workspace.project.config_error_cleared");
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0].payload["commit"], fixed_commit);
    assert_eq!(sim.project().project.config_error, None);
    assert_eq!(sim.events_of("workspace.project.config_changed").len(), 1);
}

#[test]
fn a_poll_interval_of_zero_only_reloads_on_request() {
    let mut sim = Sim::multi_repo(SimConfig::default(), &layout(&[shop("")]));
    let two = saka_toml("shop", &[shop("default = true\n"), web("")]);
    sim.world.put_doc("docs/saka.toml", two.as_bytes());
    let until = sim.clock.now().0 + 3 * i64::from(POLL_SECS) * 1000;
    assert!(sim.run_until(STEPS, |s| s.clock.now().0 >= until));
    assert_eq!(names(&sim), ["shop"]);
    let reloaded = sim.reload().unwrap();
    assert_eq!(reloaded.outcome, ReloadOutcome::Changed);
    assert_eq!(names(&sim), ["shop", "web"]);
    let changed = sim.events_of("workspace.project.config_changed");
    assert_eq!(changed[0].payload["trigger"], "operator");
    assert_eq!(sim.reload().unwrap().outcome, ReloadOutcome::Unchanged);
}

#[test]
fn a_binding_with_live_tasks_is_kept_until_they_are_done() {
    let start = layout(&[shop(""), web("default = true\n")]);
    let mut sim = Sim::multi_repo(SimConfig::default(), &start);
    let web_id = sim.project().project.repos[0].repo_id;
    assert_eq!(names(&sim), ["web", "shop"], "the default comes first");
    let tasks = sim.approve();
    assert_eq!(tasks.len(), 2);

    sim.world
        .put_doc("docs/saka.toml", saka_toml("shop", &[shop("")]).as_bytes());
    let commit = sim.world.base_head();
    let Err(EngineError::Rejected(Rejection::ProjectRepoInUse { repos })) = sim.reload() else {
        panic!("a binding with queued tasks was removed");
    };
    let task_list: Vec<_> = sim
        .tasks()
        .iter()
        .map(|t| {
            json!({
                "work_item_id": t.item.id,
                "task_key": t.item.task_key,
                "state": t.item.state.as_str(),
            })
        })
        .collect();
    assert_eq!(
        repos,
        json!([{ "name": "web", "repo_id": web_id, "tasks": task_list }])
    );
    let error = sim.project().project.config_error.unwrap();
    assert_eq!(
        (error.commit.as_str(), error.code.as_str()),
        (commit.as_str(), "project_repo_in_use")
    );
    assert_eq!(names(&sim), ["web", "shop"]);

    assert!(sim.run_to_shipped(STEPS));
    assert!(
        sim.tasks()
            .iter()
            .all(|t| t.item.repo_id == web_id && t.item.state == WorkItemState::Done)
    );
    assert!(sim.world.prepared().iter().all(|p| p.repo == WEB_REPO));
    assert!(sim.world.prs().iter().all(|p| p.repo == WEB_REPO));

    let applied = sim.reload().unwrap();
    assert_eq!(applied.outcome, ReloadOutcome::Changed);
    assert_eq!(names(&sim), ["shop"]);
    assert_eq!(sim.project().project.config_error, None);
}

#[test]
fn a_binding_works_against_its_own_base_branch() {
    let mut start = layout(&[shop(""), web("default = true\nbase_branch = \"develop\"\n")]);
    start.branches = vec![(WEB_REPO.into(), "develop".into())];
    let mut sim = Sim::multi_repo(SimConfig::default(), &start);
    let main_before = sim.world.branches_of(WEB_REPO)["main"].clone();
    let bindings = sim.project().project.repos;
    assert_eq!(
        bindings
            .iter()
            .map(|b| (b.name.as_str(), b.base_branch.as_str()))
            .collect::<Vec<_>>(),
        [("web", "develop"), ("shop", "main")]
    );
    ship(&mut sim);

    let prepared = sim.world.prepared();
    assert_eq!(prepared.len(), 2);
    let develop = sim.world.branches_of(WEB_REPO)["develop"].clone();
    for p in &prepared {
        assert_eq!(
            (p.repo.as_str(), p.base_branch.as_str()),
            (WEB_REPO, "develop")
        );
        assert!(sim.world.is_ancestor(&p.base_sha, &develop));
    }
    let prs = sim.world.prs();
    assert_eq!(prs.len(), 2);
    for pr in &prs {
        assert_eq!(
            (pr.repo.as_str(), pr.base_branch.as_str()),
            (WEB_REPO, "develop")
        );
        let merge = pr.merge_commit_sha.clone().unwrap();
        assert!(sim.world.is_ancestor(&merge, &develop));
    }
    let reads = sim.world.recipe_reads();
    assert!(!reads.is_empty());
    for (repo, commit) in &reads {
        assert_eq!(repo, WEB_REPO);
        assert!(prepared.iter().any(|p| &p.base_sha == commit));
    }
    let checks = sim.world.merge_checks();
    assert!(!checks.is_empty());
    assert!(
        checks
            .iter()
            .all(|(repo, _, base)| repo == WEB_REPO && base == "develop")
    );
    assert_eq!(sim.world.branches_of(WEB_REPO)["main"], main_before);
}

#[test]
fn the_workspace_binding_defaults_to_the_project_base_branch() {
    let start = Layout {
        saka_toml: saka_toml("docs", &[repo_entry("docs", DOCS_REPO, "")]),
        branches: vec![(DOCS_REPO.into(), "trunk".into())],
        base_branch: Some("trunk".into()),
    };
    let mut sim = Sim::multi_repo(SimConfig::default(), &start);
    let project = sim.project().project;
    assert_eq!(project.base_branch, "trunk");
    assert_eq!(project.repos[0].base_branch, "trunk");
    assert_eq!(project.repos[0].repo_id, project.workspace_repo_id);
    ship(&mut sim);
    assert_eq!(sim.spec().state, SpecState::Shipped);
    assert!(
        sim.world
            .prepared()
            .iter()
            .all(|p| p.repo == DOCS_REPO && p.base_branch == "trunk")
    );
    assert!(sim.world.prs().iter().all(|p| p.base_branch == "trunk"));
    assert!(
        sim.world
            .merge_checks()
            .iter()
            .all(|(_, _, base)| base == "trunk")
    );
}

#[test]
fn a_reload_can_swap_the_default_and_rename_bindings() {
    let start = layout(&[shop("default = true\n"), web("")]);
    let mut sim = Sim::multi_repo(SimConfig::default(), &start);
    let before = sim.project().project.repos;
    let swapped = saka_toml(
        "storefront",
        &[
            repo_entry("api", SHOP_REPO, "path_scope = [\"services/**\"]\n"),
            repo_entry("site", WEB_REPO, "default = true\n"),
        ],
    );
    sim.world.put_doc("docs/saka.toml", swapped.as_bytes());
    assert_eq!(sim.reload().unwrap().outcome, ReloadOutcome::Changed);
    let project = sim.project().project;
    assert_eq!(project.name, "storefront");
    let after: Vec<_> = project
        .repos
        .iter()
        .map(|r| {
            (
                r.name.as_str(),
                r.repo_id,
                r.is_default,
                r.path_scope.clone(),
            )
        })
        .collect();
    assert_eq!(
        after,
        [
            ("site", before[1].repo_id, true, vec!["**".to_string()]),
            (
                "api",
                before[0].repo_id,
                false,
                vec!["services/**".to_string()]
            ),
        ]
    );
    sim.restart();
    assert_eq!(
        sim.project().project,
        project,
        "the projection survives a restart"
    );
}

const MULTI_PATH: &str = "docs/specs/multi.md";

fn multi_spec(repos: [&str; 3]) -> String {
    let rows: String = repos
        .iter()
        .enumerate()
        .map(|(i, repo)| {
            let n = i + 1;
            format!("| S{n} | Part {n} | AC-01 | {repo} | `make test` |\n")
        })
        .collect();
    format!(
        "---\nsaka: spec\n---\n# Multi\n\n## Acceptance criteria\n\n- **AC-01**: every part works.\n\n## Steps\n\n| Step | Work | AC | Repo | Done when |\n|------|------|----|------|-----------|\n{rows}"
    )
}

fn three_repos() -> Layout {
    layout(&[
        shop("default = true\n"),
        web(""),
        repo_entry("api", DOCS_REPO, ""),
    ])
}

fn repo_of(sim: &Sim, name: &str) -> saka_core::ids::RepoId {
    sim.project()
        .project
        .repos
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.repo_id)
        .unwrap()
}

#[test]
fn each_step_runs_in_the_repo_its_repo_cell_names() {
    let mut sim = Sim::multi_repo(SimConfig::default(), &three_repos());
    let spec = sim.register(MULTI_PATH, &multi_spec(["", "web", "api"]));
    assert!(spec.lint_ok, "{:?}", spec.issues);
    sim.approve_spec(spec.spec_id, spec.revision_id).unwrap();
    let placed: Vec<_> = sim
        .tasks_of(spec.spec_id)
        .iter()
        .map(|t| t.item.repo_id)
        .collect();
    assert_eq!(
        placed,
        [
            repo_of(&sim, "shop"),
            repo_of(&sim, "web"),
            repo_of(&sim, "api")
        ]
    );

    assert!(
        sim.run_until(STEPS, |s| s.spec_of(spec.spec_id).state
            == SpecState::Shipped),
        "not shipped: {:?}",
        sim.tasks_of(spec.spec_id)
            .iter()
            .map(|t| (t.item.state, t.open_blocker().map(|b| b.code)))
            .collect::<Vec<_>>()
    );
    assert!(
        sim.tasks_of(spec.spec_id)
            .iter()
            .all(|t| t.item.state == WorkItemState::Done)
    );
    let mut prepared: Vec<_> = sim.world.prepared().into_iter().map(|p| p.repo).collect();
    prepared.sort();
    assert_eq!(prepared, [DOCS_REPO, SHOP_REPO, WEB_REPO]);
    let mut prs: Vec<_> = sim
        .world
        .prs()
        .into_iter()
        .map(|p| (p.head_branch, p.repo, p.merge_commit_sha))
        .collect();
    prs.sort();
    let expected = [
        ("saka/multi/s1", SHOP_REPO),
        ("saka/multi/s2", WEB_REPO),
        ("saka/multi/s3", DOCS_REPO),
    ];
    assert_eq!(prs.len(), expected.len());
    for ((branch, repo, merge), (want_branch, want_repo)) in prs.iter().zip(expected) {
        assert_eq!((branch.as_str(), repo.as_str()), (want_branch, want_repo));
        let main = sim.world.branches_of(want_repo)["main"].clone();
        assert!(sim.world.is_ancestor(merge.as_ref().unwrap(), &main));
    }
    let mut checked: Vec<_> = sim
        .world
        .merge_checks()
        .into_iter()
        .map(|(repo, _, _)| repo)
        .collect();
    checked.sort();
    checked.dedup();
    assert_eq!(checked, [DOCS_REPO, SHOP_REPO, WEB_REPO]);
}

fn other_project_on(sim: &Sim, full_name: &str) -> saka_core::ids::ProjectId {
    sim.world.put_doc(
        "other/saka.toml",
        saka_toml("other", &[repo_entry("app", full_name, "")]).as_bytes(),
    );
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let add = saka_engine::AddProject {
        workspace_url: format!("https://github.com/{DOCS_REPO}"),
        project_path: "other".into(),
        base_branch: None,
    };
    sim.block_on(engine.add_project(&add, &cause, None))
        .unwrap()
        .project
        .id
}

#[test]
fn re_approval_moves_only_queued_steps_to_another_repo() {
    let mut sim = Sim::multi_repo(
        SimConfig {
            max_active_tasks: 1,
            ..SimConfig::default()
        },
        &three_repos(),
    );
    let first = sim.register(MULTI_PATH, &multi_spec(["", "web", "web"]));
    sim.approve_spec(first.spec_id, first.revision_id).unwrap();
    let state = |s: &Sim, n: usize| s.tasks_of(first.spec_id)[n - 1].item.state;
    assert!(sim.run_until(STEPS, |s| state(s, 1) == WorkItemState::InStage));
    assert_eq!(state(&sim, 2), WorkItemState::Queued);

    let started = sim.register(MULTI_PATH, &multi_spec(["web", "web", "web"]));
    let seq = sim.engine().last_seq().unwrap();
    let Err(EngineError::Rejected(Rejection::RevisionConflict { steps })) =
        sim.approve_spec(first.spec_id, started.revision_id)
    else {
        panic!("a started step changed its repo");
    };
    assert_eq!(steps[0]["step_id"], "S1");
    assert_eq!(steps[0]["fields"], json!(["repo"]));
    assert_eq!(sim.engine().last_seq().unwrap(), seq);

    let other = other_project_on(&sim, DOCS_REPO);
    sim.occupy_branch_in(other, repo_of(&sim, "api"), "saka/multi/s2");
    let taken = sim.register(MULTI_PATH, &multi_spec(["", "api", "web"]));
    let seq = sim.engine().last_seq().unwrap();
    let Err(EngineError::Rejected(Rejection::TaskBranchConflict { branches })) =
        sim.approve_spec(first.spec_id, taken.revision_id)
    else {
        panic!("the branch is taken in the target repo");
    };
    assert_eq!(branches, ["saka/multi/s2"]);
    assert_eq!(sim.engine().last_seq().unwrap(), seq);

    let moved = sim.register(MULTI_PATH, &multi_spec(["", "", "web"]));
    sim.approve_spec(first.spec_id, moved.revision_id).unwrap();
    let repinned = sim.events_of("delivery.work_item.repinned");
    let s2 = sim.tasks_of(first.spec_id)[1].item.clone();
    let shop_id = repo_of(&sim, "shop");
    let moved_event = repinned
        .iter()
        .find(|e| e.payload["work_item_id"] == json!(s2.id))
        .unwrap();
    assert_eq!(moved_event.payload["repo_id"], json!(shop_id));
    assert_eq!(s2.repo_id, shop_id);
    assert_eq!(s2.branch, "saka/multi/s2");

    assert!(
        sim.run_until(STEPS, |s| s.spec_of(first.spec_id).state
            == SpecState::Shipped),
        "not shipped"
    );
    let pr_repo = |branch: &str| {
        sim.world
            .prs()
            .into_iter()
            .find(|p| p.head_branch == branch)
            .map(|p| p.repo)
            .unwrap()
    };
    assert_eq!(pr_repo("saka/multi/s1"), SHOP_REPO);
    assert_eq!(pr_repo("saka/multi/s2"), SHOP_REPO);
    assert_eq!(pr_repo("saka/multi/s3"), WEB_REPO);
}
