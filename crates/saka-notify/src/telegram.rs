use std::sync::Arc;

use reqwest::header::USER_AGENT;
use saka_core::secret::SecretText;
use saka_ports::{Clock, PortError, PortResult};
use serde_json::json;

use crate::api::{body_of, client, status_error, transport_error};

#[derive(Clone)]
pub(crate) struct Telegram {
    http: reqwest::Client,
    base: String,
    token: SecretText,
    chat_id: i64,
    user_agent: String,
    clock: Arc<dyn Clock>,
}

impl Telegram {
    pub(crate) fn new(
        api_url: &str,
        token: SecretText,
        chat: &str,
        user_agent: &str,
        clock: Arc<dyn Clock>,
    ) -> PortResult<Telegram> {
        let chat_id = chat.parse().map_err(|_| {
            PortError::permanent("notify_field_invalid", json!({ "field": "chat" }))
        })?;
        Ok(Telegram {
            http: client()?,
            base: api_url.trim_end_matches('/').to_string(),
            token,
            chat_id,
            user_agent: user_agent.to_string(),
            clock,
        })
    }

    fn endpoint(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.base, self.token.expose())
    }

    pub(crate) async fn send(&self, text: &str) -> PortResult<()> {
        let body = json!({
            "chat_id": self.chat_id,
            "text": text,
            "link_preview_options": { "is_disabled": true },
        });
        let response = self
            .http
            .post(self.endpoint("sendMessage"))
            .header(USER_AGENT, &self.user_agent)
            .json(&body)
            .send()
            .await
            .map_err(|e| transport_error(&e))?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let body = body_of(response).await;
        Err(status_error(status, body.as_ref(), self.clock.as_ref()))
    }
}
