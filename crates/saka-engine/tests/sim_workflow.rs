#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::rejection::Rejection;
use saka_core::vocab::{ReloadOutcome, Stage, StagePhase, WorkItemState, WorkflowPreset};
use saka_core::workflow::Workflow;
use saka_engine::EngineError;
use saka_ports::Clock;
use saka_testkit::{Layout, SHOP_REPO, Sim, SimConfig, repo_entry, saka_toml};
use serde_json::json;

const STEPS: usize = 8_000;

fn with_workflow(workflow: &str) -> String {
    let mut text = saka_toml("shop", &[repo_entry("shop", SHOP_REPO, "")]);
    text.push('\n');
    text.push_str(workflow);
    text
}

fn layout(workflow: &str) -> Layout {
    Layout {
        saka_toml: with_workflow(workflow),
        ..Layout::default()
    }
}

const REVIEW_ONLY: &str = "[workflow]\npreset = \"review_only\"\n\n[workflow.change_request]\nlabels = [\"needs-review\"]\n";
const BUILD_ONLY: &str = "[workflow]\npreset = \"build_only\"\n";

#[test]
fn a_project_registered_without_a_workflow_section_runs_the_full_loop() {
    let sim = Sim::multi_repo(SimConfig::default(), &layout(""));
    assert_eq!(sim.project().project.workflow, Workflow::default());
}

#[test]
fn the_workflow_in_saka_toml_is_stored_on_registration_and_follows_every_reload() {
    let sim = Sim::multi_repo(SimConfig::default(), &layout(REVIEW_ONLY));
    let stored = sim.project().project.workflow;
    assert_eq!(stored.preset, WorkflowPreset::ReviewOnly);
    let entry = stored.change_request.unwrap();
    assert_eq!(entry.labels, ["needs-review"]);
    assert_eq!(
        sim.events_of("workspace.project.registered")[0].payload["workflow"]["preset"],
        "review_only"
    );

    sim.world
        .put_doc("docs/saka.toml", with_workflow(BUILD_ONLY).as_bytes());
    assert_eq!(sim.reload().unwrap().outcome, ReloadOutcome::Changed);
    let after = sim.project().project.workflow;
    assert_eq!(after, Workflow::of(WorkflowPreset::BuildOnly));
    let changed = sim.events_of("workspace.project.config_changed");
    assert_eq!(changed[0].payload["workflow"]["preset"], "build_only");
    assert_eq!(sim.reload().unwrap().outcome, ReloadOutcome::Unchanged);
}

#[test]
fn a_workflow_that_means_nothing_is_rejected_and_the_old_one_stays() {
    let sim = Sim::multi_repo(SimConfig::default(), &layout(REVIEW_ONLY));
    let bad =
        "[workflow]\npreset = \"custom\"\nspec = true\n\n[workflow.stages]\nreview = \"auto\"\n";
    sim.world
        .put_doc("docs/saka.toml", with_workflow(bad).as_bytes());
    let Err(EngineError::Rejected(Rejection::ProjectConfigInvalid { issues })) = sim.reload()
    else {
        panic!("a workflow with a spec entry and no dev stage was applied");
    };
    assert_eq!(issues[0]["code"], "workflow_spec_without_dev");
    assert_eq!(issues[0]["field"], "workflow");
    assert_eq!(
        sim.project().project.workflow.preset,
        WorkflowPreset::ReviewOnly
    );
    let error = sim.project().project.config_error.unwrap();
    assert_eq!(error.code, "project_config_invalid");
    assert_eq!(
        sim.events_of("workspace.project.config_rejected")[0].payload["params"]["issues"][0]["code"],
        json!("workflow_spec_without_dev")
    );
}

#[test]
fn a_spec_is_not_approved_when_the_workflow_has_no_spec_entry() {
    let sim = Sim::multi_repo(SimConfig::default(), &layout(REVIEW_ONLY));
    let refused = sim.approve_spec(sim.spec_id, sim.revision_id);
    assert!(matches!(
        refused,
        Err(EngineError::Rejected(Rejection::EntryPointDisabled {
            entry: "spec"
        }))
    ));
    assert!(sim.events_of("spec.spec.approved").is_empty());
    assert!(sim.tasks().is_empty());
}

#[test]
fn a_build_only_project_never_merges_but_follows_the_merge_made_on_the_host() {
    let mut sim = Sim::multi_repo(SimConfig::default(), &layout(BUILD_ONLY));
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));

    let task = sim.task(1);
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let refused = engine.request_merge(task.item.id, task.item.version, None, &cause);
    assert!(matches!(
        refused,
        Err(EngineError::Rejected(Rejection::StageNotInWorkflow {
            stage: "merge"
        }))
    ));
    let detail = engine.work_item(task.item.id).unwrap().unwrap();
    assert!(!detail.merges, "the view says Saka does not merge here");

    let more = sim.engine().now().0 + 120_000;
    assert!(sim.run_until(STEPS, |s| s.clock.now().0 >= more));
    assert!(
        sim.world.merge_calls().is_empty(),
        "Saka never called merge"
    );
    assert!(sim.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge));

    sim.world
        .merge_outside(task.change_request.as_ref().unwrap().number);
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::Done));
    assert!(sim.world.merge_calls().is_empty());
}

#[test]
fn the_full_loop_merges_as_before_and_offers_the_action() {
    let mut sim = Sim::multi_repo(SimConfig::default(), &layout(""));
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    assert_eq!(sim.world.merge_calls().len(), 2);
}

const ISSUE_ENTRY: &str = "[workflow.issue]\ntrigger_label = \"run-saka\"\nmax_open = 3\n";

#[test]
fn an_issue_entry_is_registered_follows_reloads_and_leaves_with_its_section() {
    let sim = Sim::multi_repo(SimConfig::default(), &layout(ISSUE_ENTRY));
    let entry = sim.project().project.workflow.issue.unwrap();
    assert_eq!(
        (entry.trigger_label.as_str(), entry.max_open),
        ("run-saka", 3)
    );
    assert_eq!(
        sim.events_of("workspace.project.registered")[0].payload["workflow"]["issue"]["max_open"],
        3
    );

    sim.world.put_doc(
        "docs/saka.toml",
        with_workflow("[workflow.issue]\nmax_open = 9\n").as_bytes(),
    );
    assert_eq!(sim.reload().unwrap().outcome, ReloadOutcome::Changed);
    assert_eq!(sim.project().project.workflow.issue.unwrap().max_open, 9);

    sim.world
        .put_doc("docs/saka.toml", with_workflow("").as_bytes());
    assert_eq!(sim.reload().unwrap().outcome, ReloadOutcome::Changed);
    assert!(sim.project().project.workflow.issue.is_none());
}

#[test]
fn a_bad_issue_entry_is_rejected_by_code_and_the_old_workflow_stays() {
    let sim = Sim::multi_repo(SimConfig::default(), &layout(ISSUE_ENTRY));
    for (workflow, code, field) in [
        (
            "[workflow]\npreset = \"review_only\"\n\n[workflow.issue]\n",
            "workflow_entry_not_allowed",
            "workflow.issue",
        ),
        (
            "[workflow.issue]\nkind_labels = { bug = \"hotfix\" }\n",
            "kind_invalid",
            "workflow.issue.kind_labels.bug",
        ),
        (
            "[workflow.issue]\nreviewers = 2\n",
            "field_unknown",
            "workflow.issue.reviewers",
        ),
    ] {
        sim.world
            .put_doc("docs/saka.toml", with_workflow(workflow).as_bytes());
        let Err(EngineError::Rejected(Rejection::ProjectConfigInvalid { issues })) = sim.reload()
        else {
            panic!("{code} was applied");
        };
        assert_eq!(
            (issues[0]["code"].clone(), issues[0]["field"].clone()),
            (json!(code), json!(field))
        );
        assert_eq!(sim.project().project.workflow.issue.unwrap().max_open, 3);
    }
}
