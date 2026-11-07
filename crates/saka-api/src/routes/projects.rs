use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use saka_core::ids::ProjectId;
use saka_core::secret::SecretText;
use saka_core::vocab::{AuthorshipMode, ReloadTrigger};
use saka_engine::{AddProject, CheckConfig, Produced};

use super::{no_content, reply, unexpected};
use crate::dto;
use crate::http::{Mutation, body, encode_cursor, mutate, ok, page, path_id, require_key};
use crate::problem::ApiError;
use crate::state::AppState;

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
    post, path = "/api/v1/projects/actions/check-config", tag = "projects",
    params(("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::CheckConfigBody,
    responses(
        (status = 200, body = dto::ConfigCheck),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn check_project_config(
    State(s): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let req: dto::CheckConfigBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::ConfigCheck(v) => reply(StatusCode::OK, &dto::ConfigCheck::from_view(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "project.check_config",
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
            let check = CheckConfig {
                workspace_url: req.workspace_url,
                project_path: req.project_path,
                base_branch: req.base_branch,
                content: req.content,
            };
            engine
                .check_project_config(&check, &cause, Some(&action))
                .await
                .map(|v| v.project.map(|p| p.id))
                .map_err(|e| (e, None))
        },
    )
    .await
}

#[utoipa::path(
    get, path = "/api/v1/projects", tag = "projects", params(dto::ListQuery),
    responses((status = 200, body = dto::Page<dto::Project>), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_projects(State(s): State<AppState>, Query(q): Query<dto::ListQuery>) -> Response {
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

#[utoipa::path(
    post, path = "/api/v1/projects/{project_id}/actions/reload", tag = "projects",
    params(("project_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::ReloadBody,
    responses(
        (status = 200, body = dto::ProjectReload),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn reload_project(
    State(s): State<AppState>,
    Path(project): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&project)));
    let _: dto::ReloadBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let known = tri!(engine.project(project)).is_some();
    let respond = |p: Produced<'_>| match p {
        Produced::Reload(v) => reply(StatusCode::OK, &dto::ProjectReload::from_view(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "project.reload",
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
                .reload_project_config(project, ReloadTrigger::Operator, &cause, Some(&action))
                .await
                .map(|_| Some(project))
                .map_err(|e| (e, known.then_some(project)))
        },
    )
    .await
}

#[utoipa::path(
    put, path = "/api/v1/projects/{project_id}/secrets/{name}", tag = "projects",
    params(("project_id" = uuid::Uuid, Path), ("name" = String, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::SetSecretBody,
    responses(
        (status = 204, description = "The secret is stored"),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn set_secret(
    State(s): State<AppState>,
    Path((project, name)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&project)));
    let req: dto::SetSecretBody = tri!(body(&raw));
    let value = SecretText::new(req.value);
    let engine = tri!(s.engine().cloned());
    // The stored request hash covers a keyed digest of the value, never the value itself.
    let digest = Bytes::from(engine.secret_fingerprint(project, &name, &value));
    let m = Mutation {
        name: "secret.set",
        target_type: "project",
        target_id: Some(project.0),
        reason: None,
    };
    mutate(
        &s,
        key,
        &method,
        uri.path(),
        &digest,
        m,
        &no_content,
        |cause, action| async move {
            engine
                .set_secret(project, &name, &value, &cause, Some(&action))
                .map(|()| Some(project))
                .map_err(|e| (e, None))
        },
    )
    .await
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/secrets", tag = "projects",
    params(("project_id" = uuid::Uuid, Path)),
    responses(
        (status = 200, body = dto::SecretList),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_secrets(State(s): State<AppState>, Path(project): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    let items = tri!(engine.secrets(project));
    ok(
        StatusCode::OK,
        &dto::SecretList {
            items: items.iter().map(dto::SecretName::from).collect(),
        },
    )
}

#[utoipa::path(
    delete, path = "/api/v1/projects/{project_id}/secrets/{name}", tag = "projects",
    params(("project_id" = uuid::Uuid, Path), ("name" = String, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    responses(
        (status = 204, description = "The secret is gone, or never existed"),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn remove_secret(
    State(s): State<AppState>,
    Path((project, name)): Path<(String, String)>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&project)));
    let engine = tri!(s.engine().cloned());
    let m = Mutation {
        name: "secret.remove",
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
        &no_content,
        |cause, action| async move {
            engine
                .remove_secret(project, &name, &cause, Some(&action))
                .map(|()| Some(project))
                .map_err(|e| (e, None))
        },
    )
    .await
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/authorship", tag = "projects",
    params(("project_id" = uuid::Uuid, Path)),
    responses(
        (status = 200, body = dto::ProjectAuthorship),
        (status = 404, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_authorship(State(s): State<AppState>, Path(project): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    let view = tri!(engine.project_authorship(project));
    ok(StatusCode::OK, &dto::ProjectAuthorship::from_view(&view))
}

#[utoipa::path(
    put, path = "/api/v1/projects/{project_id}/authorship", tag = "projects",
    params(("project_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::SetAuthorshipBody,
    responses(
        (status = 200, body = dto::ProjectAuthorship, description = "The mode in force; setting the current mode again records nothing"),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn set_authorship(
    State(s): State<AppState>,
    Path(project): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&project)));
    let req: dto::SetAuthorshipBody = tri!(body(&raw));
    let mode: AuthorshipMode = match req.mode.parse() {
        Ok(mode) => mode,
        Err(_) => return ApiError::validation("mode", "invalid").into_response(),
    };
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Authorship(v) => reply(StatusCode::OK, &dto::ProjectAuthorship::from_view(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "project.authorship.set",
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
                .set_authorship(project, mode, &cause, Some(&action))
                .map(|_| Some(project))
                .map_err(|e| (e, None))
        },
    )
    .await
}

#[utoipa::path(
    put, path = "/api/v1/projects/{project_id}/authorship/user-token", tag = "projects",
    params(("project_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::SetUserTokenBody,
    responses(
        (status = 200, body = dto::ProjectAuthorship, description = "The token is checked and stored; its value is never returned"),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn set_user_token(
    State(s): State<AppState>,
    Path(project): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&project)));
    let req: dto::SetUserTokenBody = tri!(body(&raw));
    let value = SecretText::new(req.value);
    let engine = tri!(s.engine().cloned());
    // The stored request hash covers a keyed digest of the token, never the token itself.
    let digest = Bytes::from(engine.user_token_fingerprint(project, &value));
    let respond = |p: Produced<'_>| match p {
        Produced::Authorship(v) => reply(StatusCode::OK, &dto::ProjectAuthorship::from_view(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "project.user_token.set",
        target_type: "project",
        target_id: Some(project.0),
        reason: None,
    };
    mutate(
        &s,
        key,
        &method,
        uri.path(),
        &digest,
        m,
        &respond,
        |cause, action| async move {
            engine
                .set_user_token(project, &value, &cause, Some(&action))
                .await
                .map(|_| Some(project))
                .map_err(|e| (e, None))
        },
    )
    .await
}

#[utoipa::path(
    delete, path = "/api/v1/projects/{project_id}/authorship/user-token", tag = "projects",
    params(("project_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    responses(
        (status = 204, description = "The token is gone, or there was none"),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn remove_user_token(
    State(s): State<AppState>,
    Path(project): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&project)));
    let engine = tri!(s.engine().cloned());
    let m = Mutation {
        name: "project.user_token.remove",
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
        &no_content,
        |cause, action| async move {
            engine
                .remove_user_token(project, &cause, Some(&action))
                .map(|()| Some(project))
                .map_err(|e| (e, None))
        },
    )
    .await
}
