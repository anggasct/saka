use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use saka_core::ids::{IncidentId, ProjectId};
use saka_engine::IncidentQuery;

use crate::dto;
use crate::http::{encode_cursor, ok, page, path_id};
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
