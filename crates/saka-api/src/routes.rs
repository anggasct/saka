//! `/api/v1` handlers.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use saka_core::ids::{
    PermissionRequestId, ProjectId, SessionId, SpecId, SpecRevisionId, WorkItemId,
};
use saka_engine::{
    AddProject, EngineError, PermissionFilter, Produced, Reply, StoredEvent, WorkItemFilter,
};
use serde::Deserialize;
use serde_json::Value;
use utoipa::IntoParams;

use crate::dto::{self, CAPABILITIES};
use crate::http::{
    Mutation, body, encode_cursor, if_match, mutate, ok, page, path_id, require_key, states,
};
use crate::problem::ApiError;
use crate::state::AppState;

const SPEC_STATES: &[&str] = &["draft", "ready", "approved", "shipped", "withdrawn"];
const WORK_ITEM_STATES: &[&str] = &[
    "queued",
    "awaiting_start",
    "admitted",
    "in_stage",
    "blocked",
    "taken_over",
    "done",
    "cancelled",
];
const PERMISSION_STATES: &[&str] = &["pending", "allowed", "denied", "expired"];
const EVENT_BATCH: u32 = 100;
const EVENT_POLL: Duration = Duration::from_millis(500);

fn reply<T: serde::Serialize>(status: StatusCode, value: &T) -> Reply {
    Reply {
        status: status.as_u16(),
        body: serde_json::to_value(value).unwrap_or(Value::Null),
    }
}

fn unexpected() -> Reply {
    Reply {
        status: 500,
        body: ApiError::internal().body(),
    }
}

macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return ApiError::from(e).into_response(),
        }
    };
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

// Projects.

#[utoipa::path(
    post, path = "/api/v1/projects", tag = "projects",
    params(("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::AddProjectBody,
    responses(
        (status = 201, body = dto::Project),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn add_project(
    State(s): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let req: dto::AddProjectBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Project(v) => reply(StatusCode::CREATED, &dto::Project::from_view(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "project.register",
        target_type: "project",
        target_id: None,
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
            let add = AddProject {
                workspace_url: req.workspace_url,
                project_path: req.project_path,
                base_branch: req.base_branch,
            };
            engine
                .add_project(&add, &cause, Some(&action))
                .await
                .map(|v| Some(v.project.id))
                .map_err(|e| (e, None))
        },
    )
    .await
}

#[utoipa::path(
    get, path = "/api/v1/projects", tag = "projects", params(ListQuery),
    responses((status = 200, body = dto::Page<dto::Project>), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_projects(State(s): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    let engine = tri!(s.engine());
    let p = tri!(page(q.cursor.as_deref(), q.limit, "projects"));
    let paged = tri!(engine.projects(&p));
    ok(
        StatusCode::OK,
        &dto::Page {
            items: paged.items.iter().map(dto::Project::from_view).collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, "projects")),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}", tag = "projects",
    params(("project_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::Project), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn get_project(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = tri!(path_id(&id));
    match tri!(engine.project(ProjectId(id))) {
        Some(v) => ok(StatusCode::OK, &dto::Project::from_view(&v)),
        None => ApiError::not_found().into_response(),
    }
}

// Specs.

#[utoipa::path(
    post, path = "/api/v1/projects/{project_id}/specs", tag = "specs",
    params(("project_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::AddSpecBody,
    responses(
        (status = 201, body = dto::Spec, description = "A new spec"),
        (status = 200, body = dto::Spec, description = "A spec already registered"),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn add_spec(
    State(s): State<AppState>,
    Path(project): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&project)));
    let req: dto::AddSpecBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Spec(v, created) => reply(
            if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            &dto::Spec::from_view(v, true),
        ),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "spec.register",
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
                .add_spec(project, &req.path, &cause, Some(&action))
                .await
                .map(|_| Some(project))
                .map_err(|e| (e, Some(project)))
        },
    )
    .await
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SpecListQuery {
    /// One or more of `draft, ready, approved, shipped, withdrawn`, comma-separated.
    pub state: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/specs", tag = "specs",
    params(("project_id" = uuid::Uuid, Path), SpecListQuery),
    responses((status = 200, body = dto::Page<dto::Spec>), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_specs(
    State(s): State<AppState>,
    Path(project): Path<String>,
    Query(q): Query<SpecListQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    if tri!(engine.project(project)).is_none() {
        return ApiError::not_found().into_response();
    }
    let wanted = tri!(states(q.state.as_deref(), SPEC_STATES));
    let filter = format!("specs:{project}:{}", wanted.join(","));
    let p = tri!(page(q.cursor.as_deref(), q.limit, &filter));
    let paged = tri!(engine.specs(project, &wanted, &p));
    ok(
        StatusCode::OK,
        &dto::Page {
            items: paged
                .items
                .iter()
                .map(|v| dto::Spec::from_view(v, false))
                .collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, &filter)),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/specs/{spec_id}", tag = "specs",
    params(("spec_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::Spec), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn get_spec(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = tri!(path_id(&id));
    match tri!(engine.spec(SpecId(id))) {
        Some(v) => ok(StatusCode::OK, &dto::Spec::from_view(&v, true)),
        None => ApiError::not_found().into_response(),
    }
}

#[utoipa::path(
    post, path = "/api/v1/specs/{spec_id}/approve", tag = "specs",
    params(("spec_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters"), ("If-Match" = String, Header, description = "\"<revision id>\"")),
    request_body = dto::OptionalReason,
    responses(
        (status = 200, body = dto::Spec),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn approve_spec(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let spec = SpecId(tri!(path_id(&id)));
    let tag = tri!(if_match(&headers));
    let req: dto::OptionalReason = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let project = tri!(engine.spec(spec)).map(|v| v.spec.project_id);
    let respond = |p: Produced<'_>| match p {
        Produced::Spec(v, _) => reply(StatusCode::OK, &dto::Spec::from_view(v, true)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "spec.approve",
        target_type: "spec",
        target_id: Some(spec.0),
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
            // A tag that is not a revision id can never match.
            let revision = tag
                .trim_matches('"')
                .parse()
                .map(SpecRevisionId)
                .map_err(|_| {
                    (
                        EngineError::Rejected(saka_core::rejection::Rejection::RevisionStale),
                        project,
                    )
                })?;
            engine
                .approve_spec_with(spec, revision, req.reason, &cause, Some(&action))
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}

// Work items.

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WorkItemQuery {
    pub project_id: Option<String>,
    pub spec_id: Option<String>,
    /// One or more states, comma-separated.
    pub state: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

fn opt_id(raw: Option<&str>, field: &str) -> Result<Option<uuid::Uuid>, ApiError> {
    match raw.filter(|r| !r.is_empty()) {
        None => Ok(None),
        Some(r) => r
            .parse()
            .map(Some)
            .map_err(|_| ApiError::validation(field, "invalid")),
    }
}

#[utoipa::path(
    get, path = "/api/v1/work-items", tag = "delivery", params(WorkItemQuery),
    responses((status = 200, body = dto::Page<dto::WorkItem>), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_work_items(
    State(s): State<AppState>,
    Query(q): Query<WorkItemQuery>,
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
            items: paged.items.iter().map(dto::WorkItem::from_task).collect(),
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

#[derive(Clone, Copy)]
enum TaskAction {
    Merge,
    Retry,
    Cancel,
}

#[allow(clippy::too_many_arguments, reason = "the request parts of one action")]
async fn task_action(
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
    let (version, reason) = match which {
        TaskAction::Merge => {
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
        TaskAction::Retry | TaskAction::Cancel => {
            let req: dto::RequiredReason = tri!(body(&raw));
            (None, Some(req.reason))
        }
    };
    let project = tri!(engine.work_item(item)).map(|d| d.task.item.project_id);
    let respond = |p: Produced<'_>| match p {
        Produced::Task(t) => reply(StatusCode::ACCEPTED, &dto::WorkItem::from_task(t)),
        _ => unexpected(),
    };
    let name = match which {
        TaskAction::Merge => "work_item.merge",
        TaskAction::Retry => "work_item.retry",
        TaskAction::Cancel => "work_item.cancel",
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
                TaskAction::Merge => match version.flatten() {
                    Some(v) => {
                        engine.request_merge_with(item, v, reason.as_deref(), &cause, Some(&action))
                    }
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
            };
            result.map(|_| project).map_err(|e| (e, project))
        },
    )
    .await
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

// Runtime.

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PermissionQuery {
    /// One or more of `pending, allowed, denied, expired`, comma-separated.
    pub state: Option<String>,
    pub project_id: Option<String>,
    pub work_item_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[utoipa::path(
    get, path = "/api/v1/permission-requests", tag = "runtime", params(PermissionQuery),
    responses((status = 200, body = dto::Page<dto::PermissionRequest>), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_permissions(
    State(s): State<AppState>,
    Query(q): Query<PermissionQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let filter = PermissionFilter {
        states: tri!(states(q.state.as_deref(), PERMISSION_STATES)),
        project: tri!(opt_id(q.project_id.as_deref(), "project_id")).map(ProjectId),
        work_item: tri!(opt_id(q.work_item_id.as_deref(), "work_item_id")).map(WorkItemId),
    };
    let key = format!(
        "permissions:{}:{:?}:{:?}",
        filter.states.join(","),
        filter.project,
        filter.work_item
    );
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let paged = tri!(engine.permission_requests(&filter, &p));
    ok(
        StatusCode::OK,
        &dto::Page {
            items: paged
                .items
                .iter()
                .map(dto::PermissionRequest::from_view)
                .collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, &key)),
        },
    )
}

#[utoipa::path(
    post, path = "/api/v1/permission-requests/{permission_request_id}/answer", tag = "runtime",
    params(("permission_request_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::AnswerBody,
    responses((status = 200, body = dto::PermissionRequest), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn answer_permission(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let request = PermissionRequestId(tri!(path_id(&id)));
    let req: dto::AnswerBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let project = tri!(engine.permission(request)).map(|v| v.project_id);
    let respond = |p: Produced<'_>| match p {
        Produced::Permission(v) => reply(StatusCode::OK, &dto::PermissionRequest::from_view(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "permission.answer",
        target_type: "permission_request",
        target_id: Some(request.0),
        reason: req.message.clone(),
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
                .answer_permission_with(
                    request,
                    req.decision == dto::Decision::Allow,
                    req.message,
                    &cause,
                    Some(&action),
                )
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}

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

// System.

#[utoipa::path(
    get, path = "/api/v1/meta", tag = "system",
    responses((status = 200, body = dto::Meta))
)]
pub async fn meta(State(s): State<AppState>) -> Response {
    ok(
        StatusCode::OK,
        &dto::Meta {
            server_version: s.config().server_version.clone(),
            api_version: "1".into(),
            min_client_api_version: "1".into(),
            capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
        },
    )
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventsQuery {
    /// Start after this `seq` (`Last-Event-ID` wins when both are given).
    pub after: Option<String>,
}

fn sse_event(e: &StoredEvent) -> Event {
    let data = dto::StreamEvent {
        seq: e.seq,
        kind: e.kind.clone(),
        version: e.version,
        stream_type: e.stream_type.clone(),
        stream_id: e.stream_id,
        project_id: e.project_id.map(|p| p.0),
        occurred_at: dto::time(e.occurred_at),
        payload: e.payload.clone(),
    };
    Event::default()
        .id(e.seq.to_string())
        .event(format!("{}.v{}", e.kind, e.version))
        .data(serde_json::to_string(&data).unwrap_or_default())
}

#[utoipa::path(
    get, path = "/api/v1/events", tag = "system",
    params(EventsQuery, ("Last-Event-ID" = Option<String>, Header, description = "Resume after this seq")),
    responses(
        (status = 200, content_type = "text/event-stream", body = dto::StreamEvent, description = "One `data` per event, `id` = seq"),
        (status = 422, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn events(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> Response {
    let engine = tri!(s.engine().cloned());
    let from_header = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let after = match from_header.or(q.after) {
        Some(raw) => tri!(
            raw.trim()
                .parse::<i64>()
                .map_err(|_| ApiError::validation("after", "not_a_number"))
        ),
        None => tri!(engine.last_seq()),
    };
    let stream = stream::unfold(
        (engine, after, VecDeque::<StoredEvent>::new()),
        |(engine, mut cursor, mut buffer)| async move {
            loop {
                if let Some(e) = buffer.pop_front() {
                    let event = sse_event(&e);
                    return Some((Ok::<Event, Infallible>(event), (engine, cursor, buffer)));
                }
                match engine.events_after(cursor, EVENT_BATCH) {
                    Ok(list) if !list.is_empty() => {
                        cursor = list.last().map_or(cursor, |e| e.seq);
                        buffer.extend(list);
                        continue;
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "reading events failed"),
                }
                tokio::select! {
                    () = engine.changed() => {}
                    () = tokio::time::sleep(EVENT_POLL) => {}
                }
            }
        },
    );
    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(s.config().keep_alive)
                .text("keep-alive"),
        )
        .into_response()
}

/// Unknown `/api/*` paths.
pub async fn api_not_found() -> Response {
    ApiError::not_found().into_response()
}
