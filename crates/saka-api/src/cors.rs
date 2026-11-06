use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

const ALLOWED_METHODS: &str = "GET, POST, PUT, DELETE, OPTIONS";
const ALLOWED_HEADERS: &str =
    "authorization, content-type, accept, idempotency-key, if-match, if-none-match, last-event-id";
const EXPOSED_HEADERS: &str = "etag, retry-after";
const PREFLIGHT_MAX_AGE: &str = "600";

fn allowed_origin(headers: &HeaderMap, state: &AppState) -> Option<HeaderValue> {
    let origin = headers.get(header::ORIGIN)?;
    let text = origin.to_str().ok()?;
    state
        .config()
        .allowed_origins
        .iter()
        .any(|o| o.trim_end_matches('/') == text)
        .then(|| origin.clone())
}

fn mark(response: &mut Response, origin: HeaderValue) {
    let headers = response.headers_mut();
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.append(header::VARY, HeaderValue::from_static("origin"));
}

pub async fn cors(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(origin) = allowed_origin(req.headers(), &state) else {
        return next.run(req).await;
    };
    let preflight = req.method() == Method::OPTIONS
        && req
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    if preflight {
        let mut response = StatusCode::NO_CONTENT.into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static(ALLOWED_METHODS),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(ALLOWED_HEADERS),
        );
        headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static(PREFLIGHT_MAX_AGE),
        );
        mark(&mut response, origin);
        return response;
    }
    let mut response = next.run(req).await;
    response.headers_mut().insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(EXPOSED_HEADERS),
    );
    mark(&mut response, origin);
    response
}
