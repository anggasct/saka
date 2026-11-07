use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use saka_core::ids::{ProjectId, SessionId, SpecDraftId, WorkItemId};
use saka_engine::{Produced, RunOf, SessionCommand, SessionView, TimelineCursor};

use super::tasks::opt_id;
use super::{reply, unexpected};
use crate::dto;
use crate::http::{Mutation, body, encode_cursor, mutate, ok, page, path_id, require_key};
use crate::problem::ApiError;
use crate::state::AppState;

#[utoipa::path(
    get, path = "/api/v1/sessions/{session_id}", tag = "runtime",
    params(("session_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::Session), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn get_session(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = tri!(path_id(&id));
    match tri!(engine.session(SessionId(id))) {
        Some(v) => ok(StatusCode::OK, &dto::Session::from_view(&v)),
        None => ApiError::not_found().into_response(),
    }
}

#[utoipa::path(
    get, path = "/api/v1/sessions", tag = "runtime", params(dto::SessionsQuery),
    responses((status = 200, body = dto::SessionsReply), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_sessions(
    State(s): State<AppState>,
    Query(q): Query<dto::SessionsQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let run = tri!(run_of(
        q.work_item_id.as_deref(),
        q.spec_draft_id.as_deref()
    ));
    let active = match q.active.as_deref() {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return ApiError::validation("active", "invalid").into_response(),
    };
    match (run, active) {
        (Some(run), false) => {
            let sessions = tri!(engine.sessions_of(run));
            ok(
                StatusCode::OK,
                &dto::SessionsReply::Run(dto::SessionList {
                    items: sessions.iter().map(dto::Session::from_view).collect(),
                }),
            )
        }
        (None, true) => {
            let project = tri!(opt_id(q.project_id.as_deref(), "project_id")).map(ProjectId);
            let key = format!("sessions:{project:?}");
            let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
            let paged = tri!(engine.active_sessions(project, &p));
            ok(
                StatusCode::OK,
                &dto::SessionsReply::Active(dto::ActiveSessionList {
                    items: paged.items.iter().map(dto::ActiveSession::from).collect(),
                    next_cursor: paged.next.map(|k| encode_cursor(k, &key)),
                }),
            )
        }
        (Some(_), true) => ApiError::validation("active", "exclusive").into_response(),
        (None, false) => ApiError::validation("work_item_id", "required").into_response(),
    }
}

pub(super) fn run_of(
    work_item: Option<&str>,
    spec_draft: Option<&str>,
) -> Result<Option<RunOf>, ApiError> {
    let work_item = opt_id(work_item, "work_item_id")?;
    let spec_draft = opt_id(spec_draft, "spec_draft_id")?;
    match (work_item, spec_draft) {
        (Some(_), Some(_)) => Err(ApiError::validation("spec_draft_id", "exclusive")),
        (Some(id), None) => Ok(Some(RunOf::WorkItem(WorkItemId(id)))),
        (None, Some(id)) => Ok(Some(RunOf::SpecDraft(SpecDraftId(id)))),
        (None, None) => Ok(None),
    }
}

pub(super) const TIMELINE_DEFAULT: u32 = 200;
pub(super) const TIMELINE_MAX: u32 = 500;

#[utoipa::path(
    get, path = "/api/v1/sessions/{session_id}/timeline", tag = "runtime",
    params(("session_id" = uuid::Uuid, Path), dto::TimelineQuery),
    responses((status = 200, body = dto::Timeline), (status = 404, body = dto::Problem, content_type = "application/problem+json"), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn get_timeline(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<dto::TimelineQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let id = SessionId(tri!(path_id(&id)));
    let limit = q.limit.unwrap_or(TIMELINE_DEFAULT);
    if !(1..=TIMELINE_MAX).contains(&limit) {
        return ApiError::validation("limit", "out_of_range").into_response();
    }
    let tail = match q.tail.as_deref() {
        None => false,
        Some("true") => true,
        Some(_) => return ApiError::validation("tail", "invalid").into_response(),
    };
    let cursor = match (q.after, q.before, tail) {
        (None, None, false) => TimelineCursor::Start,
        (Some(after), None, false) => TimelineCursor::After(after),
        (None, Some(before), false) => TimelineCursor::Before(before),
        (None, None, true) => TimelineCursor::Tail,
        _ => return ApiError::validation("before", "exclusive").into_response(),
    };
    let Some(page) = tri!(engine.timeline(id, cursor, limit)) else {
        return ApiError::not_found().into_response();
    };
    ok(
        StatusCode::OK,
        &dto::Timeline {
            items: page.items.iter().map(dto::SessionActivity::from).collect(),
            next_after: page.next_after,
            prev_before: page.prev_before,
        },
    )
}

pub(super) enum SessionAction {
    Steer,
    Interrupt,
    Stop,
}

pub(super) async fn session_action(
    s: AppState,
    id: String,
    request: (Method, Uri, HeaderMap, Bytes),
    which: SessionAction,
) -> Response {
    let (method, uri, headers, raw) = request;
    let key = tri!(require_key(&headers));
    let session = SessionId(tri!(path_id(&id)));
    let engine = tri!(s.engine().cloned());
    let (command, reason) = match which {
        SessionAction::Steer => {
            let req: dto::SteerBody = tri!(body(&raw));
            (SessionCommand::Steer { text: req.text }, None)
        }
        SessionAction::Interrupt => {
            let _: dto::EmptyBody = tri!(body(&raw));
            (SessionCommand::Interrupt, None)
        }
        SessionAction::Stop => {
            let req: dto::RequiredReason = tri!(body(&raw));
            (
                SessionCommand::Stop {
                    reason: req.reason.clone(),
                },
                Some(req.reason),
            )
        }
    };
    let current = tri!(engine.session(session));
    let project = current.as_ref().map(|v| v.project_id);
    let respond = |p: Produced<'_>| match (p, &current) {
        (Produced::Session(session), Some(current)) => reply(
            StatusCode::ACCEPTED,
            &dto::Session::from_view(&SessionView {
                session: session.clone(),
                ..current.clone()
            }),
        ),
        _ => unexpected(),
    };
    let m = Mutation {
        name: match which {
            SessionAction::Steer => "session.steer",
            SessionAction::Interrupt => "session.interrupt",
            SessionAction::Stop => "session.stop",
        },
        target_type: "session",
        target_id: Some(session.0),
        reason,
    };
    mutate(
        &s,
        key,
        &method,
        uri.path(),
        &raw,
        m,
        &respond,
        |cause, action| async move {
            engine
                .session_command(session, command, &cause, Some(&action))
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/sessions/{session_id}/steer", tag = "runtime",
    params(("session_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::SteerBody,
    responses((status = 202, body = dto::Session), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn steer(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    session_action(s, id, (method, uri, headers, raw), SessionAction::Steer).await
}

#[utoipa::path(
    post, path = "/api/v1/sessions/{session_id}/interrupt", tag = "runtime",
    params(("session_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::EmptyBody,
    responses((status = 202, body = dto::Session), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn interrupt(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    session_action(s, id, (method, uri, headers, raw), SessionAction::Interrupt).await
}

#[utoipa::path(
    post, path = "/api/v1/sessions/{session_id}/stop", tag = "runtime",
    params(("session_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::RequiredReason,
    responses((status = 202, body = dto::Session), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn stop(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    session_action(s, id, (method, uri, headers, raw), SessionAction::Stop).await
}
