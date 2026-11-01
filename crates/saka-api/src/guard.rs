use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tracing::Instrument;
use uuid::Uuid;

use crate::problem::{ApiError, CORRELATION};
use crate::state::AppState;

fn host_name(host: &str) -> &str {
    if host.starts_with('[') {
        return host.split_inclusive(']').next().unwrap_or(host);
    }
    host.rsplit_once(':').map_or(host, |(h, _)| h)
}

pub fn loopback_host(host: &str) -> bool {
    matches!(
        host_name(host).to_ascii_lowercase().as_str(),
        "127.0.0.1" | "[::1]" | "localhost"
    )
}

fn origin_allowed(headers: &HeaderMap, state: &AppState) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let own = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| format!("http://{h}"));
    own.as_deref() == Some(origin)
        || state
            .config()
            .allowed_origins
            .iter()
            .any(|o| o.trim_end_matches('/') == origin)
}

#[derive(Debug, Clone)]
struct Route(String);

pub async fn label(req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string());
    let mut response = next.run(req).await;
    if let Some(route) = route {
        response.extensions_mut().insert(Route(route));
    }
    response
}

pub async fn guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let id = Uuid::now_v7();
    let method = req.method().clone();
    let span = tracing::info_span!(
        "request",
        correlation_id = %id,
        method = %method,
        route = tracing::field::Empty,
    );
    let started = std::time::Instant::now();
    let headers = req.headers();
    let host_ok = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(loopback_host);
    let refusal = if !host_ok {
        Some(ApiError::new(
            StatusCode::FORBIDDEN,
            "host_not_allowed",
            json!({}),
        ))
    } else if method == Method::POST && !origin_allowed(headers, &state) {
        Some(ApiError::new(
            StatusCode::FORBIDDEN,
            "origin_not_allowed",
            json!({}),
        ))
    } else {
        None
    };
    let response = CORRELATION
        .scope(id, async move {
            match refusal {
                Some(e) => e.into_response(),
                None => next.run(req).await,
            }
        })
        .instrument(span.clone())
        .await;
    let status = response.status().as_u16();
    let route = response
        .extensions()
        .get::<Route>()
        .map_or_else(|| "unmatched".to_string(), |r| r.0.clone());
    span.record("route", route.as_str());
    state.count_request(&route, status);
    span.in_scope(|| {
        tracing::info!(
            status,
            elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "request"
        );
    });
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_hosts_pass() {
        for ok in [
            "127.0.0.1",
            "127.0.0.1:7420",
            "localhost:5173",
            "[::1]:7420",
            "[::1]",
            "LOCALHOST",
        ] {
            assert!(loopback_host(ok), "{ok}");
        }
        for bad in [
            "example.com",
            "10.0.0.1:7420",
            "127.0.0.1.nip.io",
            "[::2]:1",
            "localhost.evil:80",
        ] {
            assert!(!loopback_host(bad), "{bad}");
        }
    }
}
