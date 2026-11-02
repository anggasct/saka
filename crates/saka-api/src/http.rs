use std::future::Future;

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use saka_engine::{Action, Cause, EngineError, Key, OPERATOR, Page, Reply, Respond};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::problem::{ApiError, correlation, problem_response};
use crate::state::AppState;

pub const IDEMPOTENCY_KEY: &str = "idempotency-key";
pub const REPLAYED: HeaderName = HeaderName::from_static("idempotent-replayed");
pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 200;

pub fn json_response(status: StatusCode, body: &Value) -> Response {
    let mut response = (status, body.to_string()).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(tag) = etag(body) {
        headers.insert(header::ETAG, tag);
    }
    response
}

pub fn ok<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_value(value) {
        Ok(body) => json_response(status, &body),
        Err(_) => ApiError::internal().into_response(),
    }
}

fn etag(body: &Value) -> Option<HeaderValue> {
    if let Some(id) = body.pointer("/revision/id").and_then(Value::as_str) {
        return HeaderValue::from_str(&format!("\"{id}\"")).ok();
    }
    if body.get("task_key").is_some()
        && let Some(v) = body.get("version").and_then(Value::as_u64)
    {
        return HeaderValue::from_str(&format!("W/\"{v}\"")).ok();
    }
    None
}

pub fn reply_response(reply: &Reply, replayed: bool) -> Response {
    let status = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = if status.is_client_error() || status.is_server_error() {
        problem_response(status, reply.body.clone())
    } else if status == StatusCode::NO_CONTENT {
        status.into_response()
    } else {
        json_response(status, &reply.body)
    };
    if status == StatusCode::CREATED
        && let Some(id) = reply.body.get("id").and_then(Value::as_str)
        && reply.body.get("workspace").is_some()
        && let Ok(location) = HeaderValue::from_str(&format!("/api/v1/projects/{id}"))
    {
        response.headers_mut().insert(header::LOCATION, location);
    }
    if replayed {
        response
            .headers_mut()
            .insert(REPLAYED, HeaderValue::from_static("true"));
    }
    response
}

pub fn body<T: DeserializeOwned>(bytes: &Bytes) -> Result<T, ApiError> {
    let raw: &[u8] = if bytes.iter().all(u8::is_ascii_whitespace) {
        b"{}"
    } else {
        bytes
    };
    serde_json::from_slice(raw).map_err(|e| {
        use serde_json::error::Category;
        match e.classify() {
            Category::Data => {
                let message = e.to_string();
                let quoted = message.split('`').nth(1).unwrap_or("body").to_string();
                let code = if message.starts_with("unknown field") {
                    "unknown"
                } else if message.starts_with("missing field") {
                    "required"
                } else {
                    "invalid"
                };
                let field = if code == "invalid" {
                    "body".to_string()
                } else {
                    quoted
                };
                ApiError::validation(&field, code)
            }
            _ => ApiError::new(StatusCode::BAD_REQUEST, "malformed_json", json!({})),
        }
    })
}

pub fn if_match(headers: &HeaderMap) -> Result<String, ApiError> {
    headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "precondition_required",
                json!({}),
            )
        })
}

pub fn encode_cursor(key: Key, filter: &str) -> String {
    let text = format!("{}:{}:{}", key.0, key.1, digest(filter));
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text)
}

fn digest(filter: &str) -> String {
    let d = Sha256::digest(filter.as_bytes());
    d.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

pub fn page(cursor: Option<&str>, limit: Option<u32>, filter: &str) -> Result<Page, ApiError> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::validation("limit", "out_of_range"));
    }
    let invalid = || {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "cursor_invalid",
            json!({}),
        )
    };
    let after = match cursor.filter(|c| !c.is_empty()) {
        None => None,
        Some(c) => {
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(c)
                .map_err(|_| invalid())?;
            let text = String::from_utf8(bytes).map_err(|_| invalid())?;
            let mut parts = text.splitn(3, ':');
            let key: i64 = parts
                .next()
                .and_then(|k| k.parse().ok())
                .ok_or_else(invalid)?;
            let id: Uuid = parts
                .next()
                .and_then(|i| i.parse().ok())
                .ok_or_else(invalid)?;
            if parts.next() != Some(digest(filter).as_str()) {
                return Err(invalid());
            }
            Some((key, id))
        }
    };
    Ok(Page { after, limit })
}

pub fn states(raw: Option<&str>, allowed: &[&str]) -> Result<Vec<String>, ApiError> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(|s| {
            let s = s.trim();
            if allowed.contains(&s) {
                Ok(s.to_string())
            } else {
                Err(ApiError::validation("state", "unknown"))
            }
        })
        .collect()
}

pub fn path_id(raw: &str) -> Result<Uuid, ApiError> {
    raw.parse().map_err(|_| ApiError::not_found())
}

pub struct Mutation<'a> {
    pub name: &'a str,
    pub target_type: &'a str,
    pub target_id: Option<Uuid>,
    pub reason: Option<String>,
}

fn idempotency_key(headers: &HeaderMap) -> Result<String, ApiError> {
    let Some(value) = headers.get(IDEMPOTENCY_KEY) else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "idempotency_key_required",
            json!({}),
        ));
    };
    let key = value.to_str().unwrap_or_default();
    let printable = key.bytes().all(|b| (0x21..=0x7e).contains(&b));
    if key.is_empty() || key.len() > 128 || !printable {
        return Err(ApiError::validation("Idempotency-Key", "invalid"));
    }
    Ok(key.to_string())
}

pub fn request_hash(method: &Method, path: &str, body: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(method.as_str().as_bytes());
    h.update(path.as_bytes());
    h.update(body);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn require_key(headers: &HeaderMap) -> Result<String, ApiError> {
    idempotency_key(headers)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the request parts a mutation is keyed by"
)]
pub async fn mutate<'r, F, Fut>(
    state: &AppState,
    key: String,
    method: &Method,
    path: &str,
    raw_body: &Bytes,
    m: Mutation<'_>,
    respond: &'r Respond<'r>,
    run: F,
) -> Response
where
    F: FnOnce(Cause, Action<'r>) -> Fut,
    Fut: Future<
        Output = Result<
            Option<saka_core::ids::ProjectId>,
            (EngineError, Option<saka_core::ids::ProjectId>),
        >,
    >,
{
    let engine = match state.engine() {
        Ok(e) => e.clone(),
        Err(e) => return e.into_response(),
    };
    let hash = request_hash(method, path, raw_body);
    let lock = state.key_lock(&key);
    let response = {
        let _held = lock.lock().await;
        match engine.find_action(OPERATOR, &key) {
            Err(e) => ApiError::from(e).into_response(),
            Ok(Some(stored)) if stored.request_hash == hash => reply_response(&stored.reply, true),
            Ok(Some(_)) => ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "idempotency_key_reused",
                json!({}),
            )
            .into_response(),
            Ok(None) => {
                let cause = Cause {
                    actor: OPERATOR.to_string(),
                    causation_id: None,
                    correlation_id: correlation().unwrap_or_else(Uuid::now_v7),
                };
                let action = Action {
                    idempotency_key: key.clone(),
                    request_hash: hash.clone(),
                    name: m.name.to_string(),
                    target_type: m.target_type.to_string(),
                    target_id: m.target_id,
                    reason: m.reason.clone(),
                    respond,
                };
                let refusal_action = Action {
                    idempotency_key: key.clone(),
                    request_hash: hash.clone(),
                    name: m.name.to_string(),
                    target_type: m.target_type.to_string(),
                    target_id: m.target_id,
                    reason: m.reason.clone(),
                    respond,
                };
                match run(cause.clone(), action).await {
                    Ok(_) => match engine.find_action(OPERATOR, &key) {
                        Ok(Some(stored)) => reply_response(&stored.reply, false),
                        Ok(None) => ApiError::internal().into_response(),
                        Err(e) => ApiError::from(e).into_response(),
                    },
                    Err((e, project)) => {
                        let error = ApiError::from(e);
                        if error.status.is_client_error() {
                            let reply = Reply {
                                status: error.status.as_u16(),
                                body: error.body(),
                            };
                            if let Err(e) =
                                engine.record_refusal(&refusal_action, &cause, project, reply)
                            {
                                tracing::warn!(error = %e, "refusal not stored");
                            }
                        }
                        error.into_response()
                    }
                }
            }
        }
    };
    drop(lock);
    state.release_key(&key);
    response
}
