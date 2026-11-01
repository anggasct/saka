//! Control API over HTTP: JSON endpoints, server-sent events, OpenAPI, and ops endpoints.
//!
//! Every request passes the loopback `Host` guard; mutations also pass the `Origin` guard
//! and run once per `Idempotency-Key`. Errors are problem details with a stable `code`.

pub mod dto;
mod guard;
mod http;
mod ops;
pub mod problem;
mod routes;
pub mod spa;
mod state;

use axum::Router;
use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::{get, post};
use utoipa::OpenApi;

pub use guard::loopback_host;
pub use state::{ApiConfig, AppState};

#[derive(OpenApi)]
#[openapi(
    info(title = "Saka control API", version = "1", description = "Local control API of a Saka server."),
    paths(
        routes::add_project, routes::list_projects, routes::get_project,
        routes::add_spec, routes::list_specs, routes::get_spec, routes::approve_spec,
        routes::list_work_items, routes::get_work_item, routes::merge, routes::retry, routes::cancel,
        routes::list_permissions, routes::answer_permission, routes::get_session,
        routes::meta, routes::events,
        ops::healthz, ops::readyz, ops::metrics,
    ),
    tags(
        (name = "projects"), (name = "specs"), (name = "delivery"), (name = "runtime"),
        (name = "system"), (name = "ops"),
    )
)]
pub struct ApiDoc;

/// The OpenAPI document, as committed in `crates/saka-api/openapi.json`.
pub fn openapi_json() -> String {
    let mut text = ApiDoc::openapi().to_pretty_json().unwrap_or_default();
    text.push('\n');
    text
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route(
            "/api/v1/projects",
            post(routes::add_project).get(routes::list_projects),
        )
        .route("/api/v1/projects/{project_id}", get(routes::get_project))
        .route(
            "/api/v1/projects/{project_id}/specs",
            post(routes::add_spec).get(routes::list_specs),
        )
        .route("/api/v1/specs/{spec_id}", get(routes::get_spec))
        .route(
            "/api/v1/specs/{spec_id}/approve",
            post(routes::approve_spec),
        )
        .route("/api/v1/work-items", get(routes::list_work_items))
        .route(
            "/api/v1/work-items/{work_item_id}",
            get(routes::get_work_item),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/merge",
            post(routes::merge),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/retry",
            post(routes::retry),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/cancel",
            post(routes::cancel),
        )
        .route("/api/v1/permission-requests", get(routes::list_permissions))
        .route(
            "/api/v1/permission-requests/{permission_request_id}/answer",
            post(routes::answer_permission),
        )
        .route("/api/v1/sessions/{session_id}", get(routes::get_session))
        .route("/api/v1/meta", get(routes::meta))
        .route("/api/v1/events", get(routes::events))
        .route(
            "/api/{*rest}",
            get(routes::api_not_found).post(routes::api_not_found),
        )
        .route("/healthz", get(ops::healthz))
        .route("/readyz", get(ops::readyz))
        .route("/metrics", get(ops::metrics))
        .route_layer(from_fn(guard::label));
    api.fallback(spa::serve)
        .layer(from_fn_with_state(state.clone(), guard::guard))
        .with_state(state)
}
