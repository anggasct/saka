use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use saka_core::diagnostics::{self, CheckResult, Report, Subject};
use saka_core::ids::{
    BlockerId, HookRunId, PermissionRequestId, PermissionRuleId, ProjectId, SessionId, SpecDraftId,
    SpecId, SpecRevisionId, WorkItemId,
};
use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_core::vocab::{AuthorshipMode, CheckId, DraftLocale, ReloadTrigger, ReportMode};
use saka_engine::{
    AddProject, CheckConfig, DiagnosticFilter, EngineError, HookRunFilter, NewSpecDraft,
    PermissionFilter, Precondition, Produced, Reply, RunOf, SessionCommand, SessionView,
    StoredEvent, TimelineCursor, WorkItemFilter,
};
use serde_json::Value;

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
    "superseded",
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

fn no_content(p: Produced<'_>) -> Reply {
    match p {
        Produced::Nothing => Reply {
            status: StatusCode::NO_CONTENT.as_u16(),
            body: Value::Null,
        },
        _ => unexpected(),
    }
}

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

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/specs", tag = "specs",
    params(("project_id" = uuid::Uuid, Path), dto::SpecListQuery),
    responses((status = 200, body = dto::Page<dto::Spec>), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_specs(
    State(s): State<AppState>,
    Path(project): Path<String>,
    Query(q): Query<dto::SpecListQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    if tri!(engine.project(project)).is_none() {
        return ApiError::not_found().into_response();
    }
    let wanted = tri!(states(q.state.as_deref(), SPEC_STATES));
    if q.path
        .as_ref()
        .is_some_and(|p| p.len() > saka_core::paths::MAX_PATH_BYTES)
    {
        return ApiError::validation("path", "too_long").into_response();
    }
    let filter = format!(
        "specs:{project}:{}:{}",
        wanted.join(","),
        q.path.as_deref().unwrap_or_default()
    );
    let p = tri!(page(q.cursor.as_deref(), q.limit, &filter));
    let paged = tri!(engine.specs(project, &wanted, q.path.as_deref(), &p));
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
    get, path = "/api/v1/projects/{project_id}/files", tag = "files",
    params(("project_id" = uuid::Uuid, Path)),
    responses(
        (status = 200, body = dto::FileTree),
        (status = 404, body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_files(State(s): State<AppState>, Path(project): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    let tree = tri!(engine.doc_tree(project).await);
    ok(StatusCode::OK, &dto::FileTree::from_view(&tree))
}

fn file_path(q: &dto::FileQuery) -> Result<&str, ApiError> {
    let path = q
        .path
        .as_deref()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| ApiError::validation("path", "required"))?;
    // Checked against the project folder once the project is known.
    match saka_core::paths::check_doc_path(".", path) {
        Ok(()) => Ok(path),
        Err(saka_core::paths::PathIssue::NotMarkdown) => {
            Err(ApiError::validation("path", "not_markdown"))
        }
        Err(_) => Err(ApiError::validation("path", "invalid")),
    }
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/file", tag = "files",
    params(("project_id" = uuid::Uuid, Path), dto::FileQuery),
    responses(
        (status = 200, body = dto::FileContent, headers(("ETag" = String, description = "\"<blob>\""))),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_file(
    State(s): State<AppState>,
    Path(project): Path<String>,
    Query(q): Query<dto::FileQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let project = ProjectId(tri!(path_id(&project)));
    let path = tri!(file_path(&q));
    let file = tri!(engine.read_doc(project, path).await);
    ok(StatusCode::OK, &dto::FileContent::from_view(&file))
}

fn precondition(headers: &HeaderMap) -> Result<Precondition, ApiError> {
    let text = |name| {
        headers
            .get(name)
            .map(|v| v.to_str().map(str::trim).map_err(|_| ()))
    };
    let if_match = text(axum::http::header::IF_MATCH);
    let if_none = text(axum::http::header::IF_NONE_MATCH);
    match (if_match, if_none) {
        (Some(_), Some(_)) => Err(ApiError::validation("If-None-Match", "exclusive")),
        (None, None) => Err(ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            "precondition_required",
            serde_json::json!({}),
        )),
        (None, Some(Ok("*"))) => Ok(Precondition::Absent),
        (None, Some(_)) => Err(ApiError::validation("If-None-Match", "invalid")),
        (Some(Ok("*")), None) => Ok(Precondition::Exists),
        (Some(Ok(tag)), None) => {
            // A weak tag never matches under the strong comparison If-Match uses.
            let weak = tag.strip_prefix("W/").is_some();
            let tag = tag.strip_prefix("W/").unwrap_or(tag);
            match tag.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
                Some(blob) if !weak => Ok(Precondition::Blob(blob.to_string())),
                Some(_) => Ok(Precondition::Blob(String::new())),
                None => Err(ApiError::validation("If-Match", "invalid")),
            }
        }
        (Some(Err(())), None) => Err(ApiError::validation("If-Match", "invalid")),
    }
}

#[utoipa::path(
    put, path = "/api/v1/projects/{project_id}/file", tag = "files",
    params(
        ("project_id" = uuid::Uuid, Path), dto::FileQuery,
        ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters"),
        ("If-Match" = Option<String>, Header, description = "\"<blob>\" of the file being replaced, or * for any existing file"),
        ("If-None-Match" = Option<String>, Header, description = "* to create a file that must not exist yet"),
    ),
    request_body = dto::WriteFileBody,
    responses(
        (status = 201, body = dto::FileWrite, description = "A new file", headers(("ETag" = String, description = "\"<blob>\""))),
        (status = 200, body = dto::FileWrite, description = "An existing file, changed or already equal", headers(("ETag" = String, description = "\"<blob>\""))),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn put_file(
    State(s): State<AppState>,
    Path(project): Path<String>,
    Query(q): Query<dto::FileQuery>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&project)));
    let path = tri!(file_path(&q)).to_string();
    let req: dto::WriteFileBody = tri!(body(&raw));
    let pre = tri!(precondition(&headers));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Doc(v) => reply(
            if v.created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            &dto::FileWrite::from_view(v),
        ),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "doc.write",
        target_type: "project",
        target_id: Some(project.0),
        reason: None,
    };
    // The file path lives in the query, so the request hash covers it too.
    let keyed = format!("{}?{}", uri.path(), uri.query().unwrap_or_default());
    mutate(
        &s,
        key,
        &method,
        &keyed,
        &raw,
        m,
        &respond,
        |cause, action| async move {
            engine
                .write_doc(project, &path, req.content, pre, &cause, Some(&action))
                .await
                .map(|_| Some(project))
                .map_err(|e| (e, Some(project)))
        },
    )
    .await
}

#[utoipa::path(
    get, path = "/api/v1/spec-template", tag = "specs", params(dto::TemplateQuery),
    responses((status = 200, body = dto::SpecTemplate), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn spec_template(Query(q): Query<dto::TemplateQuery>) -> Response {
    let code = q.locale.as_deref().unwrap_or("en");
    let Some(locale) = saka_spec::Locale::from_code(code) else {
        return ApiError::validation("locale", "invalid").into_response();
    };
    ok(
        StatusCode::OK,
        &dto::SpecTemplate {
            locale: locale.as_str().to_string(),
            parser_version: saka_spec::PARSER_VERSION,
            content: saka_spec::template(locale).to_string(),
        },
    )
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

#[utoipa::path(
    post, path = "/api/v1/specs/{spec_id}/withdraw", tag = "specs",
    params(("spec_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::RequiredReason,
    responses(
        (status = 200, body = dto::Spec),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn withdraw_spec(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let spec = SpecId(tri!(path_id(&id)));
    let req: dto::RequiredReason = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let project = tri!(engine.spec(spec)).map(|v| v.spec.project_id);
    let respond = |p: Produced<'_>| match p {
        Produced::Spec(v, _) => reply(StatusCode::OK, &dto::Spec::from_view(v, true)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "spec.withdraw",
        target_type: "spec",
        target_id: Some(spec.0),
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
                .withdraw_spec_with(spec, &req.reason, &cause, Some(&action))
                .map(|_| project)
                .map_err(|e| (e, project))
        },
    )
    .await
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
enum TaskAction {
    Merge,
    Retry,
    Cancel,
    Primary,
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
    let mut blocker = None;
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
        TaskAction::Primary => {
            let req: dto::PrimaryActionBody = tri!(body(&raw));
            blocker = Some(BlockerId(req.blocker_id));
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
        TaskAction::Merge => "work_item.merge",
        TaskAction::Retry => "work_item.retry",
        TaskAction::Cancel => "work_item.cancel",
        TaskAction::Primary => "work_item.primary_action",
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
                TaskAction::Primary => match blocker {
                    Some(blocker_id) => engine.resolve_blocker_with(
                        item,
                        blocker_id,
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
    get, path = "/api/v1/permission-requests", tag = "runtime", params(dto::PermissionQuery),
    responses((status = 200, body = dto::Page<dto::PermissionRequest>), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_permissions(
    State(s): State<AppState>,
    Query(q): Query<dto::PermissionQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let run = tri!(run_of(
        q.work_item_id.as_deref(),
        q.spec_draft_id.as_deref()
    ));
    let filter = PermissionFilter {
        states: tri!(states(q.state.as_deref(), PERMISSION_STATES)),
        project: tri!(opt_id(q.project_id.as_deref(), "project_id")).map(ProjectId),
        work_item: match run {
            Some(RunOf::WorkItem(id)) => Some(id),
            _ => None,
        },
        spec_draft: match run {
            Some(RunOf::SpecDraft(id)) => Some(id),
            _ => None,
        },
    };
    let key = format!(
        "permissions:{}:{:?}:{run:?}",
        filter.states.join(","),
        filter.project,
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
                    req.remember,
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

#[utoipa::path(
    get, path = "/api/v1/sessions", tag = "runtime", params(dto::SessionsQuery),
    responses((status = 200, body = dto::SessionList), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_sessions(
    State(s): State<AppState>,
    Query(q): Query<dto::SessionsQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let Some(run) = tri!(run_of(
        q.work_item_id.as_deref(),
        q.spec_draft_id.as_deref()
    )) else {
        return ApiError::validation("work_item_id", "required").into_response();
    };
    let sessions = tri!(engine.sessions_of(run));
    ok(
        StatusCode::OK,
        &dto::SessionList {
            items: sessions.iter().map(dto::Session::from_view).collect(),
        },
    )
}

fn run_of(work_item: Option<&str>, spec_draft: Option<&str>) -> Result<Option<RunOf>, ApiError> {
    let work_item = opt_id(work_item, "work_item_id")?;
    let spec_draft = opt_id(spec_draft, "spec_draft_id")?;
    match (work_item, spec_draft) {
        (Some(_), Some(_)) => Err(ApiError::validation("spec_draft_id", "exclusive")),
        (Some(id), None) => Ok(Some(RunOf::WorkItem(WorkItemId(id)))),
        (None, Some(id)) => Ok(Some(RunOf::SpecDraft(SpecDraftId(id)))),
        (None, None) => Ok(None),
    }
}

const TIMELINE_DEFAULT: u32 = 200;
const TIMELINE_MAX: u32 = 500;

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

enum SessionAction {
    Steer,
    Interrupt,
    Stop,
}

async fn session_action(
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
    let project = tri!(engine.session(session)).map(|v| v.project_id);
    let respond = |p: Produced<'_>| match (p, project) {
        (Produced::Session(session), Some(project_id)) => reply(
            StatusCode::ACCEPTED,
            &dto::Session::from_view(&SessionView {
                session: session.clone(),
                project_id,
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

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/permission-rules", tag = "runtime",
    params(("project_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::PermissionRuleList), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_permission_rules(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = ProjectId(tri!(path_id(&id)));
    match tri!(engine.permission_rules(id)) {
        Some(rules) => ok(
            StatusCode::OK,
            &dto::PermissionRuleList {
                items: rules.iter().map(dto::PermissionRule::from).collect(),
            },
        ),
        None => ApiError::not_found().into_response(),
    }
}

#[utoipa::path(
    post, path = "/api/v1/permission-rules/{rule_id}/revoke", tag = "runtime",
    params(("rule_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::RequiredReason,
    responses((status = 200, body = dto::PermissionRule), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn revoke_rule(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let rule = PermissionRuleId(tri!(path_id(&id)));
    let req: dto::RequiredReason = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Rule(r) => reply(StatusCode::OK, &dto::PermissionRule::from(r)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "permission_rule.revoke",
        target_type: "permission_rule",
        target_id: Some(rule.0),
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
                .revoke_rule(rule, &req.reason, &cause, Some(&action))
                .map(|r| Some(r.project_id))
                .map_err(|e| (e, None))
        },
    )
    .await
}

#[utoipa::path(
    get, path = "/api/v1/agent-accounts", tag = "runtime",
    responses((status = 200, body = dto::AgentAccountList))
)]
pub async fn list_agent_accounts(State(s): State<AppState>) -> Response {
    let engine = tri!(s.engine());
    let accounts = tri!(engine.agent_accounts());
    ok(
        StatusCode::OK,
        &dto::AgentAccountList {
            items: accounts.iter().map(dto::AgentAccount::from).collect(),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/usage-records", tag = "runtime", params(dto::UsageQuery),
    responses((status = 200, body = dto::Page<dto::UsageRecord>), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_usage_records(
    State(s): State<AppState>,
    Query(q): Query<dto::UsageQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let run = tri!(run_of(
        q.work_item_id.as_deref(),
        q.spec_draft_id.as_deref()
    ));
    let session = tri!(opt_id(q.session_id.as_deref(), "session_id")).map(SessionId);
    if run.is_none() && session.is_none() {
        return ApiError::validation("work_item_id", "required").into_response();
    }
    let key = format!("usage:{run:?}:{session:?}");
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let rows = tri!(engine.usage_records(run, session));
    let mut items: Vec<_> = rows
        .iter()
        .filter(|u| {
            p.after
                .is_none_or(|a| (u.record.ended_at.0, u.record.id.0) > a)
        })
        .collect();
    let limit = usize::try_from(p.limit).unwrap_or(usize::MAX);
    let more = items.len() > limit;
    items.truncate(limit);
    ok(
        StatusCode::OK,
        &dto::Page {
            next_cursor: if more {
                items
                    .last()
                    .map(|u| encode_cursor((u.record.ended_at.0, u.record.id.0), &key))
            } else {
                None
            },
            items: items.into_iter().map(dto::UsageRecord::from).collect(),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/meta", tag = "system",
    responses((status = 200, body = dto::Meta))
)]
pub async fn meta(State(s): State<AppState>) -> Response {
    ok(
        StatusCode::OK,
        &dto::Meta {
            server_version: s.config().server_version.clone(),
            api_version: dto::API_VERSION.into(),
            min_client_api_version: dto::API_VERSION.into(),
            capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
        },
    )
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

fn activity_event(a: &saka_core::model::SessionActivity) -> Event {
    let data = dto::ActivityNotice {
        session_id: a.session_id.0,
        owner: dto::RunOwner::from(&a.owner),
        offset: a.offset,
        kind: a.kind.clone(),
    };
    Event::default()
        .event("runtime.session.activity.v1")
        .data(serde_json::to_string(&data).unwrap_or_default())
}

#[utoipa::path(
    get, path = "/api/v1/events", tag = "system",
    params(dto::EventsQuery, ("Last-Event-ID" = Option<String>, Header, description = "Resume after this seq")),
    responses(
        (status = 200, content_type = "text/event-stream", body = dto::StreamEvent, description = "One `data` per event, `id` = seq. `runtime.session.activity.v1` messages carry an ActivityNotice and no `id`."),
        (status = 422, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn events(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<dto::EventsQuery>,
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
    let activity_cursor = tri!(engine.last_activity_seq());
    let stream = stream::unfold(
        (engine, (after, activity_cursor), VecDeque::<Event>::new()),
        |(engine, (mut cursor, mut activity_cursor), mut buffer)| async move {
            loop {
                if let Some(event) = buffer.pop_front() {
                    return Some((
                        Ok::<Event, Infallible>(event),
                        (engine, (cursor, activity_cursor), buffer),
                    ));
                }
                match engine.events_after(cursor, EVENT_BATCH) {
                    Ok(list) if !list.is_empty() => {
                        cursor = list.last().map_or(cursor, |e| e.seq);
                        buffer.extend(list.iter().map(sse_event));
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "reading events failed"),
                }
                match engine.activities_after(activity_cursor, EVENT_BATCH) {
                    Ok(list) if !list.is_empty() => {
                        activity_cursor = list.last().map_or(activity_cursor, |(seq, _)| *seq);
                        buffer.extend(list.iter().map(|(_, a)| activity_event(a)));
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "reading session activity failed"),
                }
                if !buffer.is_empty() {
                    continue;
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

pub async fn api_not_found() -> Response {
    ApiError::not_found().into_response()
}

const DRAFT_ACTIVE: &[&str] = &["queued", "preparing", "drafting", "committing", "failed"];
const DRAFT_FINISHED: &[&str] = &["succeeded", "cancelled"];

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
                context_paths: req.context_paths,
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
enum DraftAction {
    Retry,
    Cancel,
}

async fn draft_action(
    s: AppState,
    id: String,
    (method, uri, headers, raw): (Method, Uri, HeaderMap, Bytes),
    which: DraftAction,
) -> Response {
    let key = tri!(require_key(&headers));
    let draft = SpecDraftId(tri!(path_id(&id)));
    let (reason, note) = match which {
        DraftAction::Retry => {
            let req: dto::RetrySpecDraftBody = tri!(body(&raw));
            (req.reason, req.note)
        }
        DraftAction::Cancel => {
            let req: dto::RequiredReason = tri!(body(&raw));
            (req.reason, None)
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
        },
        target_type: "spec_draft",
        target_id: Some(draft.0),
        reason: Some(reason.clone()),
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
                DraftAction::Retry => {
                    engine.retry_spec_draft(draft, &reason, note, &cause, Some(&action))
                }
                DraftAction::Cancel => {
                    engine.cancel_spec_draft(draft, &reason, &cause, Some(&action))
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
    post, path = "/api/v1/scm/{integration_id}/user-link/authorizations", tag = "accounts",
    params(("integration_id" = String, Path, description = "`[[scm]].id` from the operator config"), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    request_body = dto::StartUserLinkBody,
    responses(
        (status = 202, body = dto::UserLinkAuthorization, description = "A pending authorization: a new one, or the one still pending with more than a minute left"),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn start_user_link(
    State(s): State<AppState>,
    Path(integration): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let _: dto::StartUserLinkBody = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::UserLinkAuthorization(v) => reply(
            StatusCode::ACCEPTED,
            &dto::UserLinkAuthorization::from_view(v),
        ),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "user_link.authorize",
        target_type: "user_link_authorization",
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
            engine
                .start_user_link(&integration, &cause, Some(&action))
                .await
                .map(|_| None)
                .map_err(|e| (e, None))
        },
    )
    .await
}

#[utoipa::path(
    get, path = "/api/v1/scm/{integration_id}/user-link/authorizations/{authorization_id}", tag = "accounts",
    params(("integration_id" = String, Path, description = "`[[scm]].id` from the operator config"), ("authorization_id" = uuid::Uuid, Path)),
    responses(
        (status = 200, body = dto::UserLinkAuthorization),
        (status = 404, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_user_link_authorization(
    State(s): State<AppState>,
    Path((integration, id)): Path<(String, String)>,
) -> Response {
    let engine = tri!(s.engine());
    let id = saka_core::ids::UserLinkAuthorizationId(tri!(path_id(&id)));
    let view = tri!(engine.user_link_authorization(&integration, saka_engine::OPERATOR, id));
    ok(
        StatusCode::OK,
        &dto::UserLinkAuthorization::from_view(&view),
    )
}

#[utoipa::path(
    get, path = "/api/v1/scm/{integration_id}/user-link", tag = "accounts",
    params(("integration_id" = String, Path, description = "`[[scm]].id` from the operator config")),
    responses(
        (status = 200, body = dto::UserLink),
        (status = 404, body = dto::Problem, content_type = "application/problem+json", description = "`not_found` for an unknown integration, `user_link_not_found` without a linked account"),
    )
)]
pub async fn get_user_link(State(s): State<AppState>, Path(integration): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let link = tri!(engine.user_link(&integration, saka_engine::OPERATOR));
    ok(StatusCode::OK, &dto::UserLink::from(&link))
}

#[utoipa::path(
    delete, path = "/api/v1/scm/{integration_id}/user-link", tag = "accounts",
    params(("integration_id" = String, Path, description = "`[[scm]].id` from the operator config"), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")),
    responses(
        (status = 204, description = "The link is gone, or there was none; the tokens are not revoked at the provider"),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn remove_user_link(
    State(s): State<AppState>,
    Path(integration): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let engine = tri!(s.engine().cloned());
    let m = Mutation {
        name: "user_link.remove",
        target_type: "user_link",
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
        &no_content,
        |cause, action| async move {
            engine
                .remove_user_link(&integration, &cause, Some(&action))
                .map(|()| None)
                .map_err(|e| (e, None))
        },
    )
    .await
}

fn diagnostic_filter(pairs: Vec<(String, String)>) -> Result<DiagnosticFilter, ApiError> {
    let mut checks = std::collections::BTreeSet::new();
    let mut project = None;
    for (key, value) in pairs {
        match key.as_str() {
            "check" => match value.parse::<CheckId>() {
                Ok(id) if !id.is_client() => {
                    checks.insert(id);
                }
                _ => return Err(ApiError::validation("check", "unknown")),
            },
            "project_id" => project = opt_id(Some(&value), "project_id")?.map(ProjectId),
            _ => {}
        }
    }
    Ok(DiagnosticFilter {
        checks: (!checks.is_empty()).then_some(checks),
        project_id: project,
    })
}

fn without_engine(filter: &DiagnosticFilter, server_version: &str) -> Report {
    let failing: Vec<&str> = ["db", "migrations", "worker", "reconciler"].to_vec();
    let checks = CheckId::server()
        .filter(|id| filter.checks.as_ref().is_none_or(|c| c.contains(id)))
        .map(|id| CheckResult {
            id,
            subject: Subject::instance(),
            finding: if id == CheckId::ServerReady {
                diagnostics::server_ready(&failing)
            } else {
                diagnostics::engine_not_ready()
            },
            duration_ms: 0,
        })
        .collect();
    let now = Timestamp(jiff::Timestamp::now().as_millisecond());
    Report::new(now, server_version, ReportMode::Online, checks)
}

#[utoipa::path(
    get, path = "/api/v1/diagnostics", tag = "system", params(dto::DiagnosticsQuery),
    responses(
        (status = 200, body = dto::DiagnosticReport, headers(("Cache-Control" = String, description = "Always `no-store`: every request runs the checks again."))),
        (status = 422, body = dto::Problem, content_type = "application/problem+json")
    )
)]
pub async fn diagnostics(
    State(s): State<AppState>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let filter = tri!(diagnostic_filter(pairs));
    let version = &s.config().server_version;
    let report = match s.engine_if_ready() {
        Some(engine) => tri!(engine.diagnostics(&filter, version).await),
        None => without_engine(&filter, version),
    };
    let mut response = ok(StatusCode::OK, &dto::DiagnosticReport::from(&report));
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

#[utoipa::path(
    get, path = "/api/v1/projects/{project_id}/hooks", tag = "hooks",
    params(("project_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::HookList), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_project_hooks(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = ProjectId(tri!(path_id(&id)));
    let hooks = tri!(engine.project_hooks(id));
    ok(
        StatusCode::OK,
        &dto::HookList {
            items: hooks.iter().map(dto::Hook::from).collect(),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/hook-runs", tag = "hooks", params(dto::HookRunQuery),
    responses((status = 200, body = dto::Page<dto::HookRun>), (status = 422, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn list_hook_runs(
    State(s): State<AppState>,
    Query(q): Query<dto::HookRunQuery>,
) -> Response {
    let engine = tri!(s.engine());
    let Some(project) = tri!(opt_id(q.project_id.as_deref(), "project_id")) else {
        return ApiError::validation("project_id", "required").into_response();
    };
    let state = match q.state.as_deref().filter(|v| !v.is_empty()) {
        None => None,
        Some(v) => Some(tri!(
            v.parse()
                .map_err(|_| ApiError::validation("state", "invalid"))
        )),
    };
    let filter = HookRunFilter {
        project: Some(ProjectId(project)),
        hook: q.hook.clone().filter(|h| !h.is_empty()),
        state,
        work_item: tri!(opt_id(q.work_item_id.as_deref(), "work_item_id")).map(WorkItemId),
    };
    let key = format!(
        "hook-runs:{project}:{:?}:{:?}:{:?}",
        filter.hook, filter.state, filter.work_item
    );
    let p = tri!(page(q.cursor.as_deref(), q.limit, &key));
    let paged = tri!(engine.hook_runs(&filter, &p));
    ok(
        StatusCode::OK,
        &dto::Page {
            items: paged.items.iter().map(dto::HookRun::from).collect(),
            next_cursor: paged.next.map(|k| encode_cursor(k, &key)),
        },
    )
}

#[utoipa::path(
    get, path = "/api/v1/hook-runs/{hook_run_id}", tag = "hooks",
    params(("hook_run_id" = uuid::Uuid, Path)),
    responses((status = 200, body = dto::HookRun), (status = 404, body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn get_hook_run(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let engine = tri!(s.engine());
    let id = HookRunId(tri!(path_id(&id)));
    let run = tri!(engine.hook_run(id));
    ok(StatusCode::OK, &dto::HookRun::from(&run))
}

#[utoipa::path(
    post, path = "/api/v1/hook-runs/{hook_run_id}/actions/rerun", tag = "hooks",
    params(("hook_run_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::RequiredReason,
    responses((status = 202, body = dto::HookRun), (status = "4XX", body = dto::Problem, content_type = "application/problem+json"))
)]
pub async fn rerun_hook(
    State(s): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let run = HookRunId(tri!(path_id(&id)));
    let req: dto::RequiredReason = tri!(body(&raw));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::HookRun(r) => reply(StatusCode::ACCEPTED, &dto::HookRun::from(r)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "hook_run.rerun",
        target_type: "hook_run",
        target_id: Some(run.0),
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
                .rerun_hook(run, &req.reason, &cause, Some(&action))
                .map(|r| Some(r.project_id))
                .map_err(|e| (e, None))
        },
    )
    .await
}
