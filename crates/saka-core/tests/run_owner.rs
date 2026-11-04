#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions read clearer with unwrap"
)]

use serde_json::{Value, json};
use uuid::Uuid;

use saka_core::Ctx;
use saka_core::event::{Event, Prompt, Stream};
use saka_core::ids::{
    EnvironmentId, IdSource, PermissionRequestId, PermissionRuleId, ProjectId, RepoId, SessionId,
    SpecDraftAttemptId, SpecDraftId, StageRunId, UsageRecordId, WorkItemId,
};
use saka_core::model::{Coded, RunOwner, Session, Usage};
use saka_core::session::{
    self, RunRetry, RunView, SessionConfig, SessionInput, SessionOutcome, SessionSignal,
};
use saka_core::time::Timestamp;
use saka_core::vocab::{BudgetDimension, InterruptBy, PermissionState, SessionState, Trigger};

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn stage_run() -> RunOwner {
    RunOwner::StageRun {
        work_item_id: WorkItemId(id(1)),
        stage_run_id: StageRunId(id(2)),
    }
}

fn draft_attempt() -> RunOwner {
    RunOwner::DraftAttempt {
        spec_draft_id: SpecDraftId(id(3)),
        attempt_id: SpecDraftAttemptId(id(4)),
    }
}

#[test]
fn run_owners_serialize_as_tagged_objects_and_round_trip() {
    let task = serde_json::to_value(stage_run()).unwrap();
    assert_eq!(
        task,
        json!({ "kind": "stage_run", "work_item_id": id(1), "stage_run_id": id(2) })
    );
    let draft = serde_json::to_value(draft_attempt()).unwrap();
    assert_eq!(
        draft,
        json!({ "kind": "draft_attempt", "spec_draft_id": id(3), "attempt_id": id(4) })
    );
    for owner in [stage_run(), draft_attempt()] {
        let back: RunOwner = serde_json::from_value(serde_json::to_value(owner).unwrap()).unwrap();
        assert_eq!(back, owner);
    }
    let unknown = json!({ "kind": "intake", "work_item_id": id(1) });
    assert!(serde_json::from_value::<RunOwner>(unknown).is_err());
}

fn owned_events(owner: RunOwner) -> Vec<Event> {
    let session_id = SessionId(id(10));
    let permission_request_id = PermissionRequestId(id(11));
    let environment_id = EnvironmentId(id(12));
    vec![
        Event::EnvironmentPrepared {
            owner,
            environment_id,
            repo_id: RepoId(id(13)),
            root_path: "/envs/e".into(),
            worktree_path: "/envs/e/worktree".into(),
            branch: owner.work_item_id().map(|_| "saka/x".to_string()),
            base_sha: "a".repeat(40),
            port_start: None,
            port_count: None,
            recipe: None,
            recipe_sha256: None,
            setup: vec![],
            toolchains: vec![],
            path_scope: vec!["**".into()],
            docs: None,
        },
        Event::EnvironmentTornDown {
            owner,
            environment_id,
            teardown_failures: vec![],
        },
        Event::EnvironmentTeardownFailed {
            owner,
            environment_id,
            error: Coded {
                code: "io".into(),
                params: json!({}),
            },
        },
        Event::BudgetWarned {
            owner,
            dimension: BudgetDimension::Turns,
            used: json!(5),
            limit: json!(6),
        },
        Event::SessionStarted {
            owner,
            session_id,
            environment_id,
            cli: "claude".into(),
            cli_version: None,
            model: None,
            account: "default".into(),
            runner_unit: "saka-session-x".into(),
            socket_path: "/s".into(),
            log_path: "/l".into(),
            resumed_from_session_id: None,
            resume_cause: None,
            author: saka_core::authorship::SessionAuthor::saka(),
        },
        Event::SessionReattached {
            owner,
            session_id,
            runner_version: "1".into(),
            from_offset: 0,
        },
        Event::SessionSteered {
            owner,
            session_id,
            text: "go".into(),
            turn: 1,
            n: 1,
        },
        Event::SessionInterruptRequested {
            owner,
            session_id,
            actor: InterruptBy::Operator,
            turn: 1,
        },
        Event::SessionStopRequested {
            owner,
            session_id,
            reason: "enough".into(),
        },
        Event::SessionInitialized {
            owner,
            session_id,
            cli_session_id: "cli".into(),
            model: None,
        },
        Event::SessionStateChanged {
            owner,
            session_id,
            state: SessionState::Running,
            running_ms: 0,
        },
        Event::SessionTurnRequested {
            owner,
            session_id,
            turn: 1,
            prompt: Prompt::Steer { text: "go".into() },
        },
        Event::SessionTurnEnded {
            owner,
            session_id,
            turn: 1,
            status: "success".into(),
            cost_usd_micros: None,
            input_tokens: None,
            output_tokens: None,
            running_ms: 0,
            account: "claude:default".into(),
            usage_record_id: UsageRecordId(id(14)),
        },
        Event::SessionEnded {
            owner,
            session_id,
            end_reason: Coded {
                code: "completed".into(),
                params: json!({}),
            },
        },
        Event::SessionLost {
            owner,
            session_id,
            cause: "runner_exited".into(),
        },
        Event::QuotaObserved {
            owner,
            session_id,
            account: "claude:default".into(),
            window: "five_hour".into(),
            utilization: 0.5,
            status: "allowed".into(),
            resets_at: None,
        },
        Event::PermissionRequested {
            owner,
            session_id,
            permission_request_id,
            cli_request_id: "r1".into(),
            tool: "Read".into(),
            summary: "a.md".into(),
            input: json!({}),
        },
        Event::PermissionAnswered {
            owner,
            permission_request_id,
            state: PermissionState::Allowed,
            answer_source: "policy:x".into(),
            message: None,
        },
        Event::PermissionDelivered {
            owner,
            permission_request_id,
        },
        Event::PermissionRuleAdded {
            rule_id: PermissionRuleId(id(15)),
            project_id: ProjectId(id(16)),
            owner,
            tool: "Read".into(),
            command: None,
            permission_request_id,
            created_by: "human:operator:local".into(),
        },
    ]
}

#[test]
fn every_owned_event_names_its_task_or_its_draft() {
    for owner in [stage_run(), draft_attempt()] {
        for event in owned_events(owner) {
            assert_eq!(event.owner(), Some(&owner), "{}", event.kind());
            assert_eq!(
                event.work_item_id(),
                owner.work_item_id(),
                "{}",
                event.kind()
            );
            assert_eq!(
                event.spec_draft_id(),
                owner.spec_draft_id(),
                "{}",
                event.kind()
            );
            assert_eq!(
                event.payload().get("owner"),
                Some(&serde_json::to_value(owner).unwrap()),
                "{}",
                event.kind()
            );
            assert!(
                event.payload().get("work_item_id").is_none(),
                "{}",
                event.kind()
            );
            assert!(
                event.payload().get("stage_run_id").is_none(),
                "{}",
                event.kind()
            );
            let back: Event =
                serde_json::from_value(serde_json::to_value(&event).unwrap()).unwrap();
            assert_eq!(back, event);
        }
    }
}

#[test]
fn environment_and_budget_events_stream_to_their_owner() {
    for owner in [stage_run(), draft_attempt()] {
        let expected = match owner {
            RunOwner::StageRun { work_item_id, .. } => Stream::WorkItem(work_item_id),
            RunOwner::DraftAttempt { spec_draft_id, .. } => Stream::SpecDraft(spec_draft_id),
        };
        for event in owned_events(owner) {
            let stream = match &event {
                Event::EnvironmentPrepared { .. }
                | Event::EnvironmentTornDown { .. }
                | Event::EnvironmentTeardownFailed { .. }
                | Event::BudgetWarned { .. } => expected,
                Event::PermissionRequested { .. }
                | Event::PermissionAnswered { .. }
                | Event::PermissionDelivered { .. } => {
                    Stream::PermissionRequest(PermissionRequestId(id(11)))
                }
                Event::PermissionRuleAdded { .. } => Stream::Project(ProjectId(id(16))),
                _ => Stream::Session(SessionId(id(10))),
            };
            assert_eq!(event.stream(), stream, "{}", event.kind());
        }
    }
    assert_eq!(Stream::SpecDraft(SpecDraftId(id(3))).kind(), "spec_draft");
    assert_eq!(Stream::SpecDraft(SpecDraftId(id(3))).id(), id(3));
}

struct Seq(u128);

impl IdSource for Seq {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

fn cfg() -> SessionConfig {
    SessionConfig {
        permission_wait_ms: 3_600_000,
        idle_ms: 1_800_000,
        budget_warn_percent: 80,
        rate_limit_pause: 0.9,
        account: "claude:default".into(),
    }
}

fn draft_session(state: SessionState, turns: u32) -> Session {
    Session {
        id: SessionId(id(4)),
        owner: draft_attempt(),
        environment_id: EnvironmentId(id(4)),
        cli: "claude".into(),
        cli_version: None,
        cli_session_id: Some("cli".into()),
        model: None,
        account: "default".into(),
        state,
        runner_unit: "saka-session-4".into(),
        socket_path: "/s".into(),
        log_path: "/l".into(),
        turns,
        followups: 0,
        running_ms: 0,
        running_since: Some(Timestamp(0)),
        cost_usd_micros: None,
        input_tokens: None,
        output_tokens: None,
        end_reason: None,
        resumed_from_session_id: None,
        resume_cause: None,
        idle_since: None,
        interrupt_requested: None,
        steers: 0,
        last_offset: 0,
        recorded_turn: turns.saturating_sub(1),
        recorded_running_ms: 0,
        started_at: Some(Timestamp(0)),
        ended_at: None,
        created_at: Timestamp(0),
        updated_at: Timestamp(0),
    }
}

fn draft_view(session: Option<&Session>) -> RunView<'_> {
    RunView {
        owner: draft_attempt(),
        project_id: ProjectId(id(16)),
        session,
        permissions: &[],
        prior_usage: Usage::default(),
        budget: None,
        budget_warnings: &[],
        resumed_lost: false,
        ready_environment: Some(EnvironmentId(id(4))),
        retry: RunRetry {
            trigger: Trigger::Auto,
            retried: None,
        },
        packet: Prompt::Steer {
            text: "first turn".into(),
        },
    }
}

fn decide(view: &RunView<'_>, input: SessionInput) -> SessionOutcome {
    let mut ids = Seq(100);
    let mut ctx = Ctx {
        now: Timestamp(1_000),
        ids: &mut ids,
    };
    session::decide(view, input, &cfg(), &mut ctx).unwrap()
}

#[test]
fn the_session_lifecycle_runs_for_a_draft_owner_with_the_owner_packet() {
    let launched = decide(
        &draft_view(None),
        SessionInput::Launched {
            session_id: SessionId(id(4)),
            cli: "claude".into(),
            cli_version: None,
            model: None,
            account: "default".into(),
            runner_unit: "saka-session-4".into(),
            socket_path: "/s".into(),
            log_path: "/l".into(),
            author: saka_core::authorship::SessionAuthor::saka(),
        },
    );
    assert_eq!(launched.signal, None);
    assert!(matches!(
        &launched.events[..],
        [
            Event::SessionStarted { owner: o1, .. },
            Event::SessionTurnRequested { owner: o2, turn: 1, prompt: Prompt::Steer { .. }, .. },
        ] if *o1 == draft_attempt() && *o2 == draft_attempt()
    ));

    let running = draft_session(SessionState::Running, 1);
    let ended = decide(
        &draft_view(Some(&running)),
        SessionInput::TurnEnded {
            turn: 1,
            status: "success".into(),
            cost_usd_micros: None,
            input_tokens: Some(10),
            output_tokens: Some(5),
        },
    );
    assert_eq!(
        ended.signal,
        Some(SessionSignal::TurnCompleted {
            session_id: running.id,
            turn: 1,
        })
    );
    assert!(
        ended
            .events
            .iter()
            .all(|e| e.spec_draft_id() == Some(SpecDraftId(id(3))))
    );

    let jobs = session::react(
        &draft_view(Some(&running)),
        &launched.events[1],
        Timestamp(1),
    );
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        jobs[0].payload.get("owner"),
        Some(&serde_json::to_value(draft_attempt()).unwrap())
    );
}

#[test]
fn a_second_loss_stops_the_draft_run_and_an_operator_stop_names_its_cause() {
    let running = draft_session(SessionState::Running, 1);
    let mut view = draft_view(Some(&running));
    view.resumed_lost = true;
    let lost = decide(
        &view,
        SessionInput::Gone {
            cause: "runner_exited".into(),
        },
    );
    assert_eq!(
        lost.signal,
        Some(SessionSignal::Stopped {
            code: saka_core::vocab::BlockerCode::SessionLost,
            params: json!({ "cause": "runner_exited" }),
        })
    );

    let stopped = decide(
        &draft_view(Some(&running)),
        SessionInput::Stop {
            session_id: running.id,
            reason: "wrong idea".into(),
        },
    );
    let Some(SessionSignal::Stopped { code, params }) = stopped.signal else {
        panic!("expected a stop signal");
    };
    assert_eq!(code, saka_core::vocab::BlockerCode::SessionFailed);
    assert_eq!(params.get("cause"), Some(&Value::from("operator_stop")));
}
