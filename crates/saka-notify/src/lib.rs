mod api;
mod chat;
mod discord;
mod render;
mod telegram;
mod webhook;

pub use chat::{ChannelKind, ChannelSpec, ChatConfig, ChatNotifier};
pub use render::{
    Button, Lang, Reply, TEXT_MAX_CHARS, button, callback_data, parse_callback, render,
};
pub use telegram::PollTiming;
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
