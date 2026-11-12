#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use std::collections::BTreeMap;

use proptest::prelude::*;
use saka_core::autonomy::{Ceilings, RepoPolicy};
use saka_core::ids::{ProjectId, RepoId, SpecId, SpecRevisionId, WorkItemId};
use saka_core::model::{Task, WorkItem};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    Autonomy, Stage, Trigger, WorkItemOrigin, WorkItemState, WorkKind, WorkflowPreset,
};
use saka_core::workflow::{KindIssue, KindOverride, KindRefusal, Workflow, WorkflowPolicy};
use uuid::Uuid;

const OPEN: WorkflowPolicy = WorkflowPolicy {
    allow_unreviewed_chore: true,
};
const CLOSED: WorkflowPolicy = WorkflowPolicy {
    allow_unreviewed_chore: false,
};

fn with(
    kind: WorkKind,
    stages: &[(Stage, Option<Trigger>)],
    autonomy: Option<Autonomy>,
) -> Workflow {
    with_on(
        Workflow::of(WorkflowPreset::FullLoop),
        kind,
        stages,
        autonomy,
    )
}

fn with_on(
    mut workflow: Workflow,
    kind: WorkKind,
    stages: &[(Stage, Option<Trigger>)],
    autonomy: Option<Autonomy>,
) -> Workflow {
    workflow.kinds.insert(
        kind,
        KindOverride {
            stages: stages.iter().copied().collect(),
            autonomy,
            test_paths: Vec::new(),
        },
    );
    workflow
}

fn triggers(workflow: &Workflow) -> Vec<(Stage, Trigger)> {
    workflow
        .stages
        .iter()
        .map(|s| (s.stage, s.trigger))
        .collect()
}

fn task_of(kind: WorkKind, workflow: Workflow, requested: Option<Autonomy>) -> Task {
    let mut task = Task::new(WorkItem {
        id: WorkItemId(Uuid::from_u128(1)),
        project_id: ProjectId(Uuid::from_u128(2)),
        origin: WorkItemOrigin::SpecStep,
        kind: Some(kind),
        spec_id: Some(SpecId(Uuid::from_u128(3))),
        spec_revision_id: Some(SpecRevisionId(Uuid::from_u128(4))),
        step_id: Some("S1".into()),
        step_index: Some(1),
        task_key: "t/s1".into(),
        work: "work".into(),
        repo_id: RepoId(Uuid::from_u128(5)),
        branch: "saka/t/s1".into(),
        state: WorkItemState::Queued,
        stage: None,
        stage_phase: None,
        priority: 0,
        version: 1,
        admitted_at: None,
        done_at: None,
        start_approved_at: None,
        created_at: Timestamp(1),
        updated_at: Timestamp(1),
    });
    task.workflow = workflow.for_kind(kind, &OPEN);
    task.policy = RepoPolicy {
        full_name: "acme/app".into(),
        requested,
    };
    task
}

#[test]
fn a_kind_without_overrides_gets_the_project_workflow() {
    let project = Workflow::of(WorkflowPreset::FullLoop);
    for &kind in WorkKind::ALL
        .iter()
        .filter(|k| **k != WorkKind::SecurityPatch)
    {
        assert_eq!(project.for_kind(kind, &CLOSED), project, "{kind}");
    }
}

#[test]
fn a_security_patch_always_gets_review_and_the_strict_level() {
    let build_only = Workflow::of(WorkflowPreset::BuildOnly);
    let effective = build_only.for_kind(WorkKind::SecurityPatch, &CLOSED);
    assert_eq!(
        triggers(&effective),
        [(Stage::Dev, Trigger::Auto), (Stage::Review, Trigger::Auto)]
    );
    assert_eq!(effective.autonomy_cap, Some(Autonomy::Strict));
    assert_eq!(build_only.autonomy_cap, None);

    let full = Workflow::of(WorkflowPreset::FullLoop).for_kind(WorkKind::SecurityPatch, &CLOSED);
    assert_eq!(full.stages, Workflow::of(WorkflowPreset::FullLoop).stages);
    assert_eq!(full.autonomy_cap, Some(Autonomy::Strict));
}

#[test]
fn an_override_changes_a_trigger_and_can_add_review_and_fix_but_no_other_stage() {
    let build_only = Workflow::of(WorkflowPreset::BuildOnly);
    let widened = with_on(
        build_only.clone(),
        WorkKind::Bugfix,
        &[
            (Stage::Review, Some(Trigger::Auto)),
            (Stage::Fix, Some(Trigger::Auto)),
        ],
        None,
    );
    assert_eq!(
        triggers(&widened.for_kind(WorkKind::Bugfix, &CLOSED)),
        [
            (Stage::Dev, Trigger::Auto),
            (Stage::Review, Trigger::Auto),
            (Stage::Fix, Trigger::Auto)
        ]
    );
    assert_eq!(widened.kind_issues(&CLOSED), []);

    let merge = with_on(
        build_only,
        WorkKind::Bugfix,
        &[(Stage::Merge, Some(Trigger::Manual))],
        None,
    );
    assert_eq!(
        triggers(&merge.for_kind(WorkKind::Bugfix, &CLOSED)),
        [(Stage::Dev, Trigger::Auto)]
    );
    assert_eq!(
        merge.kind_issues(&CLOSED),
        [KindIssue {
            kind: WorkKind::Bugfix,
            stage: Some(Stage::Merge),
            refusal: KindRefusal::StageNotAllowed
        }]
    );

    let manual = with(
        WorkKind::Refactor,
        &[(Stage::Merge, Some(Trigger::Manual)), (Stage::Fix, None)],
        None,
    );
    assert_eq!(
        triggers(&manual.for_kind(WorkKind::Refactor, &CLOSED)),
        [
            (Stage::Dev, Trigger::Auto),
            (Stage::Review, Trigger::Auto),
            (Stage::Merge, Trigger::Manual)
        ]
    );
    assert_eq!(manual.kind_issues(&CLOSED), []);
}

#[test]
fn a_chore_skips_review_only_when_the_operator_allows_it() {
    let workflow = with(WorkKind::Chore, &[(Stage::Review, None)], None);
    assert_eq!(
        workflow.kind_issues(&CLOSED),
        [KindIssue {
            kind: WorkKind::Chore,
            stage: Some(Stage::Review),
            refusal: KindRefusal::ReviewNotAllowed
        }]
    );
    assert!(
        workflow
            .for_kind(WorkKind::Chore, &CLOSED)
            .has_stage(Stage::Review)
    );

    assert_eq!(workflow.kind_issues(&OPEN), []);
    let unreviewed = workflow.for_kind(WorkKind::Chore, &OPEN);
    assert_eq!(
        triggers(&unreviewed),
        [(Stage::Dev, Trigger::Auto), (Stage::Merge, Trigger::Auto)],
        "the fix stage has no review to answer to, so it goes with it"
    );
    assert!(
        workflow
            .for_kind(WorkKind::Feature, &OPEN)
            .has_stage(Stage::Review)
    );
}

#[test]
fn review_cannot_be_turned_off_for_any_other_kind() {
    for kind in [
        WorkKind::Feature,
        WorkKind::Bugfix,
        WorkKind::Refactor,
        WorkKind::SecurityPatch,
    ] {
        let workflow = with(kind, &[(Stage::Review, None)], None);
        assert_eq!(
            workflow.kind_issues(&OPEN),
            [KindIssue {
                kind,
                stage: Some(Stage::Review),
                refusal: KindRefusal::ReviewRequired
            }],
            "{kind}"
        );
        assert!(
            workflow.for_kind(kind, &OPEN).has_stage(Stage::Review),
            "{kind}"
        );
    }
}

#[test]
fn the_dev_stage_cannot_be_turned_off() {
    let workflow = with(WorkKind::Bugfix, &[(Stage::Dev, None)], None);
    assert_eq!(
        workflow.kind_issues(&OPEN),
        [KindIssue {
            kind: WorkKind::Bugfix,
            stage: Some(Stage::Dev),
            refusal: KindRefusal::StageRequired
        }]
    );
    assert!(
        workflow
            .for_kind(WorkKind::Bugfix, &OPEN)
            .has_stage(Stage::Dev)
    );
}

#[test]
fn a_fix_stage_the_override_adds_needs_a_review_stage() {
    let build_only = with_on(
        Workflow::of(WorkflowPreset::BuildOnly),
        WorkKind::Refactor,
        &[(Stage::Fix, Some(Trigger::Auto))],
        None,
    );
    let issues = build_only.kind_issues(&OPEN);
    assert_eq!(
        issues,
        [KindIssue {
            kind: WorkKind::Refactor,
            stage: Some(Stage::Fix),
            refusal: KindRefusal::FixWithoutReview
        }]
    );
    assert_eq!(issues[0].refusal.code(), "workflow_fix_without_review");
    assert!(
        !build_only
            .for_kind(WorkKind::Refactor, &OPEN)
            .has_stage(Stage::Fix)
    );
}

#[test]
fn the_refusals_name_their_config_codes() {
    let codes: Vec<_> = [
        KindRefusal::StageRequired,
        KindRefusal::ReviewRequired,
        KindRefusal::ReviewNotAllowed,
        KindRefusal::StageNotAllowed,
        KindRefusal::FixWithoutReview,
    ]
    .iter()
    .map(|r| r.code())
    .collect();
    assert_eq!(
        codes,
        [
            "kind_stage_required",
            "kind_review_required",
            "kind_review_not_allowed",
            "workflow_stage_not_allowed",
            "workflow_fix_without_review"
        ]
    );
}

#[test]
fn the_gate_and_the_rounds_never_change_with_the_kind() {
    let mut project = Workflow::of(WorkflowPreset::FullLoop);
    project.fix_rounds = 5;
    let tuned = with_on(
        project.clone(),
        WorkKind::Bugfix,
        &[
            (Stage::Review, Some(Trigger::Auto)),
            (Stage::Merge, Some(Trigger::Manual)),
        ],
        Some(Autonomy::Strict),
    );
    for &kind in WorkKind::ALL {
        let effective = tuned.for_kind(kind, &OPEN);
        assert_eq!(effective.fix_rounds, 5);
        assert_eq!(effective.spec, project.spec);
        assert_eq!(effective.change_request, project.change_request);
        assert_eq!(effective.preset, project.preset);
        assert!(effective.kinds.is_empty());
    }
}

#[test]
fn an_override_never_raises_the_level_of_the_repo() {
    let ceilings = Ceilings {
        default: Autonomy::Standard,
        repos: Vec::new(),
    };
    let task = task_of(
        WorkKind::Refactor,
        with(WorkKind::Refactor, &[], Some(Autonomy::Autonomous)),
        None,
    );
    assert_eq!(task.autonomy(&ceilings), Autonomy::Standard);

    let lowered = task_of(
        WorkKind::Refactor,
        with(WorkKind::Refactor, &[], Some(Autonomy::Strict)),
        Some(Autonomy::Autonomous),
    );
    assert_eq!(lowered.autonomy(&ceilings), Autonomy::Strict);

    let patch = task_of(
        WorkKind::SecurityPatch,
        Workflow::of(WorkflowPreset::FullLoop),
        Some(Autonomy::Autonomous),
    );
    let open = Ceilings {
        default: Autonomy::Autonomous,
        repos: Vec::new(),
    };
    assert_eq!(patch.autonomy(&open), Autonomy::Strict);
    assert!(patch.needs_start_approval(&open));

    let feature = task_of(
        WorkKind::Feature,
        Workflow::of(WorkflowPreset::FullLoop),
        None,
    );
    assert_eq!(feature.autonomy(&open), Autonomy::Autonomous);
}

#[test]
fn the_overrides_round_trip_inside_the_stored_workflow() {
    let workflow = with(
        WorkKind::Bugfix,
        &[(Stage::Review, Some(Trigger::Manual)), (Stage::Fix, None)],
        Some(Autonomy::Standard),
    );
    let text = serde_json::to_string(&workflow).unwrap();
    assert_eq!(serde_json::from_str::<Workflow>(&text).unwrap(), workflow);
    assert!(text.contains(r#""kinds":{"bugfix":"#));
    assert!(
        !serde_json::to_string(&Workflow::default())
            .unwrap()
            .contains("kinds")
    );
}

fn any_preset() -> impl Strategy<Value = WorkflowPreset> {
    prop_oneof![
        Just(WorkflowPreset::FullLoop),
        Just(WorkflowPreset::BuildOnly),
        Just(WorkflowPreset::ReviewOnly),
        Just(WorkflowPreset::Custom),
    ]
}

fn any_kind() -> impl Strategy<Value = WorkKind> {
    prop::sample::select(WorkKind::ALL.to_vec())
}

fn any_stage() -> impl Strategy<Value = Stage> {
    prop::sample::select(vec![
        Stage::Dev,
        Stage::Review,
        Stage::Fix,
        Stage::Merge,
        Stage::Sync,
    ])
}

fn any_trigger() -> impl Strategy<Value = Option<Trigger>> {
    prop_oneof![
        Just(None),
        Just(Some(Trigger::Auto)),
        Just(Some(Trigger::Manual))
    ]
}

fn any_level() -> impl Strategy<Value = Option<Autonomy>> {
    prop_oneof![
        Just(None),
        Just(Some(Autonomy::Strict)),
        Just(Some(Autonomy::Standard)),
        Just(Some(Autonomy::Autonomous)),
    ]
}

fn any_workflow() -> impl Strategy<Value = Workflow> {
    (
        any_preset(),
        prop::collection::vec((any_stage(), any_trigger()), 0..8),
        any_level(),
        any_kind(),
    )
        .prop_map(|(preset, stages, autonomy, kind)| {
            let mut workflow = Workflow::of(preset);
            let overrides = KindOverride {
                stages: stages.into_iter().collect::<BTreeMap<_, _>>(),
                autonomy,
                test_paths: Vec::new(),
            };
            workflow.kinds.insert(kind, overrides);
            workflow
        })
}

proptest! {
    #[test]
    fn a_kind_never_raises_the_level_widens_the_gate_or_drops_a_required_stage(
        project in any_workflow(),
        kind in any_kind(),
        allow in any::<bool>(),
        ceiling in prop::sample::select(vec![Autonomy::Strict, Autonomy::Standard, Autonomy::Autonomous]),
        requested in any_level(),
    ) {
        let policy = WorkflowPolicy { allow_unreviewed_chore: allow };
        let effective = project.for_kind(kind, &policy);
        prop_assert_eq!(&effective, &project.for_kind(kind, &policy));
        prop_assert!(effective.kinds.is_empty());
        prop_assert_eq!(effective.spec, project.spec);
        prop_assert_eq!(effective.fix_rounds, project.fix_rounds);
        prop_assert_eq!(&effective.change_request, &project.change_request);
        prop_assert_eq!(effective.has_stage(Stage::Dev), project.has_stage(Stage::Dev));
        prop_assert!(!effective.has_stage(Stage::Fix) || effective.has_stage(Stage::Review));
        for stage in [Stage::Merge, Stage::Sync] {
            prop_assert!(!effective.has_stage(stage) || project.has_stage(stage));
        }
        if kind == WorkKind::SecurityPatch {
            prop_assert!(effective.has_stage(Stage::Review));
        } else if !(kind == WorkKind::Chore && allow) && project.has_stage(Stage::Review) {
            prop_assert!(effective.has_stage(Stage::Review));
        }

        let task = task_of(kind, project, requested);
        let ceilings = Ceilings { default: ceiling, repos: Vec::new() };
        let level = task.autonomy(&ceilings);
        prop_assert!(level <= ceiling);
        if let Some(requested) = requested {
            prop_assert!(level <= requested);
        }
        if kind == WorkKind::SecurityPatch {
            prop_assert_eq!(level, Autonomy::Strict);
        }
    }

    #[test]
    fn a_workflow_without_issues_resolves_exactly_as_written(
        project in any_workflow(),
        kind in any_kind(),
        allow in any::<bool>(),
    ) {
        let policy = WorkflowPolicy { allow_unreviewed_chore: allow };
        if project.kind_issues(&policy).is_empty() {
            let effective = project.for_kind(kind, &policy);
            let overrides = project.kinds.get(&kind);
            for (stage, trigger) in overrides.iter().flat_map(|o| o.stages.iter()) {
                prop_assert_eq!(effective.stage_trigger(*stage), *trigger);
            }
        }
    }
}
