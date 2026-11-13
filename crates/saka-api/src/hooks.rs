use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use saka_engine::EngineError;
use saka_ports::PortError;
use serde_json::json;

use crate::dto;
use crate::http::ok;
use crate::problem::ApiError;
use crate::state::AppState;

pub const MAX_WEBHOOK_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_INCIDENT_BYTES: usize = 1024 * 1024;
const REQUEST_LIMIT: Duration = Duration::from_secs(10);

pub fn hooks_router(state: AppState) -> Router {
    Router::new()
        .route("/hooks/incident/{source}", post(receive_incident))
        .route("/hooks/{integration_id}", post(receive))
        .fallback(not_found)
        .with_state(state)
}

async fn not_found() -> Response {
    ApiError::not_found().into_response()
}

fn refusal(e: &PortError) -> ApiError {
    match e.code.as_str() {
        "scm_integration_unknown" => ApiError::not_found(),
        "webhook_not_configured" => {
            ApiError::new(StatusCode::NOT_FOUND, "webhook_not_configured", json!({}))
        }
        "webhook_signature_invalid" => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "webhook_signature_invalid",
            json!({}),
        ),
        "webhook_malformed" => ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed_json",
            json!({ "field": e.params.get("field").cloned().unwrap_or_default() }),
        ),
        _ => ApiError::internal(),
    }
}

fn reject(state: &AppState, integration_id: &str, error: ApiError) -> Response {
    if let Some(engine) = state.engine_if_ready() {
        engine.webhook_rejected(integration_id);
    }
    tracing::warn!(integration_id, code = %error.code, "scm.webhook.rejected");
    error.into_response()
}

async fn receive(
    State(state): State<AppState>,
    Path(integration_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match tokio::time::timeout(
        REQUEST_LIMIT,
        handle(&state, &integration_id, headers, body),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => reject(
            &state,
            &integration_id,
            ApiError::new(StatusCode::REQUEST_TIMEOUT, "request_timeout", json!({})),
        ),
    }
}

async fn handle(
    state: &AppState,
    integration_id: &str,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let engine = match state.engine() {
        Ok(e) => e.clone(),
        Err(e) => return e.into_response(),
    };
    match engine.webhook_configured(integration_id) {
        Ok(true) => {}
        Ok(false) => {
            let e = PortError::permanent("webhook_not_configured", json!({}));
            return reject(state, integration_id, refusal(&e));
        }
        Err(e) => return reject(state, integration_id, refusal(&e)),
    }
    let Ok(bytes) = to_bytes(body, MAX_WEBHOOK_BYTES).await else {
        return reject(
            state,
            integration_id,
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                json!({}),
            ),
        );
    };
    let pairs: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
        .collect();
    let delivery = match engine.parse_webhook(integration_id, &pairs, &bytes) {
        Ok(d) => d,
        Err(e) => return reject(state, integration_id, refusal(&e)),
    };
    match engine.accept_webhook(integration_id, &delivery) {
        Ok(accepted) => {
            tracing::info!(
                integration_id,
                delivery_id = %delivery.delivery_id,
                event = %delivery.event,
                action = delivery.action.as_deref().unwrap_or(""),
                outcome = accepted.outcome.as_str(),
                change_requests = accepted.change_requests,
                "scm.webhook.received"
            );
            ok(
                StatusCode::ACCEPTED,
                &dto::WebhookAccepted::from_accepted(&accepted),
            )
        }
        Err(e) => ApiError::from(e).into_response(),
    }
}

fn incident_port_refusal(p: &PortError) -> ApiError {
    match p.code.as_str() {
        "webhook_signature_invalid" => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "webhook_signature_invalid",
            json!({}),
        ),
        "malformed_payload" => ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed_payload",
            json!({ "field": p.params.get("field").cloned().unwrap_or_default() }),
        ),
        _ => ApiError::internal(),
    }
}

fn incident_reject(state: &AppState, source: &str, error: ApiError) -> Response {
    if let Some(engine) = state.engine_if_ready() {
        engine.incident_rejected(source, &error.code);
    }
    error.into_response()
}

#[utoipa::path(
    post, path = "/hooks/incident/{source}", tag = "incidents",
    params(("source" = String, Path, description = "The name of an incident source in the server config")),
    request_body(content = String, content_type = "application/json", description = "The raw body of the monitoring tool"),
    responses(
        (status = 202, body = dto::IncidentDelivery),
        (status = "4XX", body = dto::Problem, content_type = "application/problem+json")
    )
)]
pub async fn receive_incident(
    State(state): State<AppState>,
    Path(source): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match tokio::time::timeout(
        REQUEST_LIMIT,
        handle_incident(&state, &source, headers, body),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => incident_reject(
            &state,
            &source,
            ApiError::new(StatusCode::REQUEST_TIMEOUT, "request_timeout", json!({})),
        ),
    }
}

async fn handle_incident(
    state: &AppState,
    source: &str,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let engine = match state.engine() {
        Ok(e) => e.clone(),
        Err(e) => return e.into_response(),
    };
    if engine.incident_source_config(source).is_none() {
        return ApiError::not_found().into_response();
    }
    let Ok(bytes) = to_bytes(body, MAX_INCIDENT_BYTES).await else {
        return incident_reject(
            state,
            source,
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                json!({}),
            ),
        );
    };
    if let Err(retry_after) = engine.incident_rate_limit(source) {
        let mut response = incident_reject(
            state,
            source,
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "rate_limited", json!({})),
        );
        if let Ok(value) = retry_after.to_string().parse() {
            response.headers_mut().insert("retry-after", value);
        }
        return response;
    }
    let pairs: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
        .collect();
    match engine.receive_incident(source, &pairs, &bytes) {
        Ok(receipt) => ok(StatusCode::ACCEPTED, &dto::IncidentDelivery::from(&receipt)),
        Err(EngineError::Port(p)) => incident_port_refusal(&p).into_response(),
        Err(e) => ApiError::from(e).into_response(),
    }
}
