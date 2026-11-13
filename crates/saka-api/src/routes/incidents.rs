use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use saka_core::ids::{IncidentId, ProjectId, WorkItemId};
use saka_engine::{IncidentQuery, Produced};

use super::{reply, unexpected};
use crate::dto;
use crate::http::{Mutation, body, encode_cursor, mutate, ok, page, path_id, require_key};
use crate::problem::ApiError;
use crate::state::AppState;

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/incident-sources", tag = "incidents",
    params(("project_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::IncidentSourceList), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_incident_sources(
    State(s): State<AppState>,
    Path(project): Path<String>,
) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    let views = tri!(engine.incident_sources(project));
    ok(
        StatusCode::OK,
        &dto::IncidentSourceList {
            items: views.iter().map(dto::IncidentSource::from).collect(),
        },
    )
}

fn parsed<T: std::str::FromStr>(
    field: &'static str,
    value: &Option<String>,
) -> Result<Option<T>, ApiError> {
    match value.as_deref().filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => v
            .parse()
            .map(Some)
            .map_err(|_| ApiError::validation(field, "invalid")),
    }
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/incidents", tag = "incidents",
    params(("project_id" = uuid::Uuid, Path), dto::IncidentQuery),
    responses((status = 200, body = dto::Page<dto::Incident>), (status = 404, body = dto::Problem, content_type = "application/problem+json"), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_incidents(
    State(s): State<AppState>,
    Path(project): Path<String>,
    Query(q): Query<dto::IncidentQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    let query = IncidentQuery {
        state: tri!(parsed("state", &q.state)),
        severity: tri!(parsed("severity", &q.severity)),
        environment: q.environment.clone().filter(|e| !e.is_empty()),
        source: q.source.clone().filter(|e| !e.is_empty()),
    };
    let key = format!(
        "incidents:{project}:{:?}:{:?}:{:?}:{:?}",
        query.state, query.severity, query.environment, query.source
    );
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let paged = tri!(engine.incidents(project, query, &p));
    ok(
        StatusCode::OK,
        &dto::Page {
            items: paged.items.iter().map(dto::Incident::from).collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, &key)),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/incidents/{incident_id}", tag = "incidents",
    params(("incident_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::IncidentDetail), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn get_incident(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = IncidentId(tri!(path_id(&id)));
    let detail = tri!(engine.incident(id));
    ok(StatusCode::OK, &dto::IncidentDetail::from(&detail))
}

#[utoipa::path(
    post, path = "/api/v1/incidents/{incident_id}/actions/draft-fix", tag = "incidents",
    params(("incident_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::DraftFixBody,
    responses((status = 202, body = dto::SpecDraft), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn draft_incident_fix(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let id = IncidentId(tri!(path_id(&id)));
    let req: dto::DraftFixBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let viewer = engine.clone();
    let respond = move |p: Produced<'_>| match p {
        Produced::Draft(d) => reply(
            StatusCode::ACCEPTED,
            &dto::SpecDraft::from_view(&viewer.draft_view(d.clone(), Vec::new(), None)),
        ),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "incident.draft_fix",
        target_type: "incident",
        target_id: Some(id.0),
        reason: req.note.clone(),
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
            let project = engine.incident(id).ok().map(|d| d.row.incident.project_id);
            engine
                .incident_draft_fix_with(id, req.note.as_deref(), &cause, Some(&action))
                .await
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/incidents/{incident_id}/actions/revert", tag = "incidents",
    params(("incident_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::IncidentRevertBody,
    responses((status = 202, body = dto::WorkItem), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn revert_incident(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let id = IncidentId(tri!(path_id(&id)));
    let req: dto::IncidentRevertBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let work_item = WorkItemId(req.work_item_id);
    let repo_name = engine
        .work_item(work_item)
        .ok()
        .flatten()
        .and_then(|d| d.repo_name);
    let respond = |p: Produced<'_>| match p {
        Produced::Task(t) => reply(
            StatusCode::ACCEPTED,
            &dto::WorkItem::from_task(t, repo_name.as_deref(), &[], None),
        ),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "incident.revert",
        target_type: "incident",
        target_id: Some(id.0),
        reason: Some(req.reason.clone()),
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
            let project = engine.incident(id).ok().map(|d| d.row.incident.project_id);
            engine
                .incident_revert_with(id, work_item, &req.reason, &cause, Some(&action))
                .map(|()| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/incidents/{incident_id}/actions/dismiss", tag = "incidents",
    params(("incident_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::RequiredReason,
    responses((status = 200, body = dto::Incident), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn dismiss_incident(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let id = IncidentId(tri!(path_id(&id)));
    let req: dto::RequiredReason = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Incident(row) => reply(StatusCode::OK, &dto::Incident::from(row)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "incident.dismiss",
        target_type: "incident",
        target_id: Some(id.0),
        reason: Some(req.reason.clone()),
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
            let project = engine.incident(id).ok().map(|d| d.row.incident.project_id);
            engine
                .incident_dismiss_with(id, &req.reason, &cause, Some(&action))
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}
