#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use std::collections::BTreeSet;

use proptest::prelude::*;
use saka_core::autonomy::Ceilings;
use saka_core::hook::{self, RawHook};
use saka_core::inbox::{Source, desired};
use saka_core::vocab::{
    Autonomy, InboxActionKind, InboxKind, InboxState, InboxSubject, SpecState, Stage, StagePhase,
    WorkItemState,
};
use saka_engine::{InboxFilter, InboxItem, Page};
use saka_testkit::fixtures::{TWO_STEP_PATH, TWO_STEP_SPEC};
use saka_testkit::{AgentOutcome, AgentScript, Operator, Sim, SimConfig};
use serde_json::json;

const STEPS: usize = 8_000;

fn asking() -> AgentScript {
    AgentScript {
        outcome: AgentOutcome::Commit,
        ask: Some(("Bash".into(), json!({ "command": "make deploy" }))),
        ..AgentScript::default()
    }
}

fn operator(start: bool, merge: bool) -> Operator {
    Operator {
        start,
        merge,
        retry: vec![],
        answer: None,
        decide: None,
    }
}

fn level(autonomy: Autonomy) -> Ceilings {
    Ceilings {
        default: autonomy,
        repos: Vec::new(),
    }
}

fn all(sim: &Sim, closed: bool) -> Vec<InboxItem> {
    sim.engine()
        .inbox(
            &InboxFilter {
                closed,
                ..InboxFilter::default()
            },
            &Page {
                after: None,
                limit: 100,
            },
        )
        .unwrap()
        .items
}

fn open(sim: &Sim, subject: InboxSubject) -> Vec<InboxItem> {
    all(sim, false)
        .into_iter()
        .filter(|i| i.subject == subject)
        .collect()
}

fn history(sim: &Sim, subject: InboxSubject) -> Vec<InboxItem> {
    sim.engine()
        .store()
        .read(|c| {
            let mut stmt =
                c.prepare("SELECT id FROM inbox_item WHERE subject = ?1 ORDER BY rowid")?;
            let ids: Vec<String> = stmt
                .query_map([subject.as_str()], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            Ok(ids)
        })
        .unwrap()
        .into_iter()
        .map(|id| {
            sim.engine()
                .inbox_item(saka_core::ids::InboxItemId(id.parse().unwrap()))
                .unwrap()
                .unwrap()
        })
        .collect()
}

fn operator_cause(sim: &Sim) -> saka_engine::Cause {
    sim.engine().cause("human:operator:local")
}

#[test]
fn a_pending_permission_opens_one_item_and_answering_it_closes_it_in_the_same_commit() {
    let mut sim = Sim::new(SimConfig {
        agent: asking(),
        operator: operator(true, true),
        ..SimConfig::default()
    });
    sim.approve();
    assert!(sim.run_until(STEPS, |s| !s.task(1).permissions.is_empty()));
    let request = sim.task(1).permissions[0].clone();

    let items = open(&sim, InboxSubject::Permission);
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.kind, InboxKind::Permission);
    assert_eq!(item.source_id, request.id.to_string());
    assert_eq!(item.action.kind, InboxActionKind::AnswerPermission);
    assert_eq!(
        item.action.params,
        json!({ "permission_request_id": request.id })
    );
    assert_eq!(item.task_key.as_deref(), Some("tasks-done/s1"));
    assert!(!item.dismissible());
    let before = sim.engine().last_seq().unwrap();

    sim.engine()
        .answer_permission(request.id, true, None, &operator_cause(&sim))
        .unwrap();
    assert!(sim.engine().last_seq().unwrap() > before);
    assert!(open(&sim, InboxSubject::Permission).is_empty());
    let closed = history(&sim, InboxSubject::Permission);
    assert_eq!(closed[0].state, InboxState::Resolved);
    assert!(closed[0].closed_at.is_some());
}

#[test]
fn a_permission_left_unanswered_closes_when_it_expires_and_the_blocker_takes_over() {
    let mut sim = Sim::new(SimConfig {
        agent: AgentScript {
            outcome: AgentOutcome::Hang,
            ..asking()
        },
        operator: operator(true, true),
        ..SimConfig::default()
    });
    sim.cfg.loop_cfg.permission_wait_ms = 3_600_000;
    sim.restart();
    sim.approve();
    assert!(sim.run_until(STEPS, |s| !s.task(1).permissions.is_empty()));
    assert_eq!(open(&sim, InboxSubject::Permission).len(), 1);

    assert!(sim.run_until(STEPS, |s| {
        s.task(1).item.state == WorkItemState::Blocked
    }));
    assert!(open(&sim, InboxSubject::Permission).is_empty());
    let blockers = open(&sim, InboxSubject::Blocker);
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers[0].kind, InboxKind::Blocker);
    assert_eq!(blockers[0].params["code"], "permission_expired");
}

#[test]
fn a_ready_spec_opens_an_approval_that_approving_closes_and_a_new_revision_replaces() {
    let sim = Sim::new(SimConfig::default());
    assert_eq!(sim.spec().state, SpecState::Ready);
    let first = open(&sim, InboxSubject::Spec);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].kind, InboxKind::Approval);
    assert_eq!(first[0].action.kind, InboxActionKind::ApproveSpec);
    assert_eq!(
        first[0].source_id,
        format!("{}:{}", sim.spec_id, sim.revision_id)
    );
    assert!(first[0].dismissible());

    let text = format!("{TWO_STEP_SPEC}\n\nA note added in review.\n");
    let revised = sim.register(TWO_STEP_PATH, &text);
    assert!(revised.lint_ok, "{:?}", revised.issues);
    assert_ne!(revised.revision_id, sim.revision_id);
    let second = open(&sim, InboxSubject::Spec);
    assert_eq!(second.len(), 1);
    assert_ne!(second[0].id, first[0].id);
    assert_eq!(
        second[0].action.params,
        json!({ "spec_id": sim.spec_id, "revision": revised.revision_id })
    );
    let history = history(&sim, InboxSubject::Spec);
    assert_eq!(
        history.iter().map(|i| i.state).collect::<Vec<_>>(),
        vec![InboxState::Resolved, InboxState::Open]
    );

    sim.approve_spec(sim.spec_id, revised.revision_id).unwrap();
    assert!(open(&sim, InboxSubject::Spec).is_empty());
}

#[test]
fn a_strict_repo_opens_a_start_approval_that_starting_closes() {
    let mut cfg = SimConfig {
        operator: operator(false, true),
        ..SimConfig::default()
    };
    cfg.loop_cfg.autonomy = level(Autonomy::Strict);
    let mut sim = Sim::new(cfg);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).item.state == WorkItemState::AwaitingStart
    }));
    let task = sim.task(1);
    let items = open(&sim, InboxSubject::StepStart);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, InboxKind::Approval);
    assert_eq!(items[0].action.kind, InboxActionKind::StartStep);
    assert_eq!(
        items[0].action.params,
        json!({ "work_item_id": task.item.id, "version": task.item.version })
    );

    sim.engine()
        .start_step_with(task.item.id, task.item.version, &operator_cause(&sim), None)
        .unwrap();
    assert!(open(&sim, InboxSubject::StepStart).is_empty());
    assert_eq!(
        history(&sim, InboxSubject::StepStart)[0].state,
        InboxState::Resolved
    );
}

#[test]
fn a_standard_repo_opens_a_merge_approval_that_requesting_the_merge_closes() {
    let mut sim = Sim::new(SimConfig {
        operator: operator(true, false),
        ..SimConfig::default()
    });
    sim.approve();
    assert!(sim.run_until(STEPS, |s| {
        s.task(1).is_at(Stage::Merge, StagePhase::AwaitingMerge)
    }));
    let task = sim.task(1);
    let items = open(&sim, InboxSubject::Merge);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].action.kind, InboxActionKind::Merge);
    assert_eq!(
        items[0].action.params,
        json!({ "work_item_id": task.item.id, "version": task.item.version })
    );

    sim.engine()
        .request_merge(task.item.id, task.item.version, None, &operator_cause(&sim))
        .unwrap();
    assert!(open(&sim, InboxSubject::Merge).is_empty());
}

#[test]
fn an_autonomous_repo_that_merges_by_itself_never_asks_for_a_merge() {
    let mut cfg = SimConfig {
        operator: operator(true, false),
        ..SimConfig::default()
    };
    cfg.loop_cfg.autonomy = level(Autonomy::Autonomous);
    let mut sim = Sim::new(cfg);
    sim.approve();
    let mut asked = false;
    assert!(sim.run_until(STEPS, |s| {
        asked |= !open(s, InboxSubject::Merge).is_empty();
        s.spec().state == SpecState::Shipped
    }));
    assert!(!asked);
    assert!(history(&sim, InboxSubject::Merge).is_empty());
}

#[test]
fn taking_a_task_over_closes_its_items() {
    let mut sim = Sim::new(SimConfig {
        agent: asking(),
        operator: operator(true, true),
        ..SimConfig::default()
    });
    sim.approve();
    assert!(sim.run_until(STEPS, |s| !s.task(1).permissions.is_empty()));
    assert_eq!(open(&sim, InboxSubject::Permission).len(), 1);

    sim.engine()
        .takeover_with(sim.task(1).item.id, "mine", &operator_cause(&sim), None)
        .unwrap();
    assert_eq!(sim.task(1).item.state, WorkItemState::TakenOver);
    assert!(all(&sim, false).iter().all(|i| i.work_item_id.is_none()));
}

#[test]
fn a_dismissed_approval_stays_closed_for_its_revision_and_returns_for_the_next() {
    let sim = Sim::new(SimConfig::default());
    let item = open(&sim, InboxSubject::Spec).remove(0);
    let done = sim
        .engine()
        .dismiss_inbox_item_with(item.id, "later", &operator_cause(&sim), None)
        .unwrap();
    assert_eq!(done.state, InboxState::Dismissed);
    assert!(open(&sim, InboxSubject::Spec).is_empty());
    assert_eq!(sim.events_of("delivery.inbox_item.dismissed").len(), 1);

    sim.register(TWO_STEP_PATH, TWO_STEP_SPEC);
    assert!(
        open(&sim, InboxSubject::Spec).is_empty(),
        "registering the same revision again does not reopen it"
    );

    let text = format!("{TWO_STEP_SPEC}\n\nRevised.\n");
    sim.register(TWO_STEP_PATH, &text);
    assert_eq!(open(&sim, InboxSubject::Spec).len(), 1);
}

fn notify_on_open() -> Vec<saka_core::hook::Hook> {
    hook::parse_all(&[RawHook {
        name: "needs-you".into(),
        project: "tasks".into(),
        on: "inbox_item_opened".into(),
        action: "notify".into(),
        channel: Some("ops".into()),
        ..RawHook::default()
    }])
    .unwrap()
}

#[test]
fn an_item_that_opens_tells_the_notifier_once_with_what_waits_and_nothing_secret() {
    let mut sim = Sim::new(SimConfig {
        agent: asking(),
        operator: operator(true, true),
        hooks: notify_on_open(),
        ..SimConfig::default()
    });
    assert!(sim.run_until(STEPS, |s| s.hooks.notifications().len() == 1));
    let first = sim.hooks.notifications().remove(0);
    assert_eq!(first.code, "hook.event");
    assert_eq!(first.params["event"], "inbox_item_opened");
    assert_eq!(first.params["project"]["name"], "tasks");
    assert_eq!(first.params["inbox"]["kind"], "approval");
    assert_eq!(first.params["inbox"]["subject"], "spec");

    sim.approve();
    assert!(sim.run_until(STEPS, |s| !s.task(1).permissions.is_empty()));
    assert!(sim.run_until(STEPS, |s| s.hooks.notifications().len() == 2));
    let permission = sim.hooks.notifications().remove(1);
    assert_eq!(permission.params["inbox"]["subject"], "permission");
    assert_eq!(permission.params["task"]["key"], "tasks-done/s1");
    let id = open(&sim, InboxSubject::Permission)[0].id.to_string();
    assert_eq!(permission.params["inbox"]["id"], id);

    for _ in 0..200 {
        sim.step();
    }
    assert_eq!(
        sim.hooks.notifications().len(),
        2,
        "later commits of the same task do not tell again"
    );
}

#[test]
fn without_a_hook_for_the_event_nothing_is_sent() {
    let mut sim = Sim::new(SimConfig::default());
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    assert!(sim.hooks.notifications().is_empty());
}

fn wanted(sim: &Sim, ceilings: &Ceilings) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for task in sim.all_tasks() {
        for d in desired(Source::Task(&task), ceilings) {
            out.insert((d.subject.as_str().to_string(), d.source_id));
        }
    }
    for d in desired(Source::Spec(&sim.spec()), ceilings) {
        out.insert((d.subject.as_str().to_string(), d.source_id));
    }
    out
}

fn waiting(sim: &Sim) -> BTreeSet<(String, String)> {
    all(sim, false)
        .into_iter()
        .map(|i| (i.subject.as_str().to_string(), i.source_id))
        .collect()
}

fn config() -> SimConfig {
    let mut cfg = SimConfig {
        agent: asking(),
        operator: Operator {
            start: true,
            merge: true,
            retry: vec![],
            answer: Some(true),
            decide: None,
        },
        ..SimConfig::default()
    };
    cfg.loop_cfg.autonomy = level(Autonomy::Strict);
    cfg
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn at_every_crash_point_the_open_items_are_exactly_what_is_waiting(
        raw in prop::collection::vec((0usize..400, any::<bool>()), 1..4),
    ) {
        let mut points: Vec<(usize, bool)> = raw;
        points.sort();
        let ceilings = level(Autonomy::Strict);
        let mut sim = Sim::new(config());
        sim.approve();
        let mut steps = 0;
        let mut next = points.into_iter().peekable();
        while sim.spec().state != SpecState::Shipped {
            while next.peek().is_some_and(|(at, _)| *at == steps) {
                let (_, mid_job) = next.next().unwrap();
                if !(mid_job && sim.crash_mid_job()) {
                    sim.restart();
                }
                prop_assert_eq!(waiting(&sim), wanted(&sim, &ceilings));
            }
            sim.operate();
            sim.step();
            prop_assert_eq!(waiting(&sim), wanted(&sim, &ceilings));
            steps += 1;
            prop_assert!(steps < STEPS, "stuck");
        }
        prop_assert!(waiting(&sim).is_empty());
    }
}
