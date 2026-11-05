mod webhook;

pub use webhook::{HttpWebhooks, signature};

use async_trait::async_trait;
use saka_ports::{Notification, Notifier, PortError, PortResult};
use serde_json::json;

#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredNotifier;

#[async_trait]
impl Notifier for UnconfiguredNotifier {
    async fn send(&self, notification: &Notification) -> PortResult<()> {
        Err(PortError::permanent(
            "notify_channel_unconfigured",
            json!({ "channel": notification.channel }),
        ))
    }
}
