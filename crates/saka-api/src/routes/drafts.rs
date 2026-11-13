use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use saka_core::ids::{ProjectId, SpecDraftId};
use saka_core::vocab::{DraftLocale, DraftMode, TargetKind, WorkKind};
use saka_engine::{NewSpecDraft, Produced};

use super::{reply, unexpected};
use crate::dto;
use crate::http::{Mutation, body, encode_cursor, mutate, ok, page, path_id, require_key};
use crate::problem::ApiError;
use crate::state::AppState;

pub(super) const DRAFT_ACTIVE: &[&str] = &[
    "queued",
    "preparing",
    "drafting",
    "awaiting_operator",
    "committing",
    "failed",
];
pub(super) const DRAFT_FINISHED: &[&str] = &["succeeded", "cancelled"];

#[utoipa::path(
    post, path = "/api/v1/projects/{project_id}/spec-drafts", tag = "spec-drafts",
    params(("project_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::CreateSpecDraftBody,
    responses(
        (status = 201, body = dto::SpecDraft),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create_spec_draft(
    State(s): State<AppState>,
    Path(project): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let req: dto::CreateSpecDraftBody = tri!(body(&raw));
    let locale = match req.locale.as_deref() {
        None => DraftLocale::En,
        Some(l) => tri!(
            l.parse::<DraftLocale>()
                .map_err(|_| ApiError::validation("locale", "invalid"))
        ),
    };
    let mode = match req.mode.as_deref() {
        None => DraftMode::Draft,
        Some(m) => tri!(
            m.parse::<DraftMode>()
                .map_err(|_| ApiError::validation("mode", "invalid"))
        ),
    };
    let target_kind = match req.target_kind.as_deref() {
        None => TargetKind::New,
        Some(k) => tri!(
            k.parse::<TargetKind>()
                .map_err(|_| ApiError::validation("target_kind", "invalid"))
        ),
    };
    let kind = match req.kind.as_deref() {
        None => WorkKind::Feature,
        Some(k) => tri!(
            k.parse::<WorkKind>()
                .map_err(|_| ApiError::validation("kind", "invalid"))
        ),
    };
    let project = ProjectId(tri!(path_id(&project)));
    let engine = tri!(s.engine().cloned());
    let viewer = engine.clone();
    let respond = move |p: Produced<'_>| match p {
        Produced::Draft(d) => reply(
            StatusCode::CREATED,
            &dto::SpecDraft::from_view(&viewer.draft_view(d.clone(), Vec::new(), None)),
        ),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "spec_draft.create",
        target_type: "project",
        target_id: Some(project.0),
        reason: None,
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
            let new = NewSpecDraft {
                path: req.path,
                idea: req.idea,
                locale,
                mode,
                target_kind,
                kind,
                context_paths: req.context_paths,
                deployment_id: None,
            };
            engine
                .create_spec_draft(project, new, &cause, Some(&action))
                .await
                .map(|_| Some(project))
                .map_err(|e| (e, Some(project)))
        },
    )
    .await
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/spec-drafts", tag = "spec-drafts",
    params(("project_id" = uuid::Uuid, Path), dto::SpecDraftQuery),
    responses(
        (status = 200, body = dto::Page<dto::SpecDraft>),
        (status = 404, body = dto::Problem, content_type = "application/problem+json"),
        (status = 422, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_spec_drafts(
    State(s): State<AppState>,
    Path(project): Path<String>,
    Query(q): Query<dto::SpecDraftQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    let which = q.state.as_deref().unwrap_or("active");
    let states: Vec<String> = match which {
        "active" => DRAFT_ACTIVE,
        "finished" => DRAFT_FINISHED,
        "all" => &[],
        _ => return ApiError::validation("state", "invalid").into_response(),
    }
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    let key = format!("spec_drafts:{project:?}:{which}");
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let paged = tri!(engine.spec_drafts(project, &states, &p));
    ok(
        StatusCode::OK,
        &dto::Page {
            items: paged.items.iter().map(dto::SpecDraft::list_item).collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, &key)),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/spec-drafts/{spec_draft_id}", tag = "spec-drafts",
    params(("spec_draft_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::SpecDraft), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn get_spec_draft(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = SpecDraftId(tri!(path_id(&id)));
    let view = tri!(engine.spec_draft(id));
    ok(StatusCode::OK, &dto::SpecDraft::from_view(&view))
}

#[utoipa::path(
    get, path = "/api/v1/spec-drafts/{spec_draft_id}/lint-rounds", tag = "spec-drafts",
    params(("spec_draft_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::SpecDraftLintRoundList), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_spec_draft_rounds(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = SpecDraftId(tri!(path_id(&id)));
    let rounds = tri!(engine.spec_draft_rounds(id));
    ok(
        StatusCode::OK,
        &dto::SpecDraftLintRoundList {
            items: rounds
                .iter()
                .map(|(attempt, r)| dto::SpecDraftLintRound::from_round(*attempt, r))
                .collect(),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/spec-drafts/{spec_draft_id}/content", tag = "spec-drafts",
    params(("spec_draft_id" = uuid::Uuid, Path)),
    responses(
        (status = 200, body = dto::SpecDraftContent),
        (status = 404, body = dto::Problem, content_type = "application/problem+json"),
        (status = 422, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_spec_draft_content(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = SpecDraftId(tri!(path_id(&id)));
    let content = tri!(engine.spec_draft_content(id));
    ok(StatusCode::OK, &dto::SpecDraftContent::from_view(&content))
}

#[utoipa::path(
    get, path = "/api/v1/spec-drafts/{spec_draft_id}/changes", tag = "spec-drafts",
    params(("spec_draft_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::SpecDraftChanges), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn get_spec_draft_changes(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine().cloned());
    let id = SpecDraftId(tri!(path_id(&id)));
    let changes = tri!(engine.spec_draft_changes(id).await);
    ok(StatusCode::OK, &dto::SpecDraftChanges::from_view(&changes))
}

#[utoipa::path(
    get, path = "/api/v1/spec-drafts/{spec_draft_id}/packet", tag = "spec-drafts",
    params(("spec_draft_id" = uuid::Uuid, Path)),
    responses(
        (status = 200, body = dto::SpecDraftPacket),
        (status = 404, body = dto::Problem, content_type = "application/problem+json"),
        (status = 409, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_spec_draft_packet(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = SpecDraftId(tri!(path_id(&id)));
    let packet = tri!(engine.spec_draft_packet(id));
    ok(StatusCode::OK, &dto::SpecDraftPacket::from_view(&packet))
}

#[derive(Clone, Copy)]
pub(super) enum DraftAction {
    Retry,
    Cancel,
    Message,
    Commit,
    Discard,
}

pub(super) async fn draft_action(
    s: AppState,
    id: String,
    (method, uri, headers, raw): (Method, Uri, HeaderMap, Bytes),
    which: DraftAction,
) -> Response {
    let key = tri!(require_key(&headers));
    let draft = SpecDraftId(tri!(path_id(&id)));
    let (reason, text) = match which {
        DraftAction::Retry => {
            let req: dto::RetrySpecDraftBody = tri!(body(&raw));
            (Some(req.reason), req.note)
        }
        DraftAction::Cancel | DraftAction::Discard => {
            let req: dto::RequiredReason = tri!(body(&raw));
            (Some(req.reason), None)
        }
        DraftAction::Message => {
            let req: dto::MessageSpecDraftBody = tri!(body(&raw));
            (None, Some(req.text))
        }
        DraftAction::Commit => {
            let req: dto::CommitSpecDraftBody = tri!(body(&raw));
            (None, req.message)
        }
    };
    let engine = tri!(s.engine().cloned());
    let before = engine.spec_draft(draft).ok();
    let project = before.as_ref().map(|v| v.draft.draft.project_id);
    let last_round = before.and_then(|v| v.last_round);
    let viewer = engine.clone();
    let respond = move |p: Produced<'_>| match p {
        Produced::Draft(d) => reply(
            StatusCode::ACCEPTED,
            &dto::SpecDraft::from_view(&viewer.draft_view(
                d.clone(),
                Vec::new(),
                last_round.clone(),
            )),
        ),
        _ => unexpected(),
    };
    let m = Mutation {
        name: match which {
            DraftAction::Retry => "spec_draft.retry",
            DraftAction::Cancel => "spec_draft.cancel",
            DraftAction::Message => "spec_draft.message",
            DraftAction::Commit => "spec_draft.commit",
            DraftAction::Discard => "spec_draft.discard",
        },
        target_type: "spec_draft",
        target_id: Some(draft.0),
        reason: reason.clone(),
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
            let why = reason.unwrap_or_default();
            let result = match which {
                DraftAction::Retry => {
                    engine.retry_spec_draft(draft, &why, text, &cause, Some(&action))
                }
                DraftAction::Cancel => engine.cancel_spec_draft(draft, &why, &cause, Some(&action)),
                DraftAction::Discard => {
                    engine.discard_spec_draft(draft, &why, &cause, Some(&action))
                }
                DraftAction::Message => engine.message_spec_draft(
                    draft,
                    text.as_deref().unwrap_or_default(),
                    &cause,
                    Some(&action),
                ),
                DraftAction::Commit => {
                    engine
                        .commit_spec_draft(draft, text.as_deref(), &cause, Some(&action))
                        .await
                }
            };
            result.map(|_| project).map_err(|e| (e, project))
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/spec-drafts/{spec_draft_id}/actions/retry", tag = "spec-drafts",
    params(("spec_draft_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::RetrySpecDraftBody,
    responses((status = 202, body = dto::SpecDraft), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn retry_spec_draft(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    draft_action(s, id, (method, uri, headers, raw), DraftAction::Retry).await
}

#[utoipa::path(
    post, path = "/api/v1/spec-drafts/{spec_draft_id}/actions/cancel", tag = "spec-drafts",
    params(("spec_draft_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::RequiredReason,
    responses((status = 202, body = dto::SpecDraft), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn cancel_spec_draft(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    draft_action(s, id, (method, uri, headers, raw), DraftAction::Cancel).await
}

#[utoipa::path(
    post, path = "/api/v1/spec-drafts/{spec_draft_id}/message", tag = "spec-drafts",
    params(("spec_draft_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::MessageSpecDraftBody,
    responses((status = 202, body = dto::SpecDraft), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn message_spec_draft(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    draft_action(s, id, (method, uri, headers, raw), DraftAction::Message).await
}

#[utoipa::path(
    post, path = "/api/v1/spec-drafts/{spec_draft_id}/commit", tag = "spec-drafts",
    params(("spec_draft_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::CommitSpecDraftBody,
    responses((status = 202, body = dto::SpecDraft), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn commit_spec_draft(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    draft_action(s, id, (method, uri, headers, raw), DraftAction::Commit).await
}

#[utoipa::path(
    post, path = "/api/v1/spec-drafts/{spec_draft_id}/discard", tag = "spec-drafts",
    params(("spec_draft_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::RequiredReason,
    responses((status = 202, body = dto::SpecDraft), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn discard_spec_draft(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    draft_action(s, id, (method, uri, headers, raw), DraftAction::Discard).await
}
