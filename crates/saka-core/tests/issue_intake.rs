#![allow(clippy::unwrap_used, reason = "test assertions")]

use proptest::prelude::*;
use saka_core::Ctx;
use saka_core::event::Event;
use saka_core::ids::{IdSource, ProjectId, RepoId, SpecDraftId, SpecId};
use saka_core::issue_intake::{self, Input, IssueIntake, NewIntake};
use saka_core::rejection::Rejection;
use saka_core::time::Timestamp;
use saka_core::vocab::{IssueCause, IssueIntakeState as S, IssueLane, WorkKind};
use serde_json::json;
use uuid::Uuid;

const T0: Timestamp = Timestamp(1_700_000_000_000);

struct Seq(u128);

impl IdSource for Seq {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

fn new_intake(lane: IssueLane) -> NewIntake {
    NewIntake {
        project_id: ProjectId(Uuid::from_u128(900)),
        repo_id: RepoId(Uuid::from_u128(901)),
        number: 42,
        url: "https://github.com/acme/app/issues/42".into(),
        title: "Crash on start".into(),
        author: "alice".into(),
        lane,
        kind: if lane == IssueLane::Bugfix {
            WorkKind::Bugfix
        } else {
            WorkKind::Feature
        },
        cause: IssueCause::Label,
        actor: "maintainer".into(),
    }
}

struct Run {
    intake: IssueIntake,
    ids: Seq,
    events: Vec<Event>,
}

impl Run {
    fn start(lane: IssueLane) -> Run {
        let mut ids = Seq(0);
        let mut ctx = Ctx {
            now: T0,
            ids: &mut ids,
        };
        let events = issue_intake::decide(None, Input::Accept(new_intake(lane)), &mut ctx).unwrap();
        let intake = issue_intake::evolve_all(None, &events, T0).unwrap();
        Run {
            intake,
            ids,
            events,
        }
    }

    fn apply(&mut self, input: Input) -> Result<(), Rejection> {
        let mut ctx = Ctx {
            now: T0.plus_ms(1000),
            ids: &mut self.ids,
        };
        let produced = issue_intake::decide(Some(&self.intake), input, &mut ctx)?;
        self.intake =
            issue_intake::evolve_all(Some(self.intake.clone()), &produced, T0.plus_ms(1000))
                .unwrap();
        self.events.extend(produced);
        Ok(())
    }

    fn state(&self) -> S {
        self.intake.state
    }
}

fn spec() -> SpecId {
    SpecId(Uuid::from_u128(500))
}

fn draft() -> SpecDraftId {
    SpecDraftId(Uuid::from_u128(600))
}

#[test]
fn an_accepted_intake_starts_with_who_asked_and_why() {
    let run = Run::start(IssueLane::Bugfix);
    assert_eq!(run.state(), S::Accepted);
    assert_eq!(
        (run.intake.cause, run.intake.actor.as_str()),
        (IssueCause::Label, "maintainer")
    );
    assert_eq!(run.intake.retries, 0);
    assert_eq!(run.events.len(), 1);
}

#[test]
fn the_bugfix_lane_goes_from_accepted_to_done_through_the_approval() {
    let mut run = Run::start(IssueLane::Bugfix);
    run.apply(Input::SpecReady { spec_id: spec() }).unwrap();
    assert_eq!(
        (run.state(), run.intake.spec_id),
        (S::AwaitingApproval, Some(spec()))
    );
    run.apply(Input::Approved).unwrap();
    run.apply(Input::TaskStarted).unwrap();
    assert_eq!(run.state(), S::InProgress);
    run.apply(Input::Shipped).unwrap();
    assert_eq!(run.state(), S::Done);
    assert!(!run.intake.is_live());
}

#[test]
fn the_draft_lane_waits_in_drafting_until_the_spec_is_ready() {
    let mut run = Run::start(IssueLane::Draft);
    run.apply(Input::DraftStarted { draft_id: draft() })
        .unwrap();
    assert_eq!(
        (run.state(), run.intake.draft_id),
        (S::Drafting, Some(draft()))
    );
    run.apply(Input::SpecReady { spec_id: spec() }).unwrap();
    assert_eq!(run.state(), S::AwaitingApproval);
    assert_eq!(run.intake.draft_id, Some(draft()), "the draft link stays");
}

#[test]
fn a_bugfix_intake_never_starts_a_draft() {
    let mut run = Run::start(IssueLane::Bugfix);
    let refused = run.apply(Input::DraftStarted { draft_id: draft() });
    assert!(matches!(refused, Err(Rejection::InvalidState { .. })));
    assert_eq!(run.state(), S::Accepted);
}

#[test]
fn an_approved_task_can_finish_without_a_separate_start() {
    let mut run = Run::start(IssueLane::Bugfix);
    run.apply(Input::SpecReady { spec_id: spec() }).unwrap();
    run.apply(Input::Approved).unwrap();
    run.apply(Input::Shipped).unwrap();
    assert_eq!(run.state(), S::Done);
}

#[test]
fn steps_out_of_order_are_refused_and_change_nothing() {
    let mut run = Run::start(IssueLane::Bugfix);
    for input in [Input::Approved, Input::TaskStarted, Input::Shipped] {
        let before = run.intake.clone();
        assert!(matches!(
            run.apply(input),
            Err(Rejection::InvalidState { .. })
        ));
        assert_eq!(run.intake, before);
    }
}

#[test]
fn a_closed_issue_ends_an_intake_that_is_not_approved_yet() {
    for stage in 0..3 {
        let mut run = Run::start(IssueLane::Draft);
        if stage >= 1 {
            run.apply(Input::DraftStarted { draft_id: draft() })
                .unwrap();
        }
        if stage >= 2 {
            run.apply(Input::SpecReady { spec_id: spec() }).unwrap();
        }
        run.apply(Input::IssueClosed).unwrap();
        assert_eq!(run.state(), S::Closed, "stage {stage}");
        assert!(!run.intake.is_live());
    }
}

#[test]
fn a_closed_issue_leaves_an_approved_intake_alone() {
    let mut run = Run::start(IssueLane::Bugfix);
    run.apply(Input::SpecReady { spec_id: spec() }).unwrap();
    run.apply(Input::Approved).unwrap();
    let events = run.events.len();
    run.apply(Input::IssueClosed).unwrap();
    assert_eq!((run.state(), run.events.len()), (S::Approved, events));
}

#[test]
fn dismissing_before_approval_ends_the_intake_with_the_reason() {
    let mut run = Run::start(IssueLane::Bugfix);
    run.apply(Input::SpecReady { spec_id: spec() }).unwrap();
    run.apply(Input::Dismiss {
        reason: Some("duplicate of #7".into()),
    })
    .unwrap();
    assert_eq!(run.state(), S::Dismissed);
    assert!(run.events.iter().any(|e| matches!(
        e,
        Event::IssueIntakeDismissed { reason: Some(r), .. } if r == "duplicate of #7"
    )));
}

#[test]
fn dismissing_after_approval_or_twice_is_refused() {
    let mut run = Run::start(IssueLane::Bugfix);
    run.apply(Input::SpecReady { spec_id: spec() }).unwrap();
    run.apply(Input::Approved).unwrap();
    assert!(matches!(
        run.apply(Input::Dismiss { reason: None }),
        Err(Rejection::IntakeAlreadyApproved)
    ));
    run.apply(Input::TaskStarted).unwrap();
    assert!(matches!(
        run.apply(Input::Dismiss { reason: None }),
        Err(Rejection::IntakeAlreadyApproved)
    ));

    let mut other = Run::start(IssueLane::Bugfix);
    other.apply(Input::Dismiss { reason: None }).unwrap();
    assert!(matches!(
        other.apply(Input::Dismiss { reason: None }),
        Err(Rejection::IntakeClosed { state: "dismissed" })
    ));
}

#[test]
fn a_failure_carries_its_code_and_blocks_a_new_intake_until_retried_or_dismissed() {
    let mut run = Run::start(IssueLane::Draft);
    run.apply(Input::DraftStarted { draft_id: draft() })
        .unwrap();
    run.apply(Input::Fail {
        code: "draft_failed".into(),
        params: json!({ "draft": "x" }),
    })
    .unwrap();
    assert_eq!(run.state(), S::Failed);
    let failure = run.intake.failure.clone().unwrap();
    assert_eq!(failure.code, "draft_failed");
    assert!(run.intake.is_live());
    run.apply(Input::Dismiss { reason: None }).unwrap();
    assert_eq!(run.state(), S::Dismissed);
    assert!(run.intake.failure.is_none());
}

#[test]
fn a_retry_goes_back_to_accepted_three_times_then_is_exhausted() {
    let mut run = Run::start(IssueLane::Draft);
    for attempt in 1..=3 {
        run.apply(Input::DraftStarted { draft_id: draft() })
            .unwrap();
        run.apply(Input::Fail {
            code: "draft_failed".into(),
            params: json!({}),
        })
        .unwrap();
        run.apply(Input::Retry).unwrap();
        assert_eq!(
            (run.state(), run.intake.retries, run.intake.draft_id),
            (S::Accepted, attempt, None),
            "attempt {attempt}"
        );
        assert!(run.intake.failure.is_none());
    }
    run.apply(Input::DraftStarted { draft_id: draft() })
        .unwrap();
    run.apply(Input::Fail {
        code: "draft_failed".into(),
        params: json!({}),
    })
    .unwrap();
    assert!(matches!(
        run.apply(Input::Retry),
        Err(Rejection::IntakeRetriesExhausted)
    ));
    assert_eq!(run.state(), S::Failed);
}

#[test]
fn only_a_failed_intake_can_be_retried() {
    let mut run = Run::start(IssueLane::Bugfix);
    assert!(matches!(
        run.apply(Input::Retry),
        Err(Rejection::IntakeNotFailed)
    ));
}

#[test]
fn an_unknown_intake_is_not_found_and_an_existing_one_cannot_be_accepted_again() {
    let mut ids = Seq(0);
    let mut ctx = Ctx {
        now: T0,
        ids: &mut ids,
    };
    assert!(matches!(
        issue_intake::decide(None, Input::Approved, &mut ctx),
        Err(Rejection::NotFound)
    ));
    let run = Run::start(IssueLane::Bugfix);
    let mut ctx = Ctx {
        now: T0,
        ids: &mut ids,
    };
    assert!(
        issue_intake::decide(
            Some(&run.intake),
            Input::Accept(new_intake(IssueLane::Bugfix)),
            &mut ctx
        )
        .is_err()
    );
}

fn any_input() -> impl Strategy<Value = Input> {
    prop_oneof![
        Just(Input::DraftStarted { draft_id: draft() }),
        Just(Input::SpecReady { spec_id: spec() }),
        Just(Input::Approved),
        Just(Input::TaskStarted),
        Just(Input::Shipped),
        Just(Input::IssueClosed),
        Just(Input::Dismiss { reason: None }),
        Just(Input::Fail {
            code: "x".into(),
            params: json!({})
        }),
        Just(Input::Retry),
    ]
}

proptest! {
    #[test]
    fn no_sequence_of_inputs_breaks_the_intake_invariants(
        draft_lane in any::<bool>(),
        inputs in proptest::collection::vec(any_input(), 0..40),
    ) {
        let mut run = Run::start(if draft_lane { IssueLane::Draft } else { IssueLane::Bugfix });
        let mut was_done_or_over = false;
        for input in inputs {
            let before = run.intake.clone();
            let outcome = run.apply(input);
            let now = &run.intake;
            prop_assert_eq!(now.state == S::Failed, now.failure.is_some());
            prop_assert!(now.retries <= issue_intake::MANUAL_RETRY_LIMIT);
            if outcome.is_err() {
                prop_assert_eq!(&before, now);
            }
            if was_done_or_over {
                prop_assert_eq!(before.state, now.state);
            }
            was_done_or_over = !now.is_live();
            if before.is_approved() {
                prop_assert!(matches!(now.state, S::Approved | S::InProgress | S::Done));
            }
        }
    }
}
