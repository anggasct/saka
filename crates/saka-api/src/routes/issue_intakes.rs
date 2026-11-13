use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use saka_core::ids::{IssueIntakeId, ProjectId};
use saka_engine::Produced;

use super::{reply, unexpected};
use crate::dto;
use crate::http::{Mutation, body, encode_cursor, mutate, ok, page, path_id, require_key, states};
use crate::problem::ApiError;
use crate::state::AppState;

const INTAKE_STATES: &[&str] = &[
    "accepted",
    "drafting",
    "awaiting_approval",
    "approved",
    "in_progress",
    "done",
    "dismissed",
    "closed",
    "failed",
];

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/issue-intakes", tag = "issue-intakes",
    params(("project_id" = uuid::Uuid, Path), dto::IssueIntakeQuery),
    responses(
        (status = 200, body = dto::Page<dto::IssueIntake>),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_issue_intakes(
    State(s): State<AppState>,
    Path(project): Path<String>,
    Query(q): Query<dto::IssueIntakeQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    let wanted = tri!(states(q.state.as_deref(), INTAKE_STATES));
    let key = format!("issue_intakes:{project}:{}", wanted.join(","));
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let paged = tri!(engine.issue_intakes(project, &wanted, &p));
    ok(
        StatusCode::OK,
        &dto::Page {
            items: paged
                .items
                .iter()
                .map(dto::IssueIntake::from_view)
                .collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, &key)),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/issue-intakes/{intake_id}", tag = "issue-intakes",
    params(("intake_id" = uuid::Uuid, Path)),
    responses(
        (status = 200, body = dto::IssueIntake),
        (status = 404, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_issue_intake(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = IssueIntakeId(tri!(path_id(&id)));
    let view = tri!(engine.issue_intake(id));
    ok(StatusCode::OK, &dto::IssueIntake::from_view(&view))
}

#[utoipa::path(
    post, path = "/api/v1/projects/{project_id}/issue-intakes", tag = "issue-intakes",
    params(("project_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::CreateIssueIntakeBody,
    responses(
        (status = 201, body = dto::IssueIntake),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create_issue_intake(
    State(s): State<AppState>,
    Path(project): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&project)));
    let req: dto::CreateIssueIntakeBody = tri!(body(&raw));
    if req.number == 0 {
        return ApiError::validation("number", "invalid").into_response();
    }
    let engine = tri!(s.engine().cloned());
    let respond = move |p: Produced<'_>| match p {
        Produced::IssueIntake(v) => reply(StatusCode::CREATED, &dto::IssueIntake::from_view(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "issue_intake.create",
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
            engine
                .create_issue_intake_with(
                    project,
                    req.number,
                    req.repo.as_deref(),
                    &cause,
                    Some(&action),
                )
                .await
                .map(|_| Some(project))
                .map_err(|e| (e, Some(project)))
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/issue-intakes/{intake_id}/actions/dismiss", tag = "issue-intakes",
    params(("intake_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::DismissIssueIntakeBody,
    responses(
        (status = 200, body = dto::IssueIntake),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn dismiss_issue_intake(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let id = IssueIntakeId(tri!(path_id(&id)));
    let req: dto::DismissIssueIntakeBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let project = engine.issue_intake(id).ok().map(|v| v.intake.project_id);
    let respond = |p: Produced<'_>| match p {
        Produced::IssueIntake(v) => reply(StatusCode::OK, &dto::IssueIntake::from_view(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "issue_intake.dismiss",
        target_type: "issue_intake",
        target_id: Some(id.0),
        reason: req.reason.clone(),
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
                .dismiss_issue_intake_with(id, req.reason, &cause, Some(&action))
                .await
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/issue-intakes/{intake_id}/actions/retry", tag = "issue-intakes",
    params(("intake_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::RetryIssueIntakeBody,
    responses(
        (status = 202, body = dto::IssueIntake),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn retry_issue_intake(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let id = IssueIntakeId(tri!(path_id(&id)));
    let _: dto::RetryIssueIntakeBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let project = engine.issue_intake(id).ok().map(|v| v.intake.project_id);
    let respond = |p: Produced<'_>| match p {
        Produced::IssueIntake(v) => reply(StatusCode::ACCEPTED, &dto::IssueIntake::from_view(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "issue_intake.retry",
        target_type: "issue_intake",
        target_id: Some(id.0),
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
            engine
                .retry_issue_intake_with(id, &cause, Some(&action))
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}
