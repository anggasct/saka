use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use saka_core::ids::{BlockerId, ProjectId, SpecId, WorkItemId};
use saka_core::vocab::{BlockerAction, Stage};
use saka_engine::{EngineError, Produced, WorkItemFilter};

use super::{WORK_ITEM_STATES, reply, unexpected};
use crate::dto;
use crate::http::{
    Mutation, body, encode_cursor, if_match, mutate, ok, page, path_id, require_key, states,
};
use crate::problem::ApiError;
use crate::state::AppState;

pub(super) fn opt_id(raw: Option<&str>, field: &str) -> Result<Option<uuid::Uuid>, ApiError> {
    match raw.filter(|r| !r.is_empty()) {
        None => Ok(None),
        Some(r) => r
            .parse()
            .map(Some)
            .map_err(|_| ApiError::validation(field, "invalid")),
    }
}

#[utoipa::path(
    get, path = "/api/v1/work-items", tag = "delivery", params(dto::WorkItemQuery),
    responses((status = 200, body = dto::Page<dto::WorkItem>), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_work_items(
    State(s): State<AppState>,
    Query(q): Query<dto::WorkItemQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let filter = WorkItemFilter {
        project: tri!(opt_id(q.project_id.as_deref(), "project_id")).map(ProjectId),
        spec: tri!(opt_id(q.spec_id.as_deref(), "spec_id")).map(SpecId),
        states: tri!(states(q.state.as_deref(), WORK_ITEM_STATES)),
    };
    let key = format!(
        "work-items:{:?}:{:?}:{}",
        filter.project,
        filter.spec,
        filter.states.join(",")
    );
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let paged = tri!(engine.work_items(&filter, &p));
    ok(
        StatusCode::OK,
        &dto::Page {
            items: paged.items.iter().map(dto::WorkItem::from_view).collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, &key)),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/work-items/{work_item_id}", tag = "delivery",
    params(("work_item_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::WorkItemDetail), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn get_work_item(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = tri!(path_id(&id));
    match tri!(engine.work_item(WorkItemId(id))) {
        Some(d) => ok(StatusCode::OK, &dto::WorkItemDetail::from_detail(&d)),
        None => ApiError::not_found().into_response(),
    }
}

#[utoipa::path(
    get, path = "/api/v1/work-items/{work_item_id}/work-packet", tag = "delivery",
    params(("work_item_id" = uuid::Uuid, Path)),
    responses(
        (status = 200, body = dto::WorkPacket),
        (status = 404, body = dto::Problem, content_type = "application/problem+json"),
        (status = 409, body = dto::Problem, content_type = "application/problem+json")
    )
)]
pub async fn get_work_packet(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = tri!(path_id(&id));
    let (task, packet) = tri!(engine.work_packet(WorkItemId(id)));
    ok(
        StatusCode::OK,
        &dto::WorkPacket::from_packet(&task, &packet),
    )
}

#[derive(Clone, Copy)]
pub(super) enum TaskAction {
    Start,
    Merge,
    Retry,
    Cancel,
    Primary,
    Takeover,
    Return,
    Revert,
}

#[allow(clippy::too_many_arguments, reason = "the request parts of one action")]
pub(super) async fn task_action(
    s: AppState,
    id: String,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
    which: TaskAction,
) -> Response {
    let key = tri!(require_key(&headers));
    let item = WorkItemId(tri!(path_id(&id)));
    let engine = tri!(s.engine().cloned());
    let mut blocker = None;
    let mut expected = None;
    let mut return_stage = None;
    let (version, reason) = match which {
        TaskAction::Merge | TaskAction::Start => {
            let tag = tri!(if_match(&headers));
            let req: dto::OptionalReason = tri!(body(&raw));
            let version = tag
                .strip_prefix("W/")
                .unwrap_or(&tag)
                .trim_matches('"')
                .parse::<u64>()
                .ok();
            (Some(version), req.reason)
        }
        TaskAction::Revert => {
            let tag = tri!(if_match(&headers));
            let req: dto::RequiredReason = tri!(body(&raw));
            let version = tag
                .strip_prefix("W/")
                .unwrap_or(&tag)
                .trim_matches('"')
                .parse::<u64>()
                .ok();
            (Some(version), Some(req.reason))
        }
        TaskAction::Retry | TaskAction::Cancel | TaskAction::Takeover => {
            let req: dto::RequiredReason = tri!(body(&raw));
            (None, Some(req.reason))
        }
        TaskAction::Return => {
            let req: dto::ReturnBody = tri!(body(&raw));
            return_stage = match req.stage.as_deref().map(str::parse::<Stage>) {
                None => None,
                Some(Ok(stage @ (Stage::Dev | Stage::Review | Stage::Fix | Stage::Merge))) => {
                    Some(stage)
                }
                Some(_) => return ApiError::validation("stage", "invalid").into_response(),
            };
            (None, Some(req.reason))
        }
        TaskAction::Primary => {
            let req: dto::PrimaryActionBody = tri!(body(&raw));
            blocker = Some(BlockerId(req.blocker_id));
            expected = match req.action.as_deref().map(str::parse::<BlockerAction>) {
                None => None,
                Some(Ok(kind)) => Some(kind),
                Some(Err(_)) => {
                    return ApiError::validation("action", "invalid").into_response();
                }
            };
            (None, Some(req.reason))
        }
    };
    let detail = tri!(engine.work_item(item));
    let project = detail.as_ref().map(|d| d.task.item.project_id);
    let repo_name = detail.and_then(|d| d.repo_name);
    let respond = |p: Produced<'_>| match p {
        Produced::Task(t) => reply(
            StatusCode::ACCEPTED,
            &dto::WorkItem::from_task(t, repo_name.as_deref(), &[], None),
        ),
        _ => unexpected(),
    };
    let name = match which {
        TaskAction::Start => "work_item.start",
        TaskAction::Merge => "work_item.merge",
        TaskAction::Retry => "work_item.retry",
        TaskAction::Cancel => "work_item.cancel",
        TaskAction::Primary => "work_item.primary_action",
        TaskAction::Takeover => "work_item.takeover",
        TaskAction::Return => "work_item.return",
        TaskAction::Revert => "work_item.revert",
    };
    let m = Mutation {
        name,
        target_type: "work_item",
        target_id: Some(item.0),
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
            let result = match which {
                TaskAction::Start => match version.flatten() {
                    Some(v) => engine.start_step_with(item, v, &cause, Some(&action)),
                    None => Err(EngineError::Rejected(
                        saka_core::rejection::Rejection::VersionStale,
                    )),
                },
                TaskAction::Merge => match version.flatten() {
                    Some(v) => {
                        engine.request_merge_with(item, v, reason.as_deref(), &cause, Some(&action))
                    }
                    None => Err(EngineError::Rejected(
                        saka_core::rejection::Rejection::VersionStale,
                    )),
                },
                TaskAction::Revert => match version.flatten() {
                    Some(v) => engine.revert_with(
                        item,
                        v,
                        reason.as_deref().unwrap_or_default(),
                        &cause,
                        Some(&action),
                    ),
                    None => Err(EngineError::Rejected(
                        saka_core::rejection::Rejection::VersionStale,
                    )),
                },
                TaskAction::Retry => engine.retry_with(
                    item,
                    reason.as_deref().unwrap_or_default(),
                    &cause,
                    Some(&action),
                ),
                TaskAction::Cancel => engine.cancel_with(
                    item,
                    reason.as_deref().unwrap_or_default(),
                    &cause,
                    Some(&action),
                ),
                TaskAction::Takeover => engine.takeover_with(
                    item,
                    reason.as_deref().unwrap_or_default(),
                    &cause,
                    Some(&action),
                ),
                TaskAction::Return => {
                    engine
                        .return_with(
                            item,
                            return_stage,
                            reason.as_deref().unwrap_or_default(),
                            &cause,
                            Some(&action),
                        )
                        .await
                }
                TaskAction::Primary => match blocker {
                    Some(blocker_id) => engine.resolve_blocker_with(
                        item,
                        blocker_id,
                        expected,
                        reason.as_deref().unwrap_or_default(),
                        &cause,
                        Some(&action),
                    ),
                    None => Err(EngineError::Rejected(
                        saka_core::rejection::Rejection::NotFound,
                    )),
                },
            };
            result.map(|_| project).map_err(|e| (e, project))
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/work-items/{work_item_id}/actions/start", tag = "delivery",
    params(("work_item_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters"), ("If-Match" = String, Header, description = "W/\"<version>\"")),
    request_body = dto::OptionalReason,
    responses((status = 202, body = dto::WorkItem), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn start(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    task_action(s, id, method, uri, headers, raw, TaskAction::Start).await
}

#[utoipa::path(
    post, path = "/api/v1/work-items/{work_item_id}/actions/merge", tag = "delivery",
    params(("work_item_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters"), ("If-Match" = String, Header, description = "W/\"<version>\"")),
    request_body = dto::OptionalReason,
    responses((status = 202, body = dto::WorkItem), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn merge(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    task_action(s, id, method, uri, headers, raw, TaskAction::Merge).await
}

#[utoipa::path(
    post, path = "/api/v1/work-items/{work_item_id}/actions/revert", tag = "delivery",
    params(("work_item_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters"), ("If-Match" = String, Header, description = "W/\"<version>\"")),
    request_body = dto::RequiredReason,
    responses((status = 202, body = dto::WorkItem), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn revert(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    task_action(s, id, method, uri, headers, raw, TaskAction::Revert).await
}

#[utoipa::path(
    post, path = "/api/v1/work-items/{work_item_id}/actions/retry", tag = "delivery",
    params(("work_item_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::RequiredReason,
    responses((status = 202, body = dto::WorkItem), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn retry(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    task_action(s, id, method, uri, headers, raw, TaskAction::Retry).await
}

#[utoipa::path(
    post, path = "/api/v1/work-items/{work_item_id}/actions/cancel", tag = "delivery",
    params(("work_item_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::RequiredReason,
    responses((status = 202, body = dto::WorkItem), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn cancel(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    task_action(s, id, method, uri, headers, raw, TaskAction::Cancel).await
}

#[utoipa::path(
    post, path = "/api/v1/work-items/{work_item_id}/actions/takeover", tag = "delivery",
    params(("work_item_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::RequiredReason,
    responses((status = 202, body = dto::WorkItem), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn takeover(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    task_action(s, id, method, uri, headers, raw, TaskAction::Takeover).await
}

#[utoipa::path(
    post, path = "/api/v1/work-items/{work_item_id}/actions/return", tag = "delivery",
    params(("work_item_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::ReturnBody,
    responses((status = 202, body = dto::WorkItem), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"), (status = 502, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn return_to_loop(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    task_action(s, id, method, uri, headers, raw, TaskAction::Return).await
}

#[utoipa::path(
    post, path = "/api/v1/work-items/{work_item_id}/actions/primary", tag = "delivery",
    params(("work_item_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::PrimaryActionBody,
    responses((status = 202, body = dto::WorkItem), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn primary_action(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    task_action(s, id, method, uri, headers, raw, TaskAction::Primary).await
}

#[utoipa::path(
    post, path = "/api/v1/work-items/{work_item_id}/actions/review", tag = "delivery",
    params(("work_item_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::ReviewBody,
    responses((status = 202, body = dto::WorkItem), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"), (status = 502, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn review(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let item = WorkItemId(tri!(path_id(&id)));
    let _: dto::ReviewBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let detail = tri!(engine.work_item(item));
    let project = detail.as_ref().map(|d| d.task.item.project_id);
    let repo_name = detail.and_then(|d| d.repo_name);
    let respond = |p: Produced<'_>| match p {
        Produced::Task(t) => reply(
            StatusCode::ACCEPTED,
            &dto::WorkItem::from_task(t, repo_name.as_deref(), &[], None),
        ),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "work_item.review",
        target_type: "work_item",
        target_id: Some(item.0),
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
                .request_review_with(item, &cause, Some(&action))
                .await
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/work-items/{work_item_id}/actions/fix", tag = "delivery",
    params(("work_item_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::FixBody,
    responses((status = 202, body = dto::WorkItem), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"), (status = 502, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn fix(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let item = WorkItemId(tri!(path_id(&id)));
    let _: dto::FixBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let detail = tri!(engine.work_item(item));
    let project = detail.as_ref().map(|d| d.task.item.project_id);
    let repo_name = detail.and_then(|d| d.repo_name);
    let respond = |p: Produced<'_>| match p {
        Produced::Task(t) => reply(
            StatusCode::ACCEPTED,
            &dto::WorkItem::from_task(t, repo_name.as_deref(), &[], None),
        ),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "work_item.fix",
        target_type: "work_item",
        target_id: Some(item.0),
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
                .request_fix_with(item, &cause, Some(&action))
                .await
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/projects/{project_id}/external-changes", tag = "delivery",
    params(("project_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::ExternalChangeRequest,
    responses(
        (status = 201, body = dto::WorkItem),
        (status = 200, body = dto::WorkItem),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn take_external_change(
    State(s): State<AppState>,
    Path(project): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&project)));
    let req: dto::ExternalChangeRequest = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let known = tri!(engine.project(project)).is_some();
    let respond = |p: Produced<'_>| match p {
        Produced::Taken(t, created) => {
            let repo = t.item.task_key.rsplit_once('#').map(|(name, _)| name);
            reply(
                if created {
                    StatusCode::CREATED
                } else {
                    StatusCode::OK
                },
                &dto::WorkItem::from_task(t, repo, &[], None),
            )
        }
        _ => unexpected(),
    };
    let m = Mutation {
        name: "external_change.take",
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
                .take_change_with(
                    project,
                    req.repo.as_deref(),
                    req.number,
                    &cause,
                    Some(&action),
                )
                .await
                .map(|_| Some(project))
                .map_err(|e| (e, known.then_some(project)))
        },
    )
    .await
}
