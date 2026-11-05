use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use saka_core::secret::SecretText;
use saka_ports::{Clock, Notification, Notifier, PortError, PortResult};
use serde_json::json;

use crate::discord::Discord;
use crate::render::{Lang, render};
use crate::telegram::Telegram;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Telegram,
    Discord,
}

impl ChannelKind {
    pub fn parse(raw: &str) -> Option<ChannelKind> {
        match raw {
            "telegram" => Some(ChannelKind::Telegram),
            "discord" => Some(ChannelKind::Discord),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Telegram => "telegram",
            ChannelKind::Discord => "discord",
        }
    }

    pub fn default_api_url(self) -> &'static str {
        match self {
            ChannelKind::Telegram => "https://api.telegram.org",
            ChannelKind::Discord => "https://discord.com/api/v10",
        }
    }
}

pub struct ChannelSpec {
    pub name: String,
    pub kind: ChannelKind,
    pub chat: String,
    pub api_url: String,
    pub token: SecretText,
}

pub struct ChatConfig {
    pub language: Lang,
    pub public_url: Option<String>,
    pub channels: Vec<ChannelSpec>,
    pub user_agent: String,
}

enum Sender {
    Telegram(Telegram),
    Discord(Discord),
}

struct Channel {
    kind: ChannelKind,
    sender: Sender,
}

pub struct ChatNotifier {
    language: Lang,
    public_url: Option<String>,
    channels: BTreeMap<String, Channel>,
}

impl ChatNotifier {
    pub fn new(cfg: ChatConfig, clock: &Arc<dyn Clock>) -> PortResult<ChatNotifier> {
        let mut channels = BTreeMap::new();
        for spec in cfg.channels {
            let sender = match spec.kind {
                ChannelKind::Telegram => Sender::Telegram(Telegram::new(
                    &spec.api_url,
                    spec.token,
                    &spec.chat,
                    &cfg.user_agent,
                    clock.clone(),
                )?),
                ChannelKind::Discord => Sender::Discord(Discord::new(
                    &spec.api_url,
                    spec.token,
                    &spec.chat,
                    &cfg.user_agent,
                    clock.clone(),
                )?),
            };
            channels.insert(
                spec.name,
                Channel {
                    kind: spec.kind,
                    sender,
                },
            );
        }
        Ok(ChatNotifier {
            language: cfg.language,
            public_url: cfg.public_url,
            channels,
        })
    }
}

#[async_trait]
impl Notifier for ChatNotifier {
    async fn send(&self, notification: &Notification) -> PortResult<()> {
        let Some(channel) = self.channels.get(&notification.channel) else {
            return Err(PortError::permanent(
                "notify_channel_unconfigured",
                json!({ "channel": notification.channel }),
            ));
        };
        let text = render(self.language, notification, self.public_url.as_deref());
        match &channel.sender {
            Sender::Telegram(t) => t.send(&text).await?,
            Sender::Discord(d) => d.send(&text, &notification.dedupe_key).await?,
        }
        tracing::info!(
            channel = %notification.channel,
            kind = channel.kind.as_str(),
            code = %notification.code,
            "notify.sent"
        );
        Ok(())
    }
}
