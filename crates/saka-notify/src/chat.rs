use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use saka_core::secret::SecretText;
use saka_ports::{ChatActions, Clock, Notification, Notifier, PortError, PortResult};
use serde_json::json;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::discord::Discord;
use crate::render::{Lang, button, render};
use crate::telegram::{PollTiming, Poller, Telegram};

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
    pub allowed_users: Option<Vec<String>>,
}

pub struct ChatConfig {
    pub language: Lang,
    pub public_url: Option<String>,
    pub channels: Vec<ChannelSpec>,
    pub user_agent: String,
    pub poll: PollTiming,
}

enum Sender {
    Telegram(Telegram),
    Discord(Discord),
}

struct Channel {
    kind: ChannelKind,
    sender: Sender,
    allowed_users: Option<BTreeSet<String>>,
}

pub struct ChatNotifier {
    language: Lang,
    public_url: Option<String>,
    channels: BTreeMap<String, Channel>,
    poll: PollTiming,
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
                    allowed_users: spec.allowed_users.map(|u| u.into_iter().collect()),
                },
            );
        }
        Ok(ChatNotifier {
            language: cfg.language,
            public_url: cfg.public_url,
            channels,
            poll: cfg.poll,
        })
    }

    pub fn spawn_pollers(
        &self,
        handler: &Arc<dyn ChatActions>,
        stop: &watch::Receiver<bool>,
    ) -> Vec<JoinHandle<()>> {
        let mut tasks = Vec::new();
        for (name, channel) in &self.channels {
            let (Sender::Telegram(telegram), Some(allowed)) =
                (&channel.sender, &channel.allowed_users)
            else {
                continue;
            };
            let poller = Poller {
                telegram: telegram.clone(),
                channel: name.clone(),
                allowed: allowed.clone(),
                lang: self.language,
                handler: handler.clone(),
                timing: self.poll,
            };
            tasks.push(tokio::spawn(poller.run(stop.clone())));
        }
        tasks
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
        let button = channel
            .allowed_users
            .as_ref()
            .and_then(|_| button(self.language, notification));
        match &channel.sender {
            Sender::Telegram(t) => t.send(&text, button.as_ref()).await?,
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
