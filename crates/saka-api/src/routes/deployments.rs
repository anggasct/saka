use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use saka_core::ids::{DeploymentId, ProjectId};
use saka_engine::Produced;

use super::{reply, unexpected};
use crate::dto;
use crate::http::{Mutation, body, encode_cursor, mutate, ok, page, path_id, require_key};
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

#[utoipa::path(
    post, path = "/api/v1/projects/{project_id}/environments/{environment}/actions/deploy", tag = "deployments",
    params(("project_id" = uuid::Uuid, Path), ("environment" = String, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::RequiredReason,
    responses((status = 202, body = dto::Deployment), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn deploy_environment(
    State(s): State<AppState>,
    Path((project, environment)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&project)));
    let req: dto::RequiredReason = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Deployment(row) => reply(StatusCode::ACCEPTED, &dto::Deployment::from(row)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "deployment.deploy",
        target_type: "project",
        target_id: Some(project.0),
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
            engine
                .deploy_environment_with(project, &environment, &req.reason, &cause, Some(&action))
                .map(|_| Some(project))
                .map_err(|e| (e, Some(project)))
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/deployments/{deployment_id}/actions/cancel", tag = "deployments",
    params(("deployment_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::RequiredReason,
    responses((status = 200, body = dto::Deployment), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn cancel_deployment(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let id = DeploymentId(tri!(path_id(&id)));
    let req: dto::RequiredReason = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Deployment(row) => reply(StatusCode::OK, &dto::Deployment::from(row)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "deployment.cancel",
        target_type: "deployment",
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
            let project = engine
                .deployment(id)
                .ok()
                .map(|d| d.row.deployment.project_id);
            engine
                .cancel_deployment_with(id, &req.reason, &cause, Some(&action))
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}

#[utoipa::path(
    post, path = "/api/v1/deployments/{deployment_id}/actions/draft-fix", tag = "deployments",
    params(("deployment_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::DraftFixBody,
    responses((status = 202, body = dto::SpecDraft), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn draft_fix(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let id = DeploymentId(tri!(path_id(&id)));
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
        name: "deployment.draft_fix",
        target_type: "deployment",
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
            let project = engine
                .deployment(id)
                .ok()
                .map(|d| d.row.deployment.project_id);
            engine
                .draft_fix_with(id, req.note.as_deref(), &cause, Some(&action))
                .await
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
}
