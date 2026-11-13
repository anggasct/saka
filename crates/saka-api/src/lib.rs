mod auth;
mod auth_routes;
mod cors;
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

pub use auth::{AuthSettings, Authenticator};
pub use guard::loopback_host;
pub use hooks::{MAX_WEBHOOK_BYTES, hooks_router};
pub use state::{ApiConfig, AppState};

#[derive(OpenApi)]
#[openapi(
    info(title = "Saka control API", version = "1", description = "Local control API of a Saka server."),
    paths(
        routes::projects::add_project, routes::projects::check_project_config, routes::projects::list_projects, routes::projects::get_project, routes::projects::reload_project,
        routes::projects::set_secret, routes::projects::list_secrets, routes::projects::remove_secret,
        routes::projects::get_authorship, routes::projects::set_authorship, routes::projects::set_user_token,
        routes::projects::remove_user_token,
        routes::user_links::start_user_link, routes::user_links::get_user_link_authorization, routes::user_links::get_user_link,
        routes::user_links::remove_user_link,
        routes::specs::add_spec, routes::specs::add_quick_spec, routes::specs::list_specs, routes::specs::get_spec, routes::specs::approve_spec, routes::specs::withdraw_spec,
        routes::specs::spec_template,
        routes::specs::list_files, routes::specs::get_file, routes::specs::put_file,
        routes::tasks::list_work_items, routes::tasks::get_work_item, routes::tasks::get_work_packet, routes::tasks::start, routes::tasks::merge, routes::tasks::retry, routes::tasks::revert, routes::tasks::cancel, routes::tasks::takeover, routes::tasks::return_to_loop, routes::tasks::primary_action, routes::tasks::review, routes::tasks::fix, routes::tasks::take_external_change,
        routes::approvals::list_permissions, routes::approvals::answer_permission, routes::approvals::list_decisions, routes::approvals::get_decision, routes::approvals::answer_decision, routes::sessions::get_session,
        routes::sessions::list_sessions, routes::sessions::get_timeline, routes::sessions::steer, routes::sessions::interrupt, routes::sessions::stop,
        routes::approvals::list_permission_rules, routes::approvals::add_permission_rule, routes::approvals::preview_permission_rule, routes::approvals::evaluate_permission, routes::approvals::revoke_rule, routes::system::list_agent_accounts,
        routes::system::list_usage_records,
        routes::drafts::create_spec_draft, routes::drafts::list_spec_drafts, routes::drafts::get_spec_draft,
        routes::drafts::list_spec_draft_rounds, routes::drafts::get_spec_draft_content, routes::drafts::get_spec_draft_packet, routes::drafts::get_spec_draft_changes,
        routes::drafts::retry_spec_draft, routes::drafts::cancel_spec_draft, routes::drafts::message_spec_draft,
        routes::drafts::commit_spec_draft, routes::drafts::discard_spec_draft,
        routes::issue_intakes::list_issue_intakes, routes::issue_intakes::get_issue_intake,
        routes::issue_intakes::create_issue_intake, routes::issue_intakes::dismiss_issue_intake, routes::issue_intakes::retry_issue_intake,
        routes::deployments::list_environments, routes::deployments::list_deployments, routes::deployments::get_deployment, routes::deployments::deploy_environment, routes::deployments::cancel_deployment, routes::deployments::draft_fix, routes::tasks::accept_unverified,
        routes::incidents::list_incident_sources, routes::incidents::list_incidents, routes::incidents::get_incident,
        routes::operations::list_project_hooks, routes::operations::list_hook_runs, routes::operations::get_hook_run, routes::inbox::list_inbox, routes::inbox::inbox_summary, routes::dismiss_inbox_item, routes::operations::rerun_hook,
        auth_routes::status, auth_routes::create_pairing, auth_routes::pair, auth_routes::list_devices, auth_routes::revoke_device,
        auth_routes::register_start, auth_routes::register_finish, auth_routes::login_start, auth_routes::login_finish, auth_routes::logout, auth_routes::revoke_credential,
        routes::system::meta, routes::system::events, routes::operations::diagnostics,
        ops::healthz, ops::readyz, ops::metrics,
    ),
    components(schemas(dto::ActivityNotice)),
    tags(
        (name = "projects"), (name = "accounts"), (name = "specs"), (name = "spec-drafts"), (name = "files"), (name = "delivery"), (name = "runtime"), (name = "hooks"), (name = "deployments"), (name = "incidents"), (name = "inbox"),
        (name = "spec-drafts"), (name = "issue-intakes"), (name = "system"), (name = "ops"), (name = "auth"),
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
            post(routes::projects::add_project).get(routes::projects::list_projects),
        )
        .route(
            "/api/v1/projects/actions/check-config",
            post(routes::projects::check_project_config),
        )
        .route(
            "/api/v1/projects/{project_id}",
            get(routes::projects::get_project),
        )
        .route(
            "/api/v1/projects/{project_id}/actions/reload",
            post(routes::projects::reload_project),
        )
        .route(
            "/api/v1/projects/{project_id}/authorship",
            get(routes::projects::get_authorship).put(routes::projects::set_authorship),
        )
        .route(
            "/api/v1/projects/{project_id}/authorship/user-token",
            put(routes::projects::set_user_token).delete(routes::projects::remove_user_token),
        )
        .route(
            "/api/v1/projects/{project_id}/secrets",
            get(routes::projects::list_secrets),
        )
        .route(
            "/api/v1/projects/{project_id}/secrets/{name}",
            put(routes::projects::set_secret).delete(routes::projects::remove_secret),
        )
        .route(
            "/api/v1/projects/{project_id}/specs",
            post(routes::specs::add_spec).get(routes::specs::list_specs),
        )
        .route(
            "/api/v1/projects/{project_id}/quick-specs",
            post(routes::specs::add_quick_spec),
        )
        .route(
            "/api/v1/projects/{project_id}/files",
            get(routes::specs::list_files),
        )
        .route(
            "/api/v1/projects/{project_id}/file",
            get(routes::specs::get_file)
                .put(routes::specs::put_file)
                .layer(DefaultBodyLimit::max(FILE_BODY_MAX_BYTES)),
        )
        .route("/api/v1/specs/{spec_id}", get(routes::specs::get_spec))
        .route("/api/v1/spec-template", get(routes::specs::spec_template))
        .route(
            "/api/v1/projects/{project_id}/hooks",
            get(routes::operations::list_project_hooks),
        )
        .route(
            "/api/v1/projects/{project_id}/environments",
            get(routes::deployments::list_environments),
        )
        .route(
            "/api/v1/projects/{project_id}/deployments",
            get(routes::deployments::list_deployments),
        )
        .route(
            "/api/v1/deployments/{deployment_id}",
            get(routes::deployments::get_deployment),
        )
        .route(
            "/api/v1/projects/{project_id}/environments/{environment}/actions/deploy",
            post(routes::deployments::deploy_environment),
        )
        .route(
            "/api/v1/deployments/{deployment_id}/actions/draft-fix",
            post(routes::deployments::draft_fix),
        )
        .route(
            "/api/v1/deployments/{deployment_id}/actions/cancel",
            post(routes::deployments::cancel_deployment),
        )
        .route(
            "/api/v1/projects/{project_id}/incident-sources",
            get(routes::incidents::list_incident_sources),
        )
        .route(
            "/api/v1/projects/{project_id}/incidents",
            get(routes::incidents::list_incidents),
        )
        .route(
            "/api/v1/incidents/{incident_id}",
            get(routes::incidents::get_incident),
        )
        .route("/api/v1/inbox", get(routes::inbox::list_inbox))
        .route("/api/v1/inbox/summary", get(routes::inbox::inbox_summary))
        .route(
            "/api/v1/inbox/{inbox_item_id}/dismiss",
            post(routes::dismiss_inbox_item),
        )
        .route("/api/v1/hook-runs", get(routes::operations::list_hook_runs))
        .route(
            "/api/v1/hook-runs/{hook_run_id}",
            get(routes::operations::get_hook_run),
        )
        .route(
            "/api/v1/hook-runs/{hook_run_id}/actions/rerun",
            post(routes::operations::rerun_hook),
        )
        .route(
            "/api/v1/specs/{spec_id}/approve",
            post(routes::specs::approve_spec),
        )
        .route(
            "/api/v1/specs/{spec_id}/withdraw",
            post(routes::specs::withdraw_spec),
        )
        .route("/api/v1/work-items", get(routes::tasks::list_work_items))
        .route(
            "/api/v1/work-items/{work_item_id}",
            get(routes::tasks::get_work_item),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/work-packet",
            get(routes::tasks::get_work_packet),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/start",
            post(routes::tasks::start),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/merge",
            post(routes::tasks::merge),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/retry",
            post(routes::tasks::retry),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/accept-unverified",
            post(routes::tasks::accept_unverified),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/revert",
            post(routes::tasks::revert),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/primary",
            post(routes::tasks::primary_action),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/cancel",
            post(routes::tasks::cancel),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/takeover",
            post(routes::tasks::takeover),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/return",
            post(routes::tasks::return_to_loop),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/review",
            post(routes::tasks::review),
        )
        .route(
            "/api/v1/work-items/{work_item_id}/actions/fix",
            post(routes::tasks::fix),
        )
        .route(
            "/api/v1/projects/{project_id}/external-changes",
            post(routes::tasks::take_external_change),
        )
        .route(
            "/api/v1/permission-requests",
            get(routes::approvals::list_permissions),
        )
        .route(
            "/api/v1/permission-requests/{permission_request_id}/answer",
            post(routes::approvals::answer_permission),
        )
        .route(
            "/api/v1/decision-requests",
            get(routes::approvals::list_decisions),
        )
        .route(
            "/api/v1/decision-requests/{decision_request_id}",
            get(routes::approvals::get_decision),
        )
        .route(
            "/api/v1/decision-requests/{decision_request_id}/answer",
            post(routes::approvals::answer_decision),
        )
        .route("/api/v1/sessions", get(routes::sessions::list_sessions))
        .route(
            "/api/v1/sessions/{session_id}",
            get(routes::sessions::get_session),
        )
        .route(
            "/api/v1/sessions/{session_id}/timeline",
            get(routes::sessions::get_timeline),
        )
        .route(
            "/api/v1/sessions/{session_id}/steer",
            post(routes::sessions::steer),
        )
        .route(
            "/api/v1/sessions/{session_id}/interrupt",
            post(routes::sessions::interrupt),
        )
        .route(
            "/api/v1/sessions/{session_id}/stop",
            post(routes::sessions::stop),
        )
        .route(
            "/api/v1/projects/{project_id}/permission-rules",
            get(routes::approvals::list_permission_rules)
                .post(routes::approvals::add_permission_rule),
        )
        .route(
            "/api/v1/projects/{project_id}/permission-rules/preview",
            post(routes::approvals::preview_permission_rule),
        )
        .route(
            "/api/v1/projects/{project_id}/permission-rules/evaluate",
            post(routes::approvals::evaluate_permission),
        )
        .route(
            "/api/v1/permission-rules/{rule_id}/revoke",
            post(routes::approvals::revoke_rule),
        )
        .route(
            "/api/v1/projects/{project_id}/spec-drafts",
            post(routes::drafts::create_spec_draft).get(routes::drafts::list_spec_drafts),
        )
        .route(
            "/api/v1/projects/{project_id}/issue-intakes",
            get(routes::issue_intakes::list_issue_intakes)
                .post(routes::issue_intakes::create_issue_intake),
        )
        .route(
            "/api/v1/issue-intakes/{intake_id}/actions/dismiss",
            post(routes::issue_intakes::dismiss_issue_intake),
        )
        .route(
            "/api/v1/issue-intakes/{intake_id}/actions/retry",
            post(routes::issue_intakes::retry_issue_intake),
        )
        .route(
            "/api/v1/issue-intakes/{intake_id}",
            get(routes::issue_intakes::get_issue_intake),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}",
            get(routes::drafts::get_spec_draft),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/lint-rounds",
            get(routes::drafts::list_spec_draft_rounds),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/content",
            get(routes::drafts::get_spec_draft_content),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/changes",
            get(routes::drafts::get_spec_draft_changes),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/packet",
            get(routes::drafts::get_spec_draft_packet),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/actions/retry",
            post(routes::drafts::retry_spec_draft),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/actions/cancel",
            post(routes::drafts::cancel_spec_draft),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/message",
            post(routes::drafts::message_spec_draft),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/commit",
            post(routes::drafts::commit_spec_draft),
        )
        .route(
            "/api/v1/spec-drafts/{spec_draft_id}/discard",
            post(routes::drafts::discard_spec_draft),
        )
        .route(
            "/api/v1/scm/{integration_id}/user-link/authorizations",
            post(routes::user_links::start_user_link),
        )
        .route(
            "/api/v1/scm/{integration_id}/user-link/authorizations/{authorization_id}",
            get(routes::user_links::get_user_link_authorization),
        )
        .route(
            "/api/v1/scm/{integration_id}/user-link",
            get(routes::user_links::get_user_link).delete(routes::user_links::remove_user_link),
        )
        .route(
            "/api/v1/agent-accounts",
            get(routes::system::list_agent_accounts),
        )
        .route(
            "/api/v1/usage-records",
            get(routes::system::list_usage_records),
        )
        .route("/api/v1/meta", get(routes::system::meta))
        .route("/api/v1/diagnostics", get(routes::operations::diagnostics))
        .route("/api/v1/events", get(routes::system::events))
        .route("/api/v1/auth/status", get(auth_routes::status))
        .route("/api/v1/auth/pairings", post(auth_routes::create_pairing))
        .route("/api/v1/auth/pair", post(auth_routes::pair))
        .route(
            "/api/v1/auth/passkeys/register/start",
            post(auth_routes::register_start),
        )
        .route(
            "/api/v1/auth/passkeys/register/finish",
            post(auth_routes::register_finish),
        )
        .route("/api/v1/auth/login/start", post(auth_routes::login_start))
        .route("/api/v1/auth/login/finish", post(auth_routes::login_finish))
        .route("/api/v1/auth/logout", post(auth_routes::logout))
        .route(
            "/api/v1/auth/credentials/{credential_id}/revoke",
            post(auth_routes::revoke_credential),
        )
        .route("/api/v1/auth/devices", get(auth_routes::list_devices))
        .route(
            "/api/v1/auth/devices/{device_id}/revoke",
            post(auth_routes::revoke_device),
        )
        .route(
            "/api/{*rest}",
            get(routes::system::api_not_found).post(routes::system::api_not_found),
        )
        .route("/healthz", get(ops::healthz))
        .route("/readyz", get(ops::readyz))
        .route("/metrics", get(ops::metrics))
        .route_layer(from_fn(guard::label));
    api.fallback(spa::serve)
        .layer(from_fn_with_state(state.clone(), guard::guard))
        .layer(from_fn_with_state(state.clone(), cors::cors))
        .with_state(state)
}
