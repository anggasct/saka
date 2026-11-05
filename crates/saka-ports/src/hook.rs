use async_trait::async_trait;
use saka_core::secret::SecretText;
use serde_json::Value;

use crate::PortResult;

#[derive(Debug, Clone)]
pub struct WebhookPost {
    pub url: String,
    pub event: String,
    pub delivery_id: String,
    pub body: Vec<u8>,
    pub secret: SecretText,
    pub timeout_ms: u64,
    pub timestamp_secs: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebhookReceipt {
    pub status: u16,
}

#[async_trait]
pub trait Webhooks: Send + Sync {
    async fn post(&self, request: &WebhookPost) -> PortResult<WebhookReceipt>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub channel: String,
    pub code: String,
    pub params: Value,
    pub dedupe_key: String,
}

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn send(&self, notification: &Notification) -> PortResult<()>;
}
