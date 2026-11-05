use std::time::Duration;

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use reqwest::header::{CONTENT_TYPE, USER_AGENT};
use saka_ports::{PortError, PortResult, WebhookPost, WebhookReceipt, Webhooks};
use serde_json::json;
use sha2::Sha256;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn signature(secret: &str, timestamp_secs: i64, body: &[u8]) -> PortResult<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| PortError::permanent("hook_signing_failed", json!({})))?;
    mac.update(timestamp_secs.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(format!("sha256={hex}"))
}

#[derive(Debug, Clone)]
pub struct HttpWebhooks {
    http: reqwest::Client,
    user_agent: String,
}

impl HttpWebhooks {
    pub fn new(user_agent: impl Into<String>) -> PortResult<HttpWebhooks> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| PortError::permanent("webhook_client_unavailable", json!({})))?;
        Ok(HttpWebhooks {
            http,
            user_agent: user_agent.into(),
        })
    }
}

fn transport_reason(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else {
        "transport"
    }
}

#[async_trait]
impl Webhooks for HttpWebhooks {
    async fn post(&self, request: &WebhookPost) -> PortResult<WebhookReceipt> {
        let sig = signature(
            request.secret.expose(),
            request.timestamp_secs,
            &request.body,
        )?;
        let response = self
            .http
            .post(&request.url)
            .timeout(Duration::from_millis(request.timeout_ms))
            .header(CONTENT_TYPE, "application/json")
            .header(USER_AGENT, &self.user_agent)
            .header("X-Saka-Event", &request.event)
            .header("X-Saka-Delivery", &request.delivery_id)
            .header("X-Saka-Timestamp", request.timestamp_secs.to_string())
            .header("X-Saka-Signature-256", sig)
            .body(request.body.clone())
            .send()
            .await
            .map_err(|e| {
                PortError::retryable(
                    "hook_webhook_unavailable",
                    json!({ "reason": transport_reason(&e) }),
                )
            })?;
        let status = response.status().as_u16();
        match status {
            200..=299 => Ok(WebhookReceipt { status }),
            429 | 500..=599 => Err(PortError::retryable(
                "hook_webhook_unavailable",
                json!({ "status": status }),
            )),
            _ => Err(PortError::permanent(
                "hook_webhook_rejected",
                json!({ "status": status }),
            )),
        }
    }
}
