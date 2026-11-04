pub mod dto;
mod guard;
mod hooks;
mod http;
mod ops;
pub mod problem;
mod routes;
pub mod spa;
mod state;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware::{from_fn, from_fn_with_state};
use axum::routing::{get, post, put};
use utoipa::OpenApi;

pub use guard::loopback_host;
pub use hooks::{MAX_WEBHOOK_BYTES, hooks_router};
pub use state::{ApiConfig, AppState};

#[derive(OpenApi)]
#[openapi(
    info(title = "Saka control API", version = "1", description = "Local control API of a Saka server."),
    paths(
        routes::add_project, routes::list_projects, routes::get_project, routes::reload_project,
        routes::set_secret, routes::list_secrets, routes::remove_secret,
        routes::get_authorship, routes::set_authorship,
        routes::start_user_link, routes::get_user_link_authorization, routes::get_user_link,
        routes::remove_user_link,
        routes::add_spec, routes::list_specs, routes::get_spec, routes::approve_spec, routes::withdraw_spec,
        routes::spec_template,
        routes::list_files, routes::get_file, routes::put_file,
        routes::list_work_items, routes::get_work_item, routes::get_work_packet, routes::merge, routes::retry, routes::cancel,
        routes::list_permissions, routes::answer_permission, routes::get_session,
        routes::list_sessions, routes::get_timeline, routes::steer, routes::interrupt, routes::stop,
        routes::list_permission_rules, routes::revoke_rule, routes::list_agent_accounts,
        routes::list_usage_records,
        routes::create_spec_draft, routes::list_spec_drafts, routes::get_spec_draft,
        routes::list_spec_draft_rounds, routes::get_spec_draft_content, routes::get_spec_draft_packet,
        routes::retry_spec_draft, routes::cancel_spec_draft,
        routes::meta, routes::events,
        ops::healthz, ops::readyz, ops::metrics,
    ),
    components(schemas(dto::ActivityNotice)),
    tags(
        (name = "projects"), (name = "accounts"), (name = "specs"), (name = "spec-drafts"), (name = "files"), (name = "delivery"), (name = "runtime"),
        (name = "spec-drafts"), (name = "system"), (name = "ops"),
    )
)]
pub struct ApiDoc;

const FILE_BODY_MAX_BYTES: usize = 8 * 1024 * 1024;

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
            "/api/v1/projects/{project_id}/actions/reload",
            post(routes::reload_project),
        )
        .route(
            "/api/v1/projects/{project_id}/authorship",
            get(routes::get_authorship).put(routes::set_authorship),
        )
        .route(
            "/api/v1/projects/{project_id}/secrets",
            get(routes::list_secrets),
        )
        .route(
            "/api/v1/projects/{project_id}/secrets/{name}",
            put(routes::set_secret).delete(routes::remove_secret),
        )
        .route(
            "/api/v1/projects/{project_id}/specs",
            post(routes::add_spec).get(routes::list_specs),
        )
        .route(
            "/api/v1/projects/{project_id}/files",
            get(routes::list_files),
        )
        .route(
            "/api/v1/projects/{project_id}/file",
            get(routes::get_file)
                .put(routes::put_file)
                .layer(DefaultBodyLimit::max(FILE_BODY_MAX_BYTES)),
        )
        .route("/api/v1/specs/{spec_id}", get(routes::get_spec))
        .route("/api/v1/spec-template", get(routes::spec_template))
        .route(
            "/api/v1/specs/{spec_id}/approve",
            post(routes::approve_spec),
        )
        .route(
            "/api/v1/specs/{spec_id}/withdraw",
            post(routes::withdraw_spec),
        )
        .route("/api/v1/work-items", get(routes::list_work_items))
        .route(
            "/api/v1/work-items/{work_item_id}",
            get(routes::get_work_item),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/work-packet",
            get(routes::get_work_packet),
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
        .route("/api/v1/sessions", get(routes::list_sessions))
        .route("/api/v1/sessions/{session_id}", get(routes::get_session))
        .route(
            "/api/v1/sessions/{session_id}/timeline",
            get(routes::get_timeline),
        )
        .route("/api/v1/sessions/{session_id}/steer", post(routes::steer))
        .route(
            "/api/v1/sessions/{session_id}/interrupt",
            post(routes::interrupt),
        )
        .route("/api/v1/sessions/{session_id}/stop", post(routes::stop))
        .route(
            "/api/v1/projects/{project_id}/permission-rules",
            get(routes::list_permission_rules),
        )
        .route(
            "/api/v1/permission-rules/{rule_id}/revoke",
            post(routes::revoke_rule),
        )
        .route(
            "/api/v1/projects/{project_id}/spec-drafts",
            post(routes::create_spec_draft).get(routes::list_spec_drafts),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}",
            get(routes::get_spec_draft),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/lint-rounds",
            get(routes::list_spec_draft_rounds),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/content",
            get(routes::get_spec_draft_content),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/packet",
            get(routes::get_spec_draft_packet),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/actions/retry",
            post(routes::retry_spec_draft),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/actions/cancel",
            post(routes::cancel_spec_draft),
        )
        .route(
            "/api/v1/scm/{integration_id}/user-link/authorizations",
            post(routes::start_user_link),
        )
        .route(
            "/api/v1/scm/{integration_id}/user-link/authorizations/{authorization_id}",
            get(routes::get_user_link_authorization),
        )
        .route(
            "/api/v1/scm/{integration_id}/user-link",
            get(routes::get_user_link).delete(routes::remove_user_link),
        )
        .route("/api/v1/agent-accounts", get(routes::list_agent_accounts))
        .route("/api/v1/usage-records", get(routes::list_usage_records))
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
