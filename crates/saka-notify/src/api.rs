use std::time::Duration;

use reqwest::Response;
use saka_ports::{Clock, PortError, PortResult};
use serde_json::{Value, json};

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const BODY_MAX_BYTES: usize = 64 * 1024;
const RETRY_AFTER_MAX_SECS: f64 = 3600.0;

pub(crate) fn client() -> PortResult<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| PortError::permanent("notify_client_unavailable", json!({})))
}

pub(crate) fn transport_error(e: &reqwest::Error) -> PortError {
    let reason = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else {
        "transport"
    };
    PortError::retryable("notify_unavailable", json!({ "reason": reason }))
}

pub(crate) async fn body_of(mut response: Response, max_bytes: usize) -> Option<Value> {
    let mut bytes = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if bytes.len() + chunk.len() > max_bytes {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).ok()
}

fn retry_after_secs(body: &Value) -> Option<f64> {
    body.pointer("/parameters/retry_after")
        .or_else(|| body.get("retry_after"))
        .and_then(Value::as_f64)
        .filter(|s| s.is_finite() && *s >= 0.0)
}

pub(crate) fn status_error(status: u16, body: Option<&Value>, clock: &dyn Clock) -> PortError {
    match status {
        429 | 500..=599 => {
            let mut error = PortError::retryable("notify_unavailable", json!({ "status": status }));
            if status == 429
                && let Some(secs) = body.and_then(retry_after_secs)
            {
                let ms = (secs.min(RETRY_AFTER_MAX_SECS) * 1000.0).ceil() as i64;
                error.retry_at = Some(clock.now().plus_ms(ms));
            }
            error
        }
        _ => PortError::permanent("notify_channel_rejected", json!({ "status": status })),
    }
}
