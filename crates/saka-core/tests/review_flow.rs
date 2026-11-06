#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use std::collections::BTreeMap;

use saka_core::Ctx;
use saka_core::authorship::SessionAuthor;
use saka_core::event::Event;
use saka_core::external::{self, Incoming};
use saka_core::ids::{EnvironmentId, IdSource, ProjectId, RepoId, SessionId, WorkItemId};
use saka_core::model::{AccountQuota, Task};
use saka_core::reconcile::{Action, Snapshot, reconcile};
use saka_core::session::SessionInput;
use saka_core::task::{LoopConfig, TaskInput, decide, evolve};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    BlockerCode, ChangeState, ExternalCause, MergeMethod, SessionState, Stage, StagePhase,
    StageRunState, WorkItemState,
};
use saka_core::workflow::Workflow;
use uuid::Uuid;

struct Ids(u128);

impl IdSource for Ids {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

const HEAD: &str = "1111111111111111111111111111111111111111";
const MOVED: &str = "2222222222222222222222222222222222222222";
const NOW: Timestamp = Timestamp(1_000);

fn cfg() -> LoopConfig {
    LoopConfig {
        max_followup_turns: 1,
        ci_appear_timeout_ms: 600_000,
        poll_interval_ms: 15_000,
        merge_method: MergeMethod::Squash,
        delete_branch_after_merge: true,
        commit_status: true,
        permission_wait_ms: 3_600_000,
        decision_wait_ms: 72 * 3_600_000,
        idle_ms: 1_800_000,
        budgets: BTreeMap::new(),
        budget_warn_percent: 80,
        rate_limit_pause: 0.9,
        account: "claude:default".into(),
        autonomy: saka_core::autonomy::Ceilings::default(),
    }
}

fn incoming() -> Incoming {
    Incoming {
        project_id: ProjectId(Uuid::from_u128(900)),
        repo_id: RepoId(Uuid::from_u128(901)),
        repo_name: "shop".into(),
        number: 7,
        title: "Add retries".into(),
        url: "https://scm.test/acme/shop/pull/7".into(),
        head_branch: "feature/retries".into(),
        base_branch: "main".into(),
        head_sha: HEAD.into(),
        author: "alice".into(),
        from_fork: false,
        draft: false,
        spec: None,
    }
}

struct Run {
    task: Task,
    ids: Ids,
}

impl Run {
    fn new(workflow: &Workflow) -> Run {
        let mut ids = Ids(0);
        let events = external::accept(
            workflow,
            incoming(),
            ExternalCause::Auto,
            None,
            &mut Ctx {
                now: NOW,
                ids: &mut ids,
            },
        )
        .unwrap();
        let mut run = Run {
            task: Task::new(saka_core::model::WorkItem {
                id: WorkItemId(Uuid::nil()),
                project_id: ProjectId(Uuid::nil()),
                origin: saka_core::vocab::WorkItemOrigin::ExternalChange,
                spec_id: None,
                spec_revision_id: None,
                step_id: None,
                step_index: None,
                task_key: String::new(),
                work: String::new(),
                repo_id: RepoId(Uuid::nil()),
                branch: String::new(),
                state: WorkItemState::Queued,
                stage: None,
                stage_phase: None,
                priority: 0,
                version: 1,
                admitted_at: None,
                done_at: None,
                start_approved_at: None,
                created_at: NOW,
                updated_at: NOW,
            }),
            ids,
        };
        let mut task = None;
        for e in &events {
            task = evolve(task, e, NOW);
        }
        run.task = task.unwrap();
        run.task.external.as_mut().unwrap().workflow = workflow.clone();
        run
    }

    fn feed(&mut self, input: TaskInput) -> Vec<Event> {
        let events = decide(
            &self.task,
            input,
            &cfg(),
            &mut Ctx {
                now: NOW,
                ids: &mut self.ids,
            },
        )
        .unwrap();
        let mut task = Some(self.task.clone());
        for e in &events {
            task = evolve(task, e, NOW);
        }
        self.task = task.unwrap();
        events
    }

    fn observe(&mut self, state: ChangeState, head: &str) -> Vec<Event> {
        self.feed(TaskInput::ChangeObserved {
            role: saka_core::vocab::ChangeRole::Code,
            state,
            head_sha: head.into(),
            merge_commit_sha: (state == ChangeState::Merged).then(|| "3".repeat(40)),
            merged_at: None,
            checks: Vec::new(),
        })
    }

    fn start(&mut self) {
        self.feed(TaskInput::StartReview);
    }

    fn prepare(&mut self) {
        self.start();
        self.feed(TaskInput::ReviewEnvironmentPrepared {
            environment_id: EnvironmentId(Uuid::from_u128(77)),
            root_path: "/r".into(),
            worktree_path: "/r/w".into(),
            head_sha: HEAD.into(),
        });
    }

    fn launch(&mut self) -> SessionId {
        let session_id = SessionId(Uuid::from_u128(88));
        self.feed(TaskInput::Session(SessionInput::Launched {
            session_id,
            cli: "claude".into(),
            cli_version: None,
            model: None,
            account: "default".into(),
            runner_unit: "unit".into(),
            socket_path: "/s".into(),
            log_path: "/l".into(),
            author: SessionAuthor::saka(),
        }));
        self.feed(TaskInput::Session(SessionInput::TurnAccepted { turn: 1 }));
        session_id
    }

    fn answer(&mut self, session_id: SessionId, turn: u32, text: &str) -> Vec<Event> {
        if turn > 1 {
            self.feed(TaskInput::Session(SessionInput::TurnAccepted { turn }));
        }
        self.feed(TaskInput::Session(SessionInput::TurnEnded {
            turn,
            status: "success".into(),
            cost_usd_micros: None,
            input_tokens: None,
            output_tokens: None,
        }));
        self.feed(TaskInput::ReviewReported {
            session_id,
            turn,
            text: text.into(),
        })
    }
}

const APPROVE: &str = r#"{"verdict":"approve","summary":"Fine.","findings":[],"questions":[]}"#;

fn auto() -> Workflow {
    Workflow::of(saka_core::vocab::WorkflowPreset::ReviewOnly)
}

fn manual() -> Workflow {
    let mut w = auto();
    w.set_stage(Stage::Review, saka_core::vocab::Trigger::Manual);
    w
}

fn cancelled(events: &[Event], reason: &str) -> bool {
    events
        .iter()
        .any(|e| matches!(e, Event::StageRunCancelled { reason: r, .. } if r == reason))
}

#[test]
fn a_head_that_moves_cancels_the_run_in_every_phase_and_asks_again_on_auto() {
    for phase in [
        StagePhase::AwaitingReview,
        StagePhase::PreparingEnvironment,
        StagePhase::RunningSession,
        StagePhase::PostingReview,
    ] {
        let mut run = Run::new(&auto());
        let mut session = None;
        match phase {
            StagePhase::PreparingEnvironment => run.start(),
            StagePhase::RunningSession | StagePhase::PostingReview => {
                run.prepare();
                session = Some(run.launch());
            }
            _ => {}
        }
        if phase == StagePhase::PostingReview {
            run.answer(session.unwrap(), 1, APPROVE);
        }
        assert!(run.task.is_at(Stage::Review, phase), "{phase:?}");
        let old = run.task.stage_run.as_ref().unwrap().id;

        let events = run.observe(ChangeState::Open, MOVED);
        let live = session.is_some() && phase == StagePhase::RunningSession;
        assert!(cancelled(&events, "head_moved"), "{phase:?}");
        assert_eq!(
            events
                .iter()
                .any(|e| matches!(e, Event::SessionEnded { .. })),
            live,
            "{phase:?}: a live reviewer is ended with the run"
        );
        let now = run.task.stage_run.as_ref().unwrap();
        assert_ne!(now.id, old, "{phase:?}: a new request at the new head");
        assert_eq!(now.head_sha.as_deref(), Some(MOVED));
        assert_eq!(now.state, StageRunState::Pending);
        assert!(run.task.is_at(Stage::Review, StagePhase::AwaitingReview));
    }
}

#[test]
fn a_head_that_moves_on_manual_waits_for_a_command_even_after_a_finished_review() {
    let waiting = Run::new(&manual());
    assert!(
        waiting
            .task
            .is_at(Stage::Review, StagePhase::AwaitingTrigger)
    );
    let mut run = Run::new(&auto());
    run.prepare();
    let session = run.launch();
    run.answer(session, 1, APPROVE);
    run.feed(TaskInput::ReviewPosted {
        review_id: run.task.reviews[0].id,
        event: saka_core::vocab::ReviewEvent::Approve,
        url: None,
    });
    assert!(run.task.is_at(Stage::Review, StagePhase::Reviewed));
    assert_eq!(
        run.task.stage_run.as_ref().unwrap().state,
        StageRunState::Succeeded
    );
    run.task.external.as_mut().unwrap().workflow = manual();
    let events = run.observe(ChangeState::Open, MOVED);
    assert!(cancelled(&events, "head_moved"));
    assert!(run.task.is_at(Stage::Review, StagePhase::AwaitingTrigger));
    assert_eq!(
        run.task.stage_run.as_ref().unwrap().state,
        StageRunState::Succeeded,
        "the finished run keeps its result"
    );
}

#[test]
fn a_blocked_review_follows_the_head_and_ends_with_the_pull_request() {
    let mut run = Run::new(&auto());
    run.prepare();
    let session = run.launch();
    run.answer(session, 1, "not a verdict");
    run.answer(session, 2, "still not");
    assert_eq!(run.task.item.state, WorkItemState::Blocked);
    assert_eq!(
        run.task.open_blocker().unwrap().code,
        BlockerCode::SessionFailed
    );

    let events = run.observe(ChangeState::Open, MOVED);
    assert!(matches!(
        events.as_slice(),
        [Event::ChangeRequestUpdated { .. }]
    ));
    assert_eq!(run.task.change_request.as_ref().unwrap().head_sha, MOVED);
    assert_eq!(run.task.item.state, WorkItemState::Blocked);

    run.observe(ChangeState::Merged, MOVED);
    assert_eq!(run.task.item.state, WorkItemState::Done);
    assert!(
        run.task.open_blocker().is_none(),
        "the blocker closes with the task"
    );
}

#[test]
fn a_verdict_is_taken_only_from_the_live_session_at_its_last_turn() {
    let mut run = Run::new(&auto());
    run.prepare();
    let session = run.launch();
    let other = SessionId(Uuid::from_u128(5));
    run.feed(TaskInput::Session(SessionInput::TurnEnded {
        turn: 1,
        status: "success".into(),
        cost_usd_micros: None,
        input_tokens: None,
        output_tokens: None,
    }));
    for (id, turn) in [(other, 1), (session, 2)] {
        let events = run.feed(TaskInput::ReviewReported {
            session_id: id,
            turn,
            text: APPROVE.into(),
        });
        assert!(events.is_empty());
    }
    assert!(run.task.reviews.is_empty());
    let events = run.feed(TaskInput::ReviewReported {
        session_id: session,
        turn: 1,
        text: APPROVE.into(),
    });
    assert!(!events.is_empty());
    assert_eq!(run.task.reviews.len(), 1);
    assert_eq!(
        run.task.session.as_ref().unwrap().state,
        SessionState::Ended
    );
    let again = run.feed(TaskInput::ReviewReported {
        session_id: session,
        turn: 1,
        text: APPROVE.into(),
    });
    assert!(again.is_empty(), "one verdict per run");
}

fn waiting_task(n: u128, created: i64) -> Task {
    let mut run = Run::new(&auto());
    let id = WorkItemId(Uuid::from_u128(n));
    run.task.item.id = id;
    run.task.stage_run.as_mut().unwrap().created_at = Timestamp(created);
    run.task
}

fn starts(snapshot: &Snapshot, max: usize) -> Vec<WorkItemId> {
    reconcile(snapshot, &cfg(), max, NOW)
        .into_iter()
        .filter_map(|a| match a {
            Action::Decide(id, TaskInput::StartReview) => Some(id),
            _ => None,
        })
        .collect()
}

#[test]
fn reviews_start_oldest_first_while_slots_remain_and_the_account_is_free() {
    let tasks = vec![
        waiting_task(3, 30),
        waiting_task(1, 10),
        waiting_task(2, 20),
    ];
    let mut snapshot = Snapshot {
        tasks,
        ..Snapshot::default()
    };
    assert_eq!(
        starts(&snapshot, 2),
        [
            WorkItemId(Uuid::from_u128(1)),
            WorkItemId(Uuid::from_u128(2))
        ]
    );
    assert!(starts(&snapshot, 0).is_empty());

    let mut running = Run::new(&auto());
    running.task.item.id = WorkItemId(Uuid::from_u128(9));
    running.prepare();
    snapshot.tasks.push(running.task);
    assert_eq!(
        starts(&snapshot, 2).len(),
        1,
        "a running review holds a slot"
    );
    assert!(starts(&snapshot, 1).is_empty());

    snapshot.quotas.push(AccountQuota {
        account: "claude:default".into(),
        window: "five_hour".into(),
        utilization: 0.95,
        status: "allowed".into(),
        resets_at: Some(Timestamp(NOW.0 + 60_000)),
        observed_at: NOW,
    });
    assert!(
        starts(&snapshot, 5).is_empty(),
        "a paused account starts nothing"
    );
}
