mod gateway;

use std::sync::Arc;

use reqwest::header::{AUTHORIZATION, USER_AGENT};
use saka_core::secret::SecretText;
use saka_ports::{Clock, PortError, PortResult};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::api::{BODY_MAX_BYTES, body_of, client, status_error, transport_error};
use crate::render::Button;
use saka_ports::ChatActionKind;

pub(crate) use gateway::Gateway;

const NONCE_BYTES: usize = 12;
const STYLE_PRIMARY: u8 = 1;
const STYLE_DANGER: u8 = 4;

#[derive(Clone)]
pub(crate) struct Discord {
    http: reqwest::Client,
    base: String,
    token: SecretText,
    channel: String,
    user_agent: String,
    clock: Arc<dyn Clock>,
}

fn nonce(dedupe_key: &str) -> String {
    Sha256::digest(dedupe_key.as_bytes())
        .iter()
        .take(NONCE_BYTES)
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl Discord {
    pub(crate) fn new(
        api_url: &str,
        token: SecretText,
        channel: &str,
        user_agent: &str,
        clock: Arc<dyn Clock>,
    ) -> PortResult<Discord> {
        if channel.is_empty() || !channel.bytes().all(|b| b.is_ascii_digit()) {
            return Err(PortError::permanent(
                "notify_field_invalid",
                json!({ "field": "chat" }),
            ));
        }
        Ok(Discord {
            http: client()?,
            base: api_url.trim_end_matches('/').to_string(),
            token,
            channel: channel.to_string(),
            user_agent: user_agent.to_string(),
            clock,
        })
    }

    async fn call(&self, request: reqwest::RequestBuilder) -> PortResult<Option<Value>> {
        let response = request
            .header(AUTHORIZATION, format!("Bot {}", self.token.expose()))
            .header(USER_AGENT, &self.user_agent)
            .send()
            .await
            .map_err(|e| transport_error(&e))?;
        let status = response.status().as_u16();
        let body = body_of(response, BODY_MAX_BYTES).await;
        if (200..300).contains(&status) {
            return Ok(body);
        }
        Err(status_error(status, body.as_ref(), self.clock.as_ref()))
    }

    pub(crate) async fn send(
        &self,
        text: &str,
        dedupe_key: &str,
        button: Option<&Button>,
    ) -> PortResult<()> {
        let mut body = json!({
            "content": text,
            "allowed_mentions": { "parse": [] },
            "nonce": nonce(dedupe_key),
            "enforce_nonce": true,
        });
        if let Some(button) = button {
            let style = match button.kind {
                ChatActionKind::Retry => STYLE_PRIMARY,
                ChatActionKind::Cancel => STYLE_DANGER,
            };
            body["components"] = json!([{
                "type": 1,
                "components": [{
                    "type": 2,
                    "style": style,
                    "label": button.label,
                    "custom_id": button.data(),
                }]
            }]);
        }
        let url = format!("{}/channels/{}/messages", self.base, self.channel);
        self.call(self.http.post(url).json(&body)).await.map(|_| ())
    }

    async fn gateway_url(&self) -> PortResult<String> {
        let url = format!("{}/gateway/bot", self.base);
        let body = self.call(self.http.get(url)).await?;
        body.as_ref()
            .and_then(|b| b.get("url"))
            .and_then(Value::as_str)
            .map(|u| u.trim_end_matches('/').to_string())
            .ok_or_else(|| PortError::retryable("notify_unavailable", json!({ "reason": "body" })))
    }

    async fn respond(&self, interaction: &str, token: &str, body: &Value) -> PortResult<()> {
        let url = format!("{}/interactions/{interaction}/{token}/callback", self.base);
        self.call(self.http.post(url).json(body)).await.map(|_| ())
    }
}
