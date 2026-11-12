use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use saka_core::ids::{ProjectId, SpecId, SpecRevisionId};
use saka_engine::{EngineError, Precondition, Produced, QuickRequest};
use saka_spec::{Locale, QuickSpec, WorkKind};

use super::{SPEC_STATES, reply, unexpected};
use crate::dto;
use crate::http::{
    Mutation, body, encode_cursor, if_match, mutate, ok, page, path_id, require_key, states,
};
use crate::problem::ApiError;
use crate::state::AppState;

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
    post, path = "/api/v1/projects/{project_id}/quick-specs", tag = "specs",
    params(("project_id" = uuid::Uuid, Path), ("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters")), request_body = dto::QuickSpecBody,
    responses(
        (status = 201, body = dto::QuickSpecCreated),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json"),
        (status = 502, body = dto::Problem, content_type = "application/problem+json"),
    )
)]
pub async fn add_quick_spec(
    State(s): State<AppState>,
    Path(project): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    raw: Bytes,
) -> Response {
    let key = tri!(require_key(&headers));
    let project = ProjectId(tri!(path_id(&project)));
    let req: dto::QuickSpecBody = tri!(body(&raw));
    let request = tri!(quick_request(req));
    let engine = tri!(s.engine().cloned());
    let respond = |p: Produced<'_>| match p {
        Produced::Quick(v) => reply(StatusCode::CREATED, &dto::QuickSpecCreated::from_view(v)),
        _ => unexpected(),
    };
    let m = Mutation {
        name: "spec.quick",
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
                .quick_spec_with(project, request, &cause, Some(&action))
                .await
                .map(|_| Some(project))
                .map_err(|e| (e, Some(project)))
        },
    )
    .await
}

fn quick_request(req: dto::QuickSpecBody) -> Result<QuickRequest, ApiError> {
    let kind = req
        .kind
        .parse::<WorkKind>()
        .map_err(|_| ApiError::validation("kind", "invalid"))?;
    let locale = match req.locale.as_deref() {
        None => Locale::default(),
        Some(code) => {
            Locale::from_code(code).ok_or_else(|| ApiError::validation("locale", "invalid"))?
        }
    };
    Ok(QuickRequest {
        spec: QuickSpec {
            kind,
            title: req.title,
            description: req.description,
            done_when: req.done_when,
            regression: req.regression,
            criteria: req.ac,
            locale,
            repo: req.repo,
        },
        path: req.path,
        approve: req.approve,
    })
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

pub(super) fn file_path(q: &dto::FileQuery) -> Result<&str, ApiError> {
    let path = q
        .path
        .as_deref()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| ApiError::validation("path", "required"))?;
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

pub(super) fn precondition(headers: &HeaderMap) -> Result<Precondition, ApiError> {
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
    let kind = match q.kind.as_deref().map(str::parse::<saka_spec::WorkKind>) {
        None => saka_spec::WorkKind::default(),
        Some(Ok(kind)) => kind,
        Some(Err(_)) => return ApiError::validation("kind", "invalid").into_response(),
    };
    ok(
        StatusCode::OK,
        &dto::SpecTemplate {
            locale: locale.as_str().to_string(),
            kind: kind.as_str().to_string(),
            parser_version: saka_spec::PARSER_VERSION,
            content: saka_spec::template(locale, kind).to_string(),
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
