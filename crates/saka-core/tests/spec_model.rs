#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use saka_core::Ctx;
use saka_core::admission::{Waiting, waiting_on};
use saka_core::event::Event;
use saka_core::ids::{IdSource, ProjectId, RepoId, SpecId, SpecRevisionId};
use saka_core::model::{DependsOn, ProjectRepo, RevisionView, Spec, SpecDependency, StepPlan};
use saka_core::rejection::Rejection;
use saka_core::spec::{self, Approval, NewRevision};
use saka_core::task;
use saka_core::time::Timestamp;
use saka_core::vocab::{SpecState, WorkItemState};
use serde_json::{Value, json};
use uuid::Uuid;

struct Counter(u128);

impl IdSource for Counter {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

const PROJECT: ProjectId = ProjectId(Uuid::from_u128(900));
const NOW: Timestamp = Timestamp(1_000);

fn spec(n: u128, slug: &str, state: SpecState) -> Spec {
    Spec {
        id: SpecId(Uuid::from_u128(n)),
        project_id: PROJECT,
        path: format!("docs/specs/{slug}.md"),
        slug: slug.into(),
        title: slug.into(),
        state,
        origin: saka_core::vocab::SpecOrigin::Manual,
        priority: 0,
        latest_revision_id: SpecRevisionId(Uuid::from_u128(n + 1000)),
        approved_revision_id: None,
        approved_at: None,
        shipped_at: None,
        created_at: NOW,
        updated_at: NOW,
    }
}

fn bindings(repo: RepoId) -> &'static [ProjectRepo] {
    Box::leak(Box::new([ProjectRepo {
        repo_id: repo,
        name: "app".into(),
        is_default: true,
        base_branch: "main".into(),
        path_scope: vec!["**".into()],
        autonomy: None,
    }]))
}

fn step(n: u32, after: &[&str]) -> StepPlan {
    StepPlan {
        id: format!("S{n}"),
        index: n,
        work: format!("work {n}"),
        ac: vec!["AC-01".into()],
        commands: vec![format!("make s{n}")],
        after: after.iter().map(|a| a.to_string()).collect(),
        repo: None,
        line: 10 + n,
    }
}

fn revision(s: &Spec, depends_on: &[&str], priority: i64, steps: Vec<StepPlan>) -> RevisionView {
    RevisionView {
        id: s.latest_revision_id,
        lint_ok: true,
        issues: json!([]),
        steps,
        parser_version: 2,
        priority,
        depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
    }
}

fn edge(from: &Spec, to: &Spec) -> SpecDependency {
    SpecDependency {
        spec_id: from.id,
        depends_on_spec_id: to.id,
        depends_on_slug: to.slug.clone(),
        revision_id: from.latest_revision_id,
        project_id: PROJECT,
    }
}

fn approve(
    s: &Spec,
    latest: &RevisionView,
    specs: &[Spec],
    graph: &[SpecDependency],
) -> Result<Vec<Event>, Rejection> {
    let mut ids = Counter(1);
    let mut ctx = Ctx {
        now: NOW,
        ids: &mut ids,
    };
    spec::approve(
        s,
        latest,
        Approval {
            if_match: s.latest_revision_id,
            reason: None,
            repos: bindings(RepoId(Uuid::from_u128(901))),
            parser_version: 2,
            depends_on_line: 3,
            project_specs: specs,
            project_graph: graph,
            approved: None,
            tasks: &[],
        },
        &mut ctx,
    )
}

fn issues(result: Result<Vec<Event>, Rejection>) -> Value {
    match result {
        Err(Rejection::SpecNotReady { issues }) => issues,
        other => panic!("expected spec_not_ready, got {other:?}"),
    }
}

#[test]
fn unknown_and_withdrawn_dependencies_are_refused_on_the_frontmatter_line() {
    let a = spec(1, "a", SpecState::Ready);
    let gone = spec(2, "gone", SpecState::Withdrawn);
    let specs = [a.clone(), gone];
    let latest = revision(&a, &["missing", "gone"], 0, vec![step(1, &[])]);
    assert_eq!(
        issues(approve(&a, &latest, &specs, &[])),
        json!([
            {"code": "depends_on_unknown", "severity": "error", "line": 3, "params": {"slug": "missing"}},
            {"code": "depends_on_withdrawn", "severity": "error", "line": 3, "params": {"slug": "gone"}},
        ])
    );
}

#[test]
fn a_dependency_that_waits_back_is_a_cycle() {
    let a = spec(1, "a", SpecState::Ready);
    let b = spec(2, "b", SpecState::Approved);
    let c = spec(3, "c", SpecState::Approved);
    let specs = [a.clone(), b.clone(), c.clone()];

    let two = [edge(&b, &a)];
    let latest = revision(&a, &["b"], 0, vec![step(1, &[])]);
    assert_eq!(
        issues(approve(&a, &latest, &specs, &two)),
        json!([{"code": "depends_on_cycle", "severity": "error", "line": 3, "params": {"cycle": ["a", "b", "a"]}}])
    );

    let three = [edge(&b, &c), edge(&c, &a)];
    assert_eq!(
        issues(approve(&a, &latest, &specs, &three)),
        json!([{"code": "depends_on_cycle", "severity": "error", "line": 3, "params": {"cycle": ["a", "b", "c", "a"]}}])
    );
}

#[test]
fn approve_resolves_dependencies_and_writes_the_graphs() {
    let a = spec(1, "a", SpecState::Draft);
    let b = spec(2, "b", SpecState::Ready);
    let specs = [a.clone(), b.clone()];
    let latest = revision(
        &b,
        &["a"],
        10,
        vec![
            step(1, &[]),
            step(2, &["S1"]),
            step(3, &["S1"]),
            step(4, &["S2", "S3"]),
        ],
    );
    let events = approve(&b, &latest, &specs, &[]).unwrap();
    let Event::SpecApproved {
        priority,
        depends_on,
        ..
    } = &events[0]
    else {
        panic!("first event is the approval");
    };
    assert_eq!(*priority, 10);
    assert_eq!(
        depends_on,
        &vec![DependsOn {
            slug: "a".into(),
            spec_id: a.id
        }]
    );
    let approved = events
        .iter()
        .try_fold(b.clone(), |s, e| spec::evolve(Some(s), e, NOW))
        .unwrap();
    assert_eq!(approved.priority, 10);
    let tasks: Vec<_> = events
        .iter()
        .filter_map(|e| task::evolve(None, e, NOW))
        .collect();
    assert!(tasks.iter().all(|t| t.item.priority == 10));

    let (spec_rows, item_rows) = spec::approval_graph(&b, &latest, &events, &[]);
    assert_eq!(spec_rows, vec![edge(&b, &a)]);
    let ids: Vec<_> = (1..=4)
        .map(|n| {
            tasks
                .iter()
                .find(|t| t.item.step_index == Some(n))
                .unwrap()
                .item
                .id
        })
        .collect();
    let id = |n: usize| ids[n - 1];
    let pairs: Vec<_> = item_rows
        .iter()
        .map(|d| (d.work_item_id, d.depends_on_work_item_id))
        .collect();
    assert_eq!(
        pairs,
        vec![
            (id(2), id(1)),
            (id(3), id(1)),
            (id(4), id(2)),
            (id(4), id(3))
        ]
    );

    let mut tasks = tasks;
    tasks[0].item.state = WorkItemState::Done;
    let s4 = tasks.iter().find(|t| t.item.step_index == Some(4)).unwrap();
    assert_eq!(
        waiting_on(s4, &tasks, &specs, &item_rows, &spec_rows),
        vec![
            Waiting::Step {
                step_id: "S2".into(),
                work_item_id: id(2)
            },
            Waiting::Step {
                step_id: "S3".into(),
                work_item_id: id(3)
            },
            Waiting::Spec {
                slug: "a".into(),
                spec_id: a.id
            },
        ]
    );
    let s2 = tasks.iter().find(|t| t.item.step_index == Some(2)).unwrap();
    assert_eq!(
        waiting_on(s2, &tasks, &specs, &item_rows, &spec_rows),
        vec![Waiting::Spec {
            slug: "a".into(),
            spec_id: a.id
        }]
    );
    let shipped = [spec(1, "a", SpecState::Shipped), b.clone()];
    assert!(waiting_on(s2, &tasks, &shipped, &item_rows, &spec_rows).is_empty());
    let done = &tasks[0];
    assert!(waiting_on(done, &tasks, &specs, &item_rows, &spec_rows).is_empty());
}

#[test]
fn priority_follows_the_latest_revision_until_approve() {
    let mut ids = Counter(10);
    let mut ctx = Ctx {
        now: NOW,
        ids: &mut ids,
    };
    let revision = |priority| NewRevision {
        origin: saka_core::vocab::SpecOrigin::Manual,
        source_commit: "a".repeat(40),
        content_hash: format!("h{priority}"),
        context_hash: "sha256:none".into(),
        context: Vec::new(),
        lint_ok: true,
        issue_codes: vec![],
        title: "T".into(),
        parser_version: 2,
        priority,
    };
    let events = spec::record_revision(
        None,
        PROJECT,
        "docs/specs/t.md",
        "t",
        revision(5),
        None,
        &mut ctx,
    );
    let s = events
        .iter()
        .fold(None, |s, e| spec::evolve(s, e, NOW))
        .unwrap();
    assert_eq!(s.priority, 5);
    let events = spec::record_revision(
        Some(&s),
        PROJECT,
        &s.path,
        &s.slug,
        revision(-3),
        None,
        &mut ctx,
    );
    let s = spec::evolve(Some(s), &events[0], NOW).unwrap();
    assert_eq!(s.priority, -3);
    let mut approved = s.clone();
    approved.state = SpecState::Approved;
    let events = spec::record_revision(
        Some(&approved),
        PROJECT,
        &s.path,
        &s.slug,
        revision(7),
        None,
        &mut ctx,
    );
    let after = spec::evolve(Some(approved), &events[0], NOW).unwrap();
    assert_eq!(after.priority, -3);
}

#[test]
fn old_events_read_with_defaults() {
    let recorded: Event = serde_json::from_value(json!({
        "kind": "spec.revision.recorded",
        "payload": {
            "spec_id": Uuid::from_u128(1), "revision_id": Uuid::from_u128(2), "source_commit": "a",
            "content_hash": "h", "lint_ok": true, "issue_codes": [], "title": "T", "reused": false
        }
    }))
    .unwrap();
    assert!(matches!(
        recorded,
        Event::SpecRevisionRecorded {
            parser_version: 1,
            priority: 0,
            ref context_hash,
            ref context,
            ..
        } if context_hash == saka_core::event::EMPTY_CONTEXT_HASH && context.is_empty()
    ));
    let approved: Event = serde_json::from_value(json!({
        "kind": "spec.spec.approved",
        "payload": {"spec_id": Uuid::from_u128(1), "revision_id": Uuid::from_u128(2), "reason": null}
    }))
    .unwrap();
    assert!(matches!(
        approved,
        Event::SpecApproved { priority: 0, ref depends_on, .. } if depends_on.is_empty()
    ));
}
