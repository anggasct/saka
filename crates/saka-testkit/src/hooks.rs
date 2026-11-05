use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use saka_ports::{
    Notification, Notifier, PortError, PortResult, WebhookPost, WebhookReceipt, Webhooks,
};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookScript {
    Status(u16),
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookRecord {
    pub url: String,
    pub event: String,
    pub delivery_id: String,
    pub body: String,
    pub secret: String,
    pub timestamp_secs: i64,
    pub timeout_ms: u64,
}

#[derive(Debug, Default)]
struct State {
    webhook_script: VecDeque<WebhookScript>,
    webhooks: Vec<WebhookRecord>,
    notify_script: VecDeque<PortError>,
    notifications: Vec<Notification>,
}

#[derive(Debug, Default)]
pub struct FakeHooks {
    state: Mutex<State>,
}

impl FakeHooks {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn script_webhook(&self, script: WebhookScript) {
        self.state().webhook_script.push_back(script);
    }

    pub fn webhooks(&self) -> Vec<WebhookRecord> {
        self.state().webhooks.clone()
    }

    pub fn script_notify(&self, error: PortError) {
        self.state().notify_script.push_back(error);
    }

    pub fn notifications(&self) -> Vec<Notification> {
        self.state().notifications.clone()
    }
}

#[async_trait]
impl Webhooks for FakeHooks {
    async fn post(&self, request: &WebhookPost) -> PortResult<WebhookReceipt> {
        let mut state = self.state();
        state.webhooks.push(WebhookRecord {
            url: request.url.clone(),
            event: request.event.clone(),
            delivery_id: request.delivery_id.clone(),
            body: String::from_utf8_lossy(&request.body).into_owned(),
            secret: request.secret.expose().to_string(),
            timestamp_secs: request.timestamp_secs,
            timeout_ms: request.timeout_ms,
        });
        match state.webhook_script.pop_front() {
            None | Some(WebhookScript::Status(200..=299)) => Ok(WebhookReceipt { status: 200 }),
            Some(WebhookScript::Status(status @ (429 | 500..=599))) => Err(PortError::retryable(
                "hook_webhook_unavailable",
                json!({ "status": status }),
            )),
            Some(WebhookScript::Status(status)) => Err(PortError::permanent(
                "hook_webhook_rejected",
                json!({ "status": status }),
            )),
            Some(WebhookScript::Unavailable) => Err(PortError::retryable(
                "hook_webhook_unavailable",
                json!({ "reason": "connect" }),
            )),
        }
    }
}

#[async_trait]
impl Notifier for FakeHooks {
    async fn send(&self, notification: &Notification) -> PortResult<()> {
        let mut state = self.state();
        state.notifications.push(notification.clone());
        match state.notify_script.pop_front() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
