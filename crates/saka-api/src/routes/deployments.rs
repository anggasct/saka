use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use saka_core::ids::{DeploymentId, ProjectId};

use crate::dto;
use crate::http::{encode_cursor, ok, page, path_id};
use crate::problem::ApiError;
use crate::state::AppState;

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/environments", tag = "deployments",
    params(("project_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::EnvironmentList), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_environments(State(s): State<AppState>, Path(project): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    let views = tri!(engine.environments(project));
    ok(
        StatusCode::OK,
        &dto::EnvironmentList {
            items: views.iter().map(dto::DeployEnvironment::from).collect(),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/deployments", tag = "deployments",
    params(("project_id" = uuid::Uuid, Path), dto::DeploymentQuery),
    responses((status = 200, body = dto::Page<dto::Deployment>), (status = 404, body = dto::Problem, content_type = "application/problem+json"), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_deployments(
    State(s): State<AppState>,
    Path(project): Path<String>,
    Query(q): Query<dto::DeploymentQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    let state = match q.state.as_deref().filter(|v| !v.is_empty()) {
        None => None,
        Some(v) => Some(tri!(
            v.parse()
                .map_err(|_| ApiError::validation("state", "invalid"))
        )),
    };
    let environment = q.environment.clone().filter(|e| !e.is_empty());
    let key = format!("deployments:{project}:{environment:?}:{state:?}");
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let paged = tri!(engine.deployments(project, environment, state, &p));
    ok(
        StatusCode::OK,
        &dto::Page {
            items: paged.items.iter().map(dto::Deployment::from).collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, &key)),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/deployments/{deployment_id}", tag = "deployments",
    params(("deployment_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::Deployment), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn get_deployment(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = DeploymentId(tri!(path_id(&id)));
    let detail = tri!(engine.deployment(id));
    ok(StatusCode::OK, &dto::Deployment::from(&detail))
}
