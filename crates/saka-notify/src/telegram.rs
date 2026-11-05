use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::header::USER_AGENT;
use saka_core::secret::SecretText;
use saka_ports::{ChatAction, ChatActions, Clock, PortError, PortResult};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::api::{BODY_MAX_BYTES, body_of, client, status_error, transport_error};
use crate::render::{Button, Lang, Reply, parse_callback};

const UPDATES_BODY_MAX_BYTES: usize = 512 * 1024;
const UPDATES_PER_POLL: u32 = 10;
const LONG_POLL_SLACK: Duration = Duration::from_secs(10);
const WARN_EVERY: Duration = Duration::from_secs(60);
const EDIT_MAX_CHARS: usize = 4000;

#[derive(Debug, Clone, Copy)]
pub struct PollTiming {
    pub long_poll_secs: u64,
    pub backoff_start: Duration,
    pub backoff_max: Duration,
}

impl Default for PollTiming {
    fn default() -> PollTiming {
        PollTiming {
            long_poll_secs: 30,
            backoff_start: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
        }
    }
}

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

    async fn call(
        &self,
        method: &str,
        body: &Value,
        timeout: Option<Duration>,
        max_bytes: usize,
    ) -> PortResult<Value> {
        let mut request = self
            .http
            .post(format!("{}/bot{}/{method}", self.base, self.token.expose()))
            .header(USER_AGENT, &self.user_agent)
            .json(body);
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        let response = request.send().await.map_err(|e| transport_error(&e))?;
        let status = response.status().as_u16();
        let body = body_of(response, max_bytes).await;
        if (200..300).contains(&status) {
            return body.ok_or_else(|| {
                PortError::retryable("notify_unavailable", json!({ "reason": "body" }))
            });
        }
        Err(status_error(status, body.as_ref(), self.clock.as_ref()))
    }

    pub(crate) async fn send(&self, text: &str, button: Option<&Button>) -> PortResult<()> {
        let mut body = json!({
            "chat_id": self.chat_id,
            "text": text,
            "link_preview_options": { "is_disabled": true },
        });
        if let Some(button) = button {
            body["reply_markup"] = json!({
                "inline_keyboard": [[{ "text": button.label, "callback_data": button.data() }]]
            });
        }
        self.call("sendMessage", &body, None, BODY_MAX_BYTES)
            .await
            .map(|_| ())
    }

    async fn updates(&self, offset: Option<i64>, timing: &PollTiming) -> PortResult<Vec<Value>> {
        let mut body = json!({
            "timeout": timing.long_poll_secs,
            "limit": UPDATES_PER_POLL,
            "allowed_updates": ["callback_query"],
        });
        if let Some(offset) = offset {
            body["offset"] = json!(offset);
        }
        let timeout = Duration::from_secs(timing.long_poll_secs) + LONG_POLL_SLACK;
        let answer = self
            .call("getUpdates", &body, Some(timeout), UPDATES_BODY_MAX_BYTES)
            .await?;
        answer
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| PortError::retryable("notify_unavailable", json!({ "reason": "body" })))
    }

    async fn answer(&self, callback_id: &str, text: &str, alert: bool) -> PortResult<()> {
        let body = json!({ "callback_query_id": callback_id, "text": text, "show_alert": alert });
        self.call("answerCallbackQuery", &body, None, BODY_MAX_BYTES)
            .await
            .map(|_| ())
    }

    async fn settle(&self, message_id: i64, text: &str) -> PortResult<()> {
        let text: String = text.chars().take(EDIT_MAX_CHARS).collect();
        let body = json!({ "chat_id": self.chat_id, "message_id": message_id, "text": text,
            "link_preview_options": { "is_disabled": true } });
        self.call("editMessageText", &body, None, BODY_MAX_BYTES)
            .await
            .map(|_| ())
    }

    async fn drop_webhook(&self) -> PortResult<()> {
        self.call("deleteWebhook", &json!({}), None, BODY_MAX_BYTES)
            .await
            .map(|_| ())
    }
}

pub(crate) struct Poller {
    pub(crate) telegram: Telegram,
    pub(crate) channel: String,
    pub(crate) allowed: BTreeSet<String>,
    pub(crate) lang: Lang,
    pub(crate) handler: Arc<dyn ChatActions>,
    pub(crate) timing: PollTiming,
}

fn fatal(error: &PortError) -> bool {
    error.code == "notify_channel_rejected"
        && matches!(
            error.params.get("status").and_then(Value::as_u64),
            Some(401 | 403 | 404)
        )
}

impl Poller {
    pub(crate) async fn run(self, mut stop: watch::Receiver<bool>) {
        tracing::info!(channel = %self.channel, "notify.poll.started");
        let mut offset: Option<i64> = None;
        let mut backoff = self.timing.backoff_start;
        let mut warned: Option<Instant> = None;
        let mut webhook_dropped = false;
        loop {
            if *stop.borrow() {
                break;
            }
            let step = async {
                if !webhook_dropped {
                    self.telegram.drop_webhook().await?;
                    webhook_dropped = true;
                }
                self.telegram.updates(offset, &self.timing).await
            };
            let result = tokio::select! {
                _ = stop.changed() => break,
                result = step => result,
            };
            match result {
                Ok(updates) => {
                    backoff = self.timing.backoff_start;
                    for update in updates {
                        if let Some(id) = update.get("update_id").and_then(Value::as_i64) {
                            offset = Some(id.saturating_add(1));
                        }
                        self.handle(&update).await;
                    }
                }
                Err(e) if fatal(&e) => {
                    tracing::error!(channel = %self.channel, code = %e.code, "notify.poll.rejected");
                    break;
                }
                Err(e) => {
                    if warned.is_none_or(|at| at.elapsed() >= WARN_EVERY) {
                        warned = Some(Instant::now());
                        tracing::warn!(channel = %self.channel, code = %e.code, "notify.poll.backoff");
                    }
                    tokio::select! {
                        _ = stop.changed() => break,
                        () = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(self.timing.backoff_max);
                }
            }
        }
        tracing::info!(channel = %self.channel, "notify.poll.stopped");
    }

    async fn reply(&self, callback_id: &str, reply: Reply, alert: bool) {
        let text = reply.text(self.lang);
        if let Err(e) = self.telegram.answer(callback_id, text, alert).await {
            tracing::warn!(channel = %self.channel, code = %e.code, "notify.answer.failed");
        }
    }

    async fn handle(&self, update: &Value) {
        let Some(callback) = update.get("callback_query") else {
            return;
        };
        let Some(callback_id) = callback.get("id").and_then(Value::as_str) else {
            return;
        };
        let user = callback
            .pointer("/from/id")
            .and_then(Value::as_i64)
            .map(|id| id.to_string());
        let chat = callback.pointer("/message/chat/id").and_then(Value::as_i64);
        let Some(user) = user.filter(|u| self.allowed.contains(u)) else {
            self.reply(callback_id, Reply::NotAllowed, true).await;
            return;
        };
        if chat != Some(self.telegram.chat_id) {
            self.reply(callback_id, Reply::NotAllowed, true).await;
            return;
        }
        let parsed = callback
            .get("data")
            .and_then(Value::as_str)
            .and_then(parse_callback);
        let Some((kind, blocker_id)) = parsed else {
            self.reply(callback_id, Reply::Invalid, false).await;
            return;
        };
        let outcome = self
            .handler
            .act(&ChatAction {
                platform: "telegram",
                user_id: user,
                kind,
                blocker_id,
            })
            .await;
        let reply = Reply::of(outcome, kind);
        tracing::info!(channel = %self.channel, platform = "telegram", outcome = ?outcome, "chat.action");
        self.reply(callback_id, reply, false).await;
        let message_id = callback
            .pointer("/message/message_id")
            .and_then(Value::as_i64);
        let original = callback.pointer("/message/text").and_then(Value::as_str);
        if let (true, Some(message_id), Some(original)) = (reply.settles(), message_id, original) {
            let text = format!("{original}\n\n{}", reply.text(self.lang));
            if let Err(e) = self.telegram.settle(message_id, &text).await {
                tracing::warn!(channel = %self.channel, code = %e.code, "notify.settle.failed");
            }
        }
    }
}
