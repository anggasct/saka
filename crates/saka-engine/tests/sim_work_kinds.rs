#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::rejection::Rejection;
use saka_core::vocab::{ReloadOutcome, Stage, Trigger, WorkItemState, WorkKind};
use saka_core::workflow::WorkflowPolicy;
use saka_engine::EngineError;
use saka_testkit::{Layout, SHOP_REPO, Sim, SimConfig, one_step_spec, repo_entry, saka_toml};

const BUGFIX_BODY: &str = "\n## Regression\n\n- `make test-regression`\n";

fn config(workflow: &str) -> String {
    let mut text = saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]);
    text.push('\n');
    text.push_str(workflow);
    text
}

fn sim(workflow: &str, policy: WorkflowPolicy) -> Sim {
    Sim::multi_repo(
        SimConfig {
            workflow_policy: policy,
            ..SimConfig::default()
        },
        &Layout {
            saka_toml: config(workflow),
            ..Layout::default()
        },
    )
}

fn spec_text(title: &str, frontmatter: &str, body: &str) -> String {
    format!("{}{body}", one_step_spec(title, frontmatter))
}

fn rejected_issues(result: Result<saka_engine::ReloadView, EngineError>) -> Vec<(String, String)> {
    let Err(EngineError::Rejected(Rejection::ProjectConfigInvalid { issues })) = result else {
        panic!("the config was applied");
    };
    issues
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["code"].as_str().unwrap().to_string(),
                i["field"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn column(sim: &Sim, sql: &str) -> Vec<Option<String>> {
    sim.engine()
        .store()
        .read(|c| {
            let mut stmt = c.prepare(sql)?;
            let rows = stmt
                .query_map([], |r| r.get::<_, Option<String>>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .unwrap()
}

#[test]
fn approving_a_chore_records_its_kind_on_the_spec_and_on_the_task() {
    let sim = sim("", WorkflowPolicy::default());
    let chore = sim.register(
        "docs/specs/tidy-deps.md",
        &spec_text("Tidy deps", "kind: chore\n", ""),
    );
    assert!(chore.lint_ok, "{:?}", chore.issues);
    assert_eq!(sim.spec_of(chore.spec_id).kind, WorkKind::Chore);
    assert_eq!(
        sim.events_of("spec.revision.recorded")
            .iter()
            .map(|e| e.payload["kind"].clone())
            .collect::<Vec<_>>(),
        ["feature", "chore"]
    );

    let ids = sim.approve_spec(chore.spec_id, chore.revision_id).unwrap();
    assert_eq!(ids.len(), 1);
    let tasks = sim.tasks_of(chore.spec_id);
    assert_eq!(tasks[0].item.kind, Some(WorkKind::Chore));
    let approved = sim.events_of("spec.spec.approved");
    assert_eq!(approved.last().unwrap().payload["kind"], "chore");
    let created = sim.events_of("delivery.work_item.created");
    assert_eq!(created.last().unwrap().payload["kind"], "chore");
    assert_eq!(
        column(&sim, "SELECT kind FROM spec WHERE slug = 'tidy-deps'"),
        [Some("chore".to_string())]
    );
    assert_eq!(
        column(
            &sim,
            "SELECT kind FROM work_item WHERE task_key = 'tidy-deps/s1'"
        ),
        [Some("chore".to_string())]
    );
}

#[test]
fn a_spec_without_a_kind_is_a_feature_and_so_is_its_task() {
    let mut sim = sim("", WorkflowPolicy::default());
    sim.approve();
    assert_eq!(sim.spec_of(sim.spec_id).kind, WorkKind::Feature);
    assert_eq!(sim.tasks()[0].item.kind, Some(WorkKind::Feature));
}

#[test]
fn a_new_revision_with_another_kind_repins_a_task_that_has_not_started() {
    let sim = sim("", WorkflowPolicy::default());
    let first = sim.register(
        "docs/specs/login.md",
        &spec_text("Login", "kind: refactor\n", ""),
    );
    sim.approve_spec(first.spec_id, first.revision_id).unwrap();
    assert_eq!(
        sim.tasks_of(first.spec_id)[0].item.kind,
        Some(WorkKind::Refactor)
    );

    let second = sim.register(
        "docs/specs/login.md",
        &spec_text("Login loop", "kind: bugfix\n", BUGFIX_BODY),
    );
    assert!(second.lint_ok, "{:?}", second.issues);
    assert_eq!(sim.spec_of(first.spec_id).kind, WorkKind::Bugfix);
    sim.approve_spec(first.spec_id, second.revision_id).unwrap();
    let task = &sim.tasks_of(first.spec_id)[0];
    assert_eq!(task.item.kind, Some(WorkKind::Bugfix));
    assert_eq!(task.item.spec_revision_id, Some(second.revision_id));
    let repinned = sim.events_of("delivery.work_item.repinned");
    assert_eq!(repinned.last().unwrap().payload["kind"], "bugfix");
}

#[test]
fn kind_overrides_are_stored_on_the_project_and_shape_the_workflow_of_that_kind_only() {
    let sim = sim(
        "[workflow.kinds.bugfix]\nstages = { merge = \"manual\" }\nautonomy = \"strict\"\n",
        WorkflowPolicy::default(),
    );
    let stored = sim.project().project.workflow;
    assert_eq!(stored.kinds.len(), 1);
    assert_eq!(
        sim.events_of("workspace.project.registered")[0].payload["workflow"]["kinds"]["bugfix"]["stages"]
            ["merge"],
        "manual"
    );

    let bugfix = sim.register(
        "docs/specs/fix.md",
        &spec_text("Fix", "kind: bugfix\n", BUGFIX_BODY),
    );
    let feature = sim.register("docs/specs/feat.md", &spec_text("Feat", "", ""));
    sim.approve_spec(bugfix.spec_id, bugfix.revision_id)
        .unwrap();
    sim.approve_spec(feature.spec_id, feature.revision_id)
        .unwrap();
    let fix_task = &sim.tasks_of(bugfix.spec_id)[0];
    let feat_task = &sim.tasks_of(feature.spec_id)[0];
    assert_eq!(
        fix_task.workflow.stage_trigger(Stage::Merge),
        Some(Trigger::Manual)
    );
    assert_eq!(
        feat_task.workflow.stage_trigger(Stage::Merge),
        Some(Trigger::Auto)
    );
    assert!(fix_task.workflow.kinds.is_empty());
}

#[test]
fn a_reload_with_other_overrides_changes_the_workflow_of_tasks_that_have_not_started() {
    let sim = sim(
        "[workflow.kinds.bugfix]\nstages = { merge = \"manual\" }\n",
        WorkflowPolicy::default(),
    );
    let bugfix = sim.register(
        "docs/specs/fix.md",
        &spec_text("Fix", "kind: bugfix\n", BUGFIX_BODY),
    );
    sim.approve_spec(bugfix.spec_id, bugfix.revision_id)
        .unwrap();
    let merge = |sim: &Sim| {
        sim.tasks_of(bugfix.spec_id)[0]
            .workflow
            .stage_trigger(Stage::Merge)
    };
    assert_eq!(merge(&sim), Some(Trigger::Manual));

    sim.world.put_doc("docs/saka.toml", config("").as_bytes());
    assert_eq!(sim.reload().unwrap().outcome, ReloadOutcome::Changed);
    assert_eq!(merge(&sim), Some(Trigger::Auto));
    assert!(sim.project().project.workflow.kinds.is_empty());
}

#[test]
fn an_unknown_kind_or_key_rejects_the_reload_and_the_old_overrides_stay() {
    let sim = sim(
        "[workflow.kinds.bugfix]\nstages = { merge = \"manual\" }\n",
        WorkflowPolicy::default(),
    );
    for (workflow, expected) in [
        (
            "[workflow.kinds.hotfix]\nstages = {}\n",
            ("field_unknown", "workflow.kinds.hotfix"),
        ),
        (
            "[workflow.kinds.bugfix]\nretries = 1\n",
            ("field_unknown", "workflow.kinds.bugfix.retries"),
        ),
    ] {
        sim.world
            .put_doc("docs/saka.toml", config(workflow).as_bytes());
        let issues = rejected_issues(sim.reload());
        assert_eq!(issues, [(expected.0.to_string(), expected.1.to_string())]);
        assert_eq!(sim.project().project.workflow.kinds.len(), 1);
    }
}

#[test]
fn the_operator_decides_whether_a_chore_may_skip_review() {
    let skip = "[workflow.kinds.chore]\nstages = { review = \"off\" }\n";
    let closed = sim("", WorkflowPolicy::default());
    closed
        .world
        .put_doc("docs/saka.toml", config(skip).as_bytes());
    assert_eq!(
        rejected_issues(closed.reload()),
        [(
            "kind_review_not_allowed".to_string(),
            "workflow.kinds.chore.stages.review".to_string()
        )]
    );

    let open = sim(
        skip,
        WorkflowPolicy {
            allow_unreviewed_chore: true,
        },
    );
    let chore = open.register(
        "docs/specs/tidy.md",
        &spec_text("Tidy", "kind: chore\n", ""),
    );
    open.approve_spec(chore.spec_id, chore.revision_id).unwrap();
    let task = &open.tasks_of(chore.spec_id)[0];
    assert!(!task.workflow.has_stage(Stage::Review));
    assert!(!task.workflow.has_stage(Stage::Fix));
    assert!(open.project().allow_unreviewed_chore);
    assert!(!closed.project().allow_unreviewed_chore);
}

#[test]
fn a_rule_the_file_breaks_is_refused_with_its_code_and_field() {
    let sim = sim("", WorkflowPolicy::default());
    for (workflow, code, field) in [
        (
            "[workflow.kinds.bugfix]\nstages = { review = \"off\" }\n",
            "kind_review_required",
            "workflow.kinds.bugfix.stages.review",
        ),
        (
            "[workflow.kinds.security_patch]\nstages = { review = \"off\" }\n",
            "kind_review_required",
            "workflow.kinds.security_patch.stages.review",
        ),
        (
            "[workflow.kinds.feature]\nstages = { dev = \"off\" }\n",
            "kind_stage_required",
            "workflow.kinds.feature.stages.dev",
        ),
        (
            "[workflow.kinds.bugfix]\ntest_paths = [\"[bad\"]\n",
            "kind_test_paths_invalid",
            "workflow.kinds.bugfix.test_paths[0]",
        ),
    ] {
        sim.world
            .put_doc("docs/saka.toml", config(workflow).as_bytes());
        assert_eq!(
            rejected_issues(sim.reload()),
            [(code.to_string(), field.to_string())]
        );
    }
}

const STEPS: usize = 12_000;

#[test]
fn a_security_patch_waits_for_the_operator_and_is_reviewed_whatever_the_project_says() {
    let mut cfg = SimConfig::default();
    cfg.operator.start = false;
    let mut sim = Sim::multi_repo(
        cfg,
        &Layout {
            saka_toml: config("[workflow]\npreset = \"build_only\"\n"),
            ..Layout::default()
        },
    );
    let patch = sim.register(
        "docs/specs/close-hole.md",
        &spec_text("Close hole", "kind: security_patch\n", ""),
    );
    sim.approve_spec(patch.spec_id, patch.revision_id).unwrap();
    let task = sim.tasks_of(patch.spec_id).remove(0);
    assert!(task.workflow.has_stage(Stage::Review));
    assert!(!task.workflow.has_stage(Stage::Merge));
    assert!(sim.run_until(STEPS, |s| {
        s.tasks_of(patch.spec_id)[0].item.state == WorkItemState::AwaitingStart
    }));
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let waiting = sim.tasks_of(patch.spec_id).remove(0);
    engine
        .start_step_with(waiting.item.id, waiting.item.version, &cause, None)
        .unwrap();
    drop(engine);
    assert!(
        sim.run_until(STEPS, |s| !s
            .events_of("delivery.review.recorded")
            .is_empty()),
        "the review never ran although a security patch always has one"
    );
}
