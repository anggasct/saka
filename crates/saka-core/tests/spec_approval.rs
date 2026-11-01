#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use proptest::prelude::*;
use saka_core::Ctx;
use saka_core::admission::waiting_on;
use saka_core::event::Event;
use saka_core::ids::{IdSource, ProjectId, RepoId, SpecId, SpecRevisionId, WorkItemId};
use saka_core::model::{RevisionView, Spec, StepPlan, Task, WorkItem, WorkItemDependency};
use saka_core::plan::{PlanStep, RevisionPlan, plan_revision};
use saka_core::reconcile::{Action, Snapshot, reconcile};
use saka_core::rejection::Rejection;
use saka_core::spec::{self, Approval};
use saka_core::task::{self, LoopConfig, TaskInput};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    ConflictReason, MergeMethod, PlanAction, SpecOrigin, SpecState, StepField, SupersedeReason,
    WorkItemOrigin, WorkItemState,
};
use serde_json::json;
use uuid::Uuid;

struct Counter(u128);

impl IdSource for Counter {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

const PROJECT: ProjectId = ProjectId(Uuid::from_u128(900));
const REPO: RepoId = RepoId(Uuid::from_u128(901));
const SPEC: SpecId = SpecId(Uuid::from_u128(1));
const FROM: SpecRevisionId = SpecRevisionId(Uuid::from_u128(10));
const TO: SpecRevisionId = SpecRevisionId(Uuid::from_u128(11));
const NOW: Timestamp = Timestamp(5_000);
const APPROVED_AT: Timestamp = Timestamp(1_000);

fn cfg() -> LoopConfig {
    LoopConfig {
        max_followup_turns: 1,
        permission_wait_ms: 24 * 3_600_000,
        idle_ms: 30 * 60_000,
        budgets: std::collections::BTreeMap::from([(
            saka_core::vocab::Stage::Dev,
            saka_core::model::Budget {
                running_minutes: 60,
                turns: 10,
                tokens: 0,
                cost_usd_micros: 0,
            },
        )]),
        budget_warn_percent: 80,
        rate_limit_pause: 0.9,
        account: "claude:default".into(),
        ci_appear_timeout_ms: 600_000,
        poll_interval_ms: 15_000,
        merge_method: MergeMethod::Squash,
    }
}

fn step(n: u32) -> StepPlan {
    StepPlan {
        id: format!("S{n}"),
        index: n,
        work: format!("work {n}"),
        ac: vec![format!("AC-0{n}")],
        commands: vec![format!("make s{n}")],
        after: if n == 1 {
            vec![]
        } else {
            vec![format!("S{}", n - 1)]
        },
    }
}

fn revision(id: SpecRevisionId, steps: Vec<StepPlan>, priority: i64) -> RevisionView {
    RevisionView {
        id,
        lint_ok: true,
        issues: json!([]),
        steps,
        parser_version: 2,
        priority,
        depends_on: vec![],
    }
}

fn steps(n: u32) -> Vec<StepPlan> {
    (1..=n).map(step).collect()
}

fn approved_spec(latest: SpecRevisionId) -> Spec {
    Spec {
        id: SPEC,
        project_id: PROJECT,
        path: "docs/specs/t.md".into(),
        slug: "t".into(),
        title: "T".into(),
        state: SpecState::Approved,
        origin: SpecOrigin::Manual,
        priority: 0,
        latest_revision_id: latest,
        approved_revision_id: Some(FROM),
        approved_at: Some(APPROVED_AT),
        shipped_at: None,
        created_at: Timestamp(1),
        updated_at: APPROVED_AT,
    }
}

fn item(n: u32, state: WorkItemState) -> WorkItem {
    WorkItem {
        id: WorkItemId(Uuid::from_u128(100 + u128::from(n))),
        project_id: PROJECT,
        origin: WorkItemOrigin::SpecStep,
        spec_id: Some(SPEC),
        spec_revision_id: Some(FROM),
        step_id: Some(format!("S{n}")),
        step_index: Some(n),
        task_key: format!("t/s{n}"),
        work: format!("work {n}"),
        repo_id: REPO,
        branch: format!("saka/t/s{n}"),
        state,
        stage: None,
        stage_phase: None,
        priority: 0,
        version: 1,
        admitted_at: None,
        done_at: None,
        created_at: APPROVED_AT,
        updated_at: APPROVED_AT,
    }
}

fn task_of(item: WorkItem) -> Task {
    Task::new(item)
}

fn actions(plan: &RevisionPlan) -> Vec<(String, PlanAction)> {
    plan.steps
        .iter()
        .map(|s| (s.step_id.clone(), s.action))
        .collect()
}

fn the(plan: &RevisionPlan, id: &str) -> PlanStep {
    plan.steps
        .iter()
        .find(|s| s.step_id == id)
        .cloned()
        .unwrap()
}

fn approve(
    spec: &Spec,
    from: &RevisionView,
    to: &RevisionView,
    tasks: &[WorkItem],
) -> Result<Vec<Event>, Rejection> {
    let mut ids = Counter(500);
    let mut ctx = Ctx {
        now: NOW,
        ids: &mut ids,
    };
    spec::approve(
        spec,
        to,
        Approval {
            if_match: to.id,
            reason: None,
            default_repo: REPO,
            parser_version: 2,
            depends_on_line: 1,
            project_specs: &[],
            project_graph: &[],
            approved: Some(from),
            tasks,
        },
        &mut ctx,
    )
}

fn apply(tasks: &[WorkItem], events: &[Event]) -> Vec<WorkItem> {
    let mut out: Vec<Task> = tasks.iter().cloned().map(task_of).collect();
    for e in events {
        if let Some(t) = task::evolve(None, e, NOW) {
            out.push(t);
            continue;
        }
        for t in &mut out {
            *t = task::evolve(Some(t.clone()), e, NOW).unwrap();
        }
    }
    out.into_iter().map(|t| t.item).collect()
}

#[test]
fn each_row_of_the_plan_table() {
    let from = revision(FROM, steps(4), 0);
    let mut to_steps = steps(3);
    to_steps[1].work = "changed 2".into();
    to_steps[2].work = "changed 3".into();
    to_steps.push(StepPlan {
        work: "new 4".into(),
        ..step(4)
    });
    to_steps.push(step(5));
    to_steps.push(step(6));
    let to = revision(TO, to_steps, 0);
    let tasks = vec![
        item(1, WorkItemState::Done),
        item(2, WorkItemState::Admitted),
        item(3, WorkItemState::Queued),
        item(4, WorkItemState::AwaitingStart),
        item(5, WorkItemState::Superseded),
    ];
    let plan = plan_revision(&from, &to, &tasks, 2);
    assert_eq!(
        actions(&plan),
        vec![
            ("S1".into(), PlanAction::Keep),
            ("S2".into(), PlanAction::Conflict),
            ("S3".into(), PlanAction::Repin),
            ("S4".into(), PlanAction::Repin),
            ("S5".into(), PlanAction::Restore),
            ("S6".into(), PlanAction::Add),
        ]
    );
    let s2 = the(&plan, "S2");
    assert_eq!(s2.reason, Some(ConflictReason::Changed));
    assert_eq!(s2.fields, vec![StepField::Work]);
    assert_eq!(s2.state, Some(WorkItemState::Admitted));
    assert_eq!(s2.work_item_id, Some(tasks[1].id));
    assert_eq!(the(&plan, "S6").work_item_id, None);
    assert!(!plan.ready);
    assert_eq!(
        plan.conflict_params(),
        json!([{
            "step_id": "S2",
            "reason": "changed",
            "fields": ["work"],
            "work_item_id": tasks[1].id,
            "state": "admitted",
        }])
    );

    let shorter = revision(TO, steps(1), 0);
    let removed = plan_revision(
        &from,
        &shorter,
        &[
            item(1, WorkItemState::Done),
            item(2, WorkItemState::Queued),
            item(3, WorkItemState::Blocked),
            item(4, WorkItemState::Superseded),
        ],
        2,
    );
    assert_eq!(
        actions(&removed),
        vec![
            ("S1".into(), PlanAction::Keep),
            ("S2".into(), PlanAction::Supersede),
            ("S3".into(), PlanAction::Conflict),
        ]
    );
    assert_eq!(the(&removed, "S3").reason, Some(ConflictReason::Removed));
    assert!(the(&removed, "S3").fields.is_empty());
}

type Change = fn(&mut StepPlan);

#[test]
fn every_changed_field_is_named_in_order() {
    let from = revision(FROM, steps(2), 0);
    let cases: Vec<(Change, Vec<StepField>)> = vec![
        (|s| s.work = "x".into(), vec![StepField::Work]),
        (|s| s.ac = vec!["AC-09".into()], vec![StepField::Ac]),
        (
            |s| s.commands = vec!["make x".into()],
            vec![StepField::Commands],
        ),
        (|s| s.after = vec![], vec![StepField::After]),
        (
            |s| {
                s.after = vec![];
                s.work = "x".into();
                s.commands.push("make y".into());
            },
            vec![StepField::Work, StepField::Commands, StepField::After],
        ),
    ];
    for (change, fields) in cases {
        let mut to_steps = steps(2);
        change(&mut to_steps[1]);
        let to = revision(TO, to_steps, 0);
        let plan = plan_revision(
            &from,
            &to,
            &[item(1, WorkItemState::Done), item(2, WorkItemState::Done)],
            2,
        );
        assert_eq!(the(&plan, "S2").fields, fields);
        assert_eq!(the(&plan, "S2").action, PlanAction::Conflict);
    }
}

#[test]
fn readiness_needs_clean_lint_and_the_current_parser() {
    let from = revision(FROM, steps(2), 0);
    let tasks = [
        item(1, WorkItemState::Queued),
        item(2, WorkItemState::Queued),
    ];
    let to = revision(TO, steps(3), 0);
    assert!(plan_revision(&from, &to, &tasks, 2).ready);
    let failing = RevisionView {
        lint_ok: false,
        ..to.clone()
    };
    assert!(!plan_revision(&from, &failing, &tasks, 2).ready);
    let old = RevisionView {
        parser_version: 1,
        ..to.clone()
    };
    assert!(!plan_revision(&from, &old, &tasks, 2).ready);
}

#[test]
fn re_approval_repins_supersedes_and_keeps_the_first_approval_time() {
    let from = revision(FROM, steps(3), 0);
    let mut to_steps = steps(2);
    to_steps[1].work = "changed 2".into();
    let to = revision(TO, to_steps, 7);
    let spec = approved_spec(TO);
    let tasks = vec![
        item(1, WorkItemState::Done),
        item(2, WorkItemState::Queued),
        item(3, WorkItemState::Queued),
    ];
    let events = approve(&spec, &from, &to, &tasks).unwrap();
    assert_eq!(
        events.iter().map(Event::kind).collect::<Vec<_>>(),
        vec![
            "spec.spec.approved",
            "delivery.work_item.repinned",
            "delivery.work_item.superseded"
        ]
    );
    assert!(matches!(
        &events[0],
        Event::SpecApproved { previous_revision_id: Some(p), priority: 7, .. } if *p == FROM
    ));
    assert_eq!(
        events[1],
        Event::WorkItemRepinned {
            work_item_id: tasks[1].id,
            spec_revision_id: TO,
            step_index: 2,
            work: "changed 2".into(),
            priority: 7,
            restored: false,
        }
    );
    assert_eq!(
        events[2],
        Event::WorkItemSuperseded {
            work_item_id: tasks[2].id,
            spec_revision_id: TO,
            reason: SupersedeReason::StepRemoved,
        }
    );
    let after = events
        .iter()
        .try_fold(spec.clone(), |s, e| spec::evolve(Some(s), e, NOW))
        .unwrap();
    assert_eq!(after.approved_revision_id, Some(TO));
    assert_eq!(after.approved_at, Some(APPROVED_AT));
    assert_eq!(after.priority, 7);

    let items = apply(&tasks, &events);
    assert_eq!(items[0].spec_revision_id, Some(FROM));
    assert_eq!(items[1].spec_revision_id, Some(TO));
    assert_eq!(items[1].work, "changed 2");
    assert_eq!(items[1].priority, 7);
    assert_eq!(items[2].state, WorkItemState::Superseded);

    let graph = spec::approval_graph(&spec, &to, &events, &tasks);
    assert_eq!(
        graph.1,
        vec![WorkItemDependency {
            work_item_id: tasks[1].id,
            depends_on_work_item_id: tasks[0].id,
            project_id: PROJECT,
        }]
    );
}

#[test]
fn a_returning_step_is_restored_with_its_key_and_branch() {
    let from = revision(FROM, steps(2), 0);
    let to = revision(TO, steps(4), 0);
    let spec = approved_spec(TO);
    let tasks = vec![
        item(1, WorkItemState::Done),
        item(2, WorkItemState::Queued),
        item(3, WorkItemState::Superseded),
    ];
    let events = approve(&spec, &from, &to, &tasks).unwrap();
    assert_eq!(
        events.iter().map(Event::kind).collect::<Vec<_>>(),
        vec![
            "spec.spec.approved",
            "delivery.work_item.repinned",
            "delivery.work_item.repinned",
            "delivery.work_item.created"
        ]
    );
    assert!(matches!(
        &events[2],
        Event::WorkItemRepinned { work_item_id, restored: true, .. } if *work_item_id == tasks[2].id
    ));
    let items = apply(&tasks, &events);
    let restored = items.iter().find(|t| t.id == tasks[2].id).unwrap();
    assert_eq!(restored.state, WorkItemState::Queued);
    assert_eq!(restored.task_key, "t/s3");
    assert_eq!(restored.branch, "saka/t/s3");
    assert_eq!(restored.spec_revision_id, Some(TO));
    let added = items.last().unwrap();
    assert_eq!(added.step_id.as_deref(), Some("S4"));
    assert_eq!(added.task_key, "t/s4");

    let graph = spec::approval_graph(&spec, &to, &events, &tasks).1;
    let pairs: Vec<(WorkItemId, WorkItemId)> = graph
        .iter()
        .map(|d| (d.work_item_id, d.depends_on_work_item_id))
        .collect();
    assert_eq!(
        pairs,
        vec![
            (tasks[1].id, tasks[0].id),
            (tasks[2].id, tasks[1].id),
            (added.id, tasks[2].id)
        ]
    );
}

#[test]
fn re_approval_refuses_conflicts_and_keeps_the_first_approval_checks() {
    let from = revision(FROM, steps(2), 0);
    let mut to_steps = steps(2);
    to_steps[0].commands = vec!["make other".into()];
    let to = revision(TO, to_steps, 0);
    let spec = approved_spec(TO);
    let tasks = vec![item(1, WorkItemState::Done), item(2, WorkItemState::Queued)];
    match approve(&spec, &from, &to, &tasks) {
        Err(Rejection::RevisionConflict { steps }) => assert_eq!(
            steps,
            json!([{
                "step_id": "S1",
                "reason": "changed",
                "fields": ["commands"],
                "work_item_id": tasks[0].id,
                "state": "done",
            }])
        ),
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(
        Rejection::RevisionConflict { steps: json!([]) }.code(),
        "revision_conflict"
    );

    let same = approved_spec(FROM);
    assert_eq!(
        approve(&same, &from, &from, &tasks),
        Err(Rejection::SpecAlreadyApproved)
    );
    let mut shipped = approved_spec(TO);
    shipped.state = SpecState::Shipped;
    assert_eq!(
        approve(&shipped, &from, &revision(TO, steps(3), 0), &tasks),
        Err(Rejection::InvalidState {
            state: "shipped".into()
        })
    );
    let failing = RevisionView {
        lint_ok: false,
        ..revision(TO, steps(3), 0)
    };
    assert!(matches!(
        approve(&spec, &from, &failing, &tasks),
        Err(Rejection::SpecNotReady { .. })
    ));
}

#[test]
fn superseded_tasks_never_run_and_never_hold_the_spec() {
    let superseded = task_of(item(2, WorkItemState::Superseded));
    let mut ids = Counter(700);
    let mut ctx = Ctx {
        now: NOW,
        ids: &mut ids,
    };
    for input in [
        TaskInput::Admit,
        TaskInput::Cancel {
            reason: "stop".into(),
        },
    ] {
        assert_eq!(
            task::decide(&superseded, input, &cfg(), &mut ctx),
            Err(Rejection::InvalidState {
                state: "superseded".into()
            })
        );
    }
    assert!(task::available_actions(&superseded).is_empty());
    let deps = [WorkItemDependency {
        work_item_id: superseded.item.id,
        depends_on_work_item_id: WorkItemId(Uuid::from_u128(101)),
        project_id: PROJECT,
    }];
    assert!(waiting_on(&superseded, &[], &[], &deps, &[]).is_empty());

    let spec = approved_spec(FROM);
    let snapshot = Snapshot {
        quotas: Vec::new(),
        specs: vec![spec.clone()],
        tasks: vec![task_of(item(1, WorkItemState::Queued)), superseded.clone()],
        work_item_dependencies: vec![],
        spec_dependencies: vec![],
    };
    let admitted: Vec<WorkItemId> = reconcile(&snapshot, &cfg(), 4, NOW)
        .into_iter()
        .filter_map(|a| match a {
            Action::Decide(id, TaskInput::Admit) => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(admitted, vec![snapshot.tasks[0].item.id]);

    let done = Snapshot {
        tasks: vec![task_of(item(1, WorkItemState::Done)), superseded],
        ..snapshot
    };
    assert!(
        reconcile(&done, &cfg(), 4, NOW).contains(&Action::ShipSpec(SPEC)),
        "superseded tasks do not hold the spec back"
    );
    assert!(spec::ships([
        WorkItemState::Done,
        WorkItemState::Superseded
    ]));
    assert!(!spec::ships([WorkItemState::Superseded]));
    assert!(!spec::ships([
        WorkItemState::Done,
        WorkItemState::Cancelled
    ]));
    assert!(!spec::ships([]));
}

#[test]
fn withdraw_supersedes_unstarted_tasks_only_without_active_ones() {
    let spec = approved_spec(FROM);
    let tasks = vec![
        item(1, WorkItemState::Done),
        item(2, WorkItemState::Cancelled),
        item(3, WorkItemState::Queued),
        item(4, WorkItemState::AwaitingStart),
    ];
    let events = spec::withdraw(&spec, &tasks, "not needed".into()).unwrap();
    assert_eq!(
        events,
        vec![
            Event::SpecWithdrawn {
                spec_id: SPEC,
                reason: "not needed".into()
            },
            Event::WorkItemSuperseded {
                work_item_id: tasks[2].id,
                spec_revision_id: FROM,
                reason: SupersedeReason::SpecWithdrawn
            },
            Event::WorkItemSuperseded {
                work_item_id: tasks[3].id,
                spec_revision_id: FROM,
                reason: SupersedeReason::SpecWithdrawn
            },
        ]
    );
    let after = events
        .iter()
        .try_fold(spec.clone(), |s, e| spec::evolve(Some(s), e, NOW))
        .unwrap();
    assert_eq!(after.state, SpecState::Withdrawn);
    assert_eq!(after.approved_revision_id, Some(FROM));

    for state in [
        WorkItemState::Admitted,
        WorkItemState::InStage,
        WorkItemState::Blocked,
        WorkItemState::TakenOver,
    ] {
        let busy = [item(1, WorkItemState::Done), item(2, state)];
        assert_eq!(
            spec::withdraw(&spec, &busy, "x".into()),
            Err(Rejection::SpecHasActiveTasks {
                tasks: json!([{ "work_item_id": busy[1].id, "step_id": "S2", "state": state }])
            })
        );
    }
    for state in [SpecState::Shipped, SpecState::Withdrawn] {
        let closed = Spec {
            state,
            ..spec.clone()
        };
        assert_eq!(
            spec::withdraw(&closed, &[], "x".into()),
            Err(Rejection::InvalidState {
                state: state.as_str().into()
            })
        );
    }
    let draft = Spec {
        state: SpecState::Draft,
        approved_revision_id: None,
        ..spec
    };
    assert_eq!(spec::withdraw(&draft, &[], "x".into()).unwrap().len(), 1);
}

#[test]
fn old_approved_events_read_without_a_previous_revision() {
    let old = json!({
        "kind": "spec.spec.approved",
        "payload": { "spec_id": SPEC, "revision_id": FROM, "reason": null }
    });
    let event: Event = serde_json::from_value(old).unwrap();
    assert!(matches!(
        event,
        Event::SpecApproved {
            previous_revision_id: None,
            ..
        }
    ));
}

const STATES: [WorkItemState; 7] = [
    WorkItemState::Queued,
    WorkItemState::AwaitingStart,
    WorkItemState::Admitted,
    WorkItemState::InStage,
    WorkItemState::Blocked,
    WorkItemState::Done,
    WorkItemState::Cancelled,
];

fn changed(n: u32, mask: u8) -> StepPlan {
    let mut s = step(n);
    if mask & 1 != 0 {
        s.work = format!("changed {n}");
    }
    if mask & 2 != 0 {
        s.ac.push("AC-99".into());
    }
    if mask & 4 != 0 {
        s.commands = vec![format!("make other {n}")];
    }
    if mask & 8 != 0 && n > 1 {
        s.after = vec!["S1".into()];
        if n == 2 {
            s.after.clear();
        }
    }
    s
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn plans_never_touch_started_work_and_keep_one_task_per_step(
        from_len in 1u32..6,
        states in prop::collection::vec(0usize..7, 6),
        gone in 0u32..3,
        to_len in 1u32..8,
        masks in prop::collection::vec(0u8..16, 8),
    ) {
        let from = revision(FROM, steps(from_len), 0);
        let mut tasks: Vec<WorkItem> = (1..=from_len)
            .map(|n| item(n, STATES[states[n as usize - 1]]))
            .collect();
        tasks.extend((from_len + 1..=from_len + gone).map(|n| item(n, WorkItemState::Superseded)));
        let to_steps: Vec<StepPlan> = (1..=to_len).map(|n| changed(n, masks[n as usize - 1])).collect();
        let to = revision(TO, to_steps, 3);
        let plan = plan_revision(&from, &to, &tasks, 2);

        for planned in &plan.steps {
            let started = planned.state.is_some_and(WorkItemState::has_started);
            if started {
                prop_assert!(matches!(planned.action, PlanAction::Keep | PlanAction::Conflict));
            }
        }
        for s in &to.steps {
            prop_assert_eq!(plan.steps.iter().filter(|p| p.step_id == s.id).count(), 1);
        }
        prop_assert_eq!(plan.ready, plan.conflicts().is_empty());

        let spec = approved_spec(TO);
        match approve(&spec, &from, &to, &tasks) {
            Ok(events) => {
                prop_assert!(plan.ready);
                let items = apply(&tasks, &events);
                for s in &to.steps {
                    let live: Vec<&WorkItem> = items
                        .iter()
                        .filter(|t| t.step_id.as_deref() == Some(s.id.as_str()) && t.state != WorkItemState::Superseded)
                        .collect();
                    prop_assert_eq!(live.len(), 1);
                }
                for t in items.iter().filter(|t| t.state == WorkItemState::Superseded) {
                    prop_assert!(!to.steps.iter().any(|s| Some(s.id.as_str()) == t.step_id.as_deref()));
                }
                for before in tasks.iter().filter(|t| t.state.has_started()) {
                    let now = items.iter().find(|t| t.id == before.id).unwrap();
                    prop_assert_eq!(now, before);
                }
            }
            Err(Rejection::RevisionConflict { .. }) => prop_assert!(!plan.ready),
            Err(other) => prop_assert!(false, "unexpected rejection {:?}", other),
        }
    }
}

const ALL_STATES: [WorkItemState; 9] = [
    WorkItemState::Queued,
    WorkItemState::AwaitingStart,
    WorkItemState::Admitted,
    WorkItemState::InStage,
    WorkItemState::Blocked,
    WorkItemState::TakenOver,
    WorkItemState::Done,
    WorkItemState::Cancelled,
    WorkItemState::Superseded,
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn superseded_tasks_are_never_admitted_and_never_count_as_cancelled(
        states in prop::collection::vec(0usize..9, 1..8),
        slots in 0usize..6,
    ) {
        let tasks: Vec<Task> = states
            .iter()
            .enumerate()
            .map(|(i, s)| task_of(item(u32::try_from(i).unwrap() + 1, ALL_STATES[*s])))
            .collect();
        let snapshot = Snapshot {
        quotas: Vec::new(),
            specs: vec![approved_spec(FROM)],
            tasks: tasks.clone(),
            work_item_dependencies: vec![],
            spec_dependencies: vec![],
        };
        let admitted: Vec<WorkItemId> = reconcile(&snapshot, &cfg(), slots, NOW)
            .into_iter()
            .filter_map(|a| match a {
                Action::Decide(id, TaskInput::Admit) => Some(id),
                _ => None,
            })
            .collect();
        let cancelled = tasks.iter().any(|t| t.item.state == WorkItemState::Cancelled);
        let active = tasks
            .iter()
            .filter(|t| matches!(t.item.state, WorkItemState::Admitted | WorkItemState::InStage))
            .count();
        let queued: Vec<WorkItemId> = tasks
            .iter()
            .filter(|t| t.item.state == WorkItemState::Queued)
            .map(|t| t.item.id)
            .collect();
        for id in &admitted {
            prop_assert!(queued.contains(id));
        }
        let expected = if cancelled { 0 } else { queued.len().min(slots.saturating_sub(active)) };
        prop_assert_eq!(admitted.len(), expected);
    }
}
