use serde_json::Value;

use crate::Ctx;
use crate::event::Event;
use crate::ids::{IssueIntakeId, ProjectId, RepoId, SpecDraftId, SpecId};
use crate::model::Coded;
use crate::rejection::Rejection;
use crate::time::Timestamp;
use crate::vocab::{IssueCause, IssueIntakeState, IssueLane, WorkKind};

pub const MANUAL_RETRY_LIMIT: u32 = 3;

#[derive(Debug, Clone, PartialEq)]
pub struct IssueIntake {
    pub id: IssueIntakeId,
    pub project_id: ProjectId,
    pub repo_id: RepoId,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub author: String,
    pub state: IssueIntakeState,
    pub lane: IssueLane,
    pub kind: WorkKind,
    pub cause: IssueCause,
    pub actor: String,
    pub spec_id: Option<SpecId>,
    pub draft_id: Option<SpecDraftId>,
    pub failure: Option<Coded>,
    pub retries: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewIntake {
    pub project_id: ProjectId,
    pub repo_id: RepoId,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub author: String,
    pub lane: IssueLane,
    pub kind: WorkKind,
    pub cause: IssueCause,
    pub actor: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Accept(NewIntake),
    DraftStarted { draft_id: SpecDraftId },
    SpecReady { spec_id: SpecId },
    Approved,
    TaskStarted,
    Shipped,
    IssueClosed,
    Dismiss { reason: Option<String> },
    Fail { code: String, params: Value },
    Retry,
}

impl IssueIntake {
    pub fn is_live(&self) -> bool {
        !matches!(
            self.state,
            IssueIntakeState::Dismissed | IssueIntakeState::Closed | IssueIntakeState::Done
        )
    }

    pub fn is_approved(&self) -> bool {
        matches!(
            self.state,
            IssueIntakeState::Approved | IssueIntakeState::InProgress
        )
    }

    pub fn is_before_approval(&self) -> bool {
        matches!(
            self.state,
            IssueIntakeState::Accepted
                | IssueIntakeState::Drafting
                | IssueIntakeState::AwaitingApproval
                | IssueIntakeState::Failed
        )
    }
}

fn invalid_state(intake: &IssueIntake) -> Rejection {
    Rejection::InvalidState {
        state: intake.state.as_str().to_string(),
    }
}

fn change(intake: &IssueIntake, to: IssueIntakeState) -> Event {
    Event::IssueIntakeStateChanged {
        intake_id: intake.id,
        from: intake.state,
        to,
    }
}

pub fn decide(
    state: Option<&IssueIntake>,
    input: Input,
    ctx: &mut Ctx<'_>,
) -> Result<Vec<Event>, Rejection> {
    let Input::Accept(new) = input else {
        let intake = state.ok_or(Rejection::NotFound)?;
        return decide_existing(intake, input);
    };
    if state.is_some() {
        return Err(Rejection::Unsupported);
    }
    let intake_id = IssueIntakeId::new(ctx.ids);
    Ok(vec![Event::IssueIntakeAccepted {
        intake_id,
        project_id: new.project_id,
        repo_id: new.repo_id,
        number: new.number,
        url: new.url,
        title: new.title,
        author: new.author,
        lane: new.lane,
        kind: new.kind,
        cause: new.cause,
        actor: new.actor,
    }])
}

fn decide_existing(intake: &IssueIntake, input: Input) -> Result<Vec<Event>, Rejection> {
    use IssueIntakeState as S;
    match input {
        Input::Accept(_) => Err(Rejection::Unsupported),
        Input::DraftStarted { draft_id } => match intake.state {
            S::Accepted if intake.lane == IssueLane::Draft => Ok(vec![
                Event::IssueIntakeLinked {
                    intake_id: intake.id,
                    spec_id: None,
                    draft_id: Some(draft_id),
                },
                change(intake, S::Drafting),
            ]),
            _ => Err(invalid_state(intake)),
        },
        Input::SpecReady { spec_id } => match intake.state {
            S::Accepted | S::Drafting => Ok(vec![
                Event::IssueIntakeLinked {
                    intake_id: intake.id,
                    spec_id: Some(spec_id),
                    draft_id: intake.draft_id,
                },
                change(intake, S::AwaitingApproval),
            ]),
            _ => Err(invalid_state(intake)),
        },
        Input::Approved => match intake.state {
            S::AwaitingApproval => Ok(vec![change(intake, S::Approved)]),
            _ => Err(invalid_state(intake)),
        },
        Input::TaskStarted => match intake.state {
            S::Approved => Ok(vec![change(intake, S::InProgress)]),
            _ => Err(invalid_state(intake)),
        },
        Input::Shipped => match intake.state {
            S::Approved | S::InProgress => Ok(vec![change(intake, S::Done)]),
            _ => Err(invalid_state(intake)),
        },
        Input::IssueClosed => {
            if intake.is_before_approval() {
                Ok(vec![change(intake, S::Closed)])
            } else {
                Ok(Vec::new())
            }
        }
        Input::Dismiss { reason } => {
            if intake.is_approved() {
                return Err(Rejection::IntakeAlreadyApproved);
            }
            if !intake.is_live() {
                return Err(Rejection::IntakeClosed {
                    state: intake.state.as_str(),
                });
            }
            Ok(vec![
                change(intake, S::Dismissed),
                Event::IssueIntakeDismissed {
                    intake_id: intake.id,
                    reason,
                },
            ])
        }
        Input::Fail { code, params } => match intake.state {
            S::Accepted | S::Drafting | S::AwaitingApproval => Ok(vec![
                change(intake, S::Failed),
                Event::IssueIntakeFailed {
                    intake_id: intake.id,
                    code,
                    params,
                },
            ]),
            _ => Err(invalid_state(intake)),
        },
        Input::Retry => {
            if intake.state != S::Failed {
                return Err(Rejection::IntakeNotFailed);
            }
            if intake.retries >= MANUAL_RETRY_LIMIT {
                return Err(Rejection::IntakeRetriesExhausted);
            }
            Ok(vec![
                Event::IssueIntakeRetried {
                    intake_id: intake.id,
                    retries: intake.retries + 1,
                },
                change(intake, S::Accepted),
            ])
        }
    }
}

pub fn evolve_all(
    mut intake: Option<IssueIntake>,
    events: &[Event],
    now: Timestamp,
) -> Option<IssueIntake> {
    for event in events {
        intake = evolve(intake, event, now);
    }
    intake
}

pub fn evolve(intake: Option<IssueIntake>, event: &Event, now: Timestamp) -> Option<IssueIntake> {
    if let Event::IssueIntakeAccepted {
        intake_id,
        project_id,
        repo_id,
        number,
        url,
        title,
        author,
        lane,
        kind,
        cause,
        actor,
    } = event
    {
        return Some(IssueIntake {
            id: *intake_id,
            project_id: *project_id,
            repo_id: *repo_id,
            number: *number,
            url: url.clone(),
            title: title.clone(),
            author: author.clone(),
            state: IssueIntakeState::Accepted,
            lane: *lane,
            kind: *kind,
            cause: *cause,
            actor: actor.clone(),
            spec_id: None,
            draft_id: None,
            failure: None,
            retries: 0,
            created_at: now,
            updated_at: now,
        });
    }
    let mut intake = intake?;
    match event {
        Event::IssueIntakeLinked {
            spec_id, draft_id, ..
        } => {
            intake.spec_id = spec_id.or(intake.spec_id);
            intake.draft_id = draft_id.or(intake.draft_id);
        }
        Event::IssueIntakeStateChanged { to, .. } => {
            intake.state = *to;
            if *to != IssueIntakeState::Failed {
                intake.failure = None;
            }
        }
        Event::IssueIntakeFailed { code, params, .. } => {
            intake.failure = Some(Coded {
                code: code.clone(),
                params: params.clone(),
            });
        }
        Event::IssueIntakeRetried { retries, .. } => {
            intake.retries = *retries;
            intake.draft_id = None;
        }
        Event::IssueIntakeDismissed { .. } => {}
        _ => return Some(intake),
    }
    intake.updated_at = now;
    Some(intake)
}
