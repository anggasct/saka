use std::sync::Arc;

use reqwest::header::{AUTHORIZATION, USER_AGENT};
use saka_core::secret::SecretText;
use saka_ports::{Clock, PortError, PortResult};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::api::{BODY_MAX_BYTES, body_of, client, status_error, transport_error};

const NONCE_BYTES: usize = 12;

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

    pub(crate) async fn send(&self, text: &str, dedupe_key: &str) -> PortResult<()> {
        let body = json!({
            "content": text,
            "allowed_mentions": { "parse": [] },
            "nonce": nonce(dedupe_key),
            "enforce_nonce": true,
        });
        let response = self
            .http
            .post(format!("{}/channels/{}/messages", self.base, self.channel))
            .header(AUTHORIZATION, format!("Bot {}", self.token.expose()))
            .header(USER_AGENT, &self.user_agent)
            .json(&body)
            .send()
            .await
            .map_err(|e| transport_error(&e))?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let body = body_of(response, BODY_MAX_BYTES).await;
        Err(status_error(status, body.as_ref(), self.clock.as_ref()))
    }
}
