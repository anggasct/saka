use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use saka_core::diagnostics::{self, CheckResult, Report, Subject};
use saka_core::ids::{HookRunId, ProjectId, WorkItemId};
use saka_core::time::Timestamp;
use saka_core::vocab::{CheckId, ReportMode};
use saka_engine::{DiagnosticFilter, HookRunFilter, Produced};

use super::tasks::opt_id;
use super::{reply, unexpected};
use crate::dto;
use crate::http::{Mutation, body, encode_cursor, mutate, ok, page, path_id, require_key};
use crate::problem::ApiError;
use crate::state::AppState;

pub(super) fn diagnostic_filter(
    pairs: Vec<(String, String)>,
) -> Result<DiagnosticFilter, ApiError> {
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

pub(super) fn without_engine(filter: &DiagnosticFilter, server_version: &str) -> Report {
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
