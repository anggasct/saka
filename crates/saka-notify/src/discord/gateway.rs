use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use saka_core::secret::SecretText;
use saka_ports::{ChatAction, ChatActions, PortError};
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use super::Discord;
use crate::render::{Lang, Reply, parse_callback};
use crate::telegram::PollTiming;

const GATEWAY_QUERY: &str = "v=10&encoding=json";
const FRAME_MAX_BYTES: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const SEEN_MAX: usize = 256;
const COMPONENT_INTERACTION: u64 = 3;
const RESPONSE_MESSAGE: u8 = 4;
const RESPONSE_UPDATE: u8 = 7;
const EPHEMERAL: u32 = 64;
const FATAL_CLOSE_CODES: [u16; 6] = [4004, 4010, 4011, 4012, 4013, 4014];
const NO_RESUME_CLOSE_CODES: [u16; 2] = [4007, 4009];

pub(crate) struct Gateway {
    pub(crate) discord: Discord,
    pub(crate) channel: String,
    pub(crate) allowed: BTreeSet<String>,
    pub(crate) lang: Lang,
    pub(crate) handler: Arc<dyn ChatActions>,
    pub(crate) timing: PollTiming,
}

#[derive(Default)]
struct Session {
    id: Option<String>,
    resume_url: Option<String>,
    seq: Option<u64>,
    seen: VecDeque<String>,
}

enum Ended {
    Stop,
    Fatal,
    Reconnect { resume: bool },
}

impl Gateway {
    pub(crate) async fn run(self, mut stop: watch::Receiver<bool>) {
        tracing::info!(channel = %self.channel, "notify.gateway.started");
        let mut session = Session::default();
        let mut backoff = self.timing.backoff_start;
        loop {
            if *stop.borrow() {
                break;
            }
            let ended = self.connect(&mut session, &mut stop, &mut backoff).await;
            match ended {
                Ended::Stop => break,
                Ended::Fatal => {
                    tracing::error!(channel = %self.channel, "notify.gateway.rejected");
                    break;
                }
                Ended::Reconnect { resume } => {
                    if !resume {
                        session.id = None;
                        session.resume_url = None;
                        session.seq = None;
                    }
                    tracing::warn!(channel = %self.channel, resume, "notify.gateway.backoff");
                    tokio::select! {
                        _ = stop.changed() => break,
                        () = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(self.timing.backoff_max);
                }
            }
        }
        tracing::info!(channel = %self.channel, "notify.gateway.stopped");
    }

    async fn connect(
        &self,
        session: &mut Session,
        stop: &mut watch::Receiver<bool>,
        backoff: &mut Duration,
    ) -> Ended {
        let base = match (&session.resume_url, &session.id) {
            (Some(url), Some(_)) => url.clone(),
            _ => match self.discord.gateway_url().await {
                Ok(url) => url,
                Err(e) if e.code == "notify_channel_rejected" => return Ended::Fatal,
                Err(_) => return Ended::Reconnect { resume: false },
            },
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(FRAME_MAX_BYTES))
            .max_frame_size(Some(FRAME_MAX_BYTES));
        let url = format!("{base}/?{GATEWAY_QUERY}");
        let connecting = tokio_tungstenite::connect_async_with_config(url, Some(config), false);
        let socket = tokio::select! {
            _ = stop.changed() => return Ended::Stop,
            result = tokio::time::timeout(CONNECT_TIMEOUT, connecting) => match result {
                Ok(Ok((socket, _))) => socket,
                _ => return Ended::Reconnect { resume: session.id.is_some() },
            },
        };
        self.talk(socket, session, stop, backoff).await
    }

    async fn talk(
        &self,
        socket: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        session: &mut Session,
        stop: &mut watch::Receiver<bool>,
        backoff: &mut Duration,
    ) -> Ended {
        let (mut sink, mut frames) = socket.split();
        let mut interval: Option<tokio::time::Interval> = None;
        let mut acked = true;
        loop {
            let beat = async {
                match interval.as_mut() {
                    Some(interval) => interval.tick().await,
                    None => std::future::pending().await,
                }
            };
            let frame = tokio::select! {
                _ = stop.changed() => {
                    let _ = sink.send(Message::Close(None)).await;
                    return Ended::Stop;
                }
                _ = beat => {
                    if !acked {
                        return Ended::Reconnect { resume: session.id.is_some() };
                    }
                    acked = false;
                    let heartbeat = json!({ "op": 1, "d": session.seq });
                    if sink.send(Message::text(heartbeat.to_string())).await.is_err() {
                        return Ended::Reconnect { resume: session.id.is_some() };
                    }
                    continue;
                }
                frame = frames.next() => frame,
            };
            let text = match frame {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(Message::Close(close))) => {
                    return match close.map(|c| u16::from(c.code)) {
                        Some(code) if FATAL_CLOSE_CODES.contains(&code) => Ended::Fatal,
                        Some(code) if NO_RESUME_CLOSE_CODES.contains(&code) => {
                            Ended::Reconnect { resume: false }
                        }
                        _ => Ended::Reconnect {
                            resume: session.id.is_some(),
                        },
                    };
                }
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => {
                    return Ended::Reconnect {
                        resume: session.id.is_some(),
                    };
                }
            };
            let Ok(message) = serde_json::from_str::<Value>(text.as_str()) else {
                continue;
            };
            if let Some(seq) = message.get("s").and_then(Value::as_u64) {
                session.seq = Some(seq);
            }
            match message.get("op").and_then(Value::as_u64) {
                Some(10) => {
                    let ms = message
                        .pointer("/d/heartbeat_interval")
                        .and_then(Value::as_u64)
                        .filter(|ms| *ms > 0)
                        .unwrap_or(41_250);
                    let period = Duration::from_millis(ms);
                    let mut timer =
                        tokio::time::interval_at(tokio::time::Instant::now() + period / 2, period);
                    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    interval = Some(timer);
                    let hello = self.handshake(session);
                    if sink.send(Message::text(hello.to_string())).await.is_err() {
                        return Ended::Reconnect {
                            resume: session.id.is_some(),
                        };
                    }
                }
                Some(11) => acked = true,
                Some(1) => {
                    let heartbeat = json!({ "op": 1, "d": session.seq });
                    let _ = sink.send(Message::text(heartbeat.to_string())).await;
                }
                Some(7) => {
                    return Ended::Reconnect {
                        resume: session.id.is_some(),
                    };
                }
                Some(9) => {
                    let resumable = message.get("d").and_then(Value::as_bool).unwrap_or(false);
                    return Ended::Reconnect {
                        resume: resumable && session.id.is_some(),
                    };
                }
                Some(0) => {
                    *backoff = self.timing.backoff_start;
                    self.dispatch(&message, session).await;
                }
                _ => {}
            }
        }
    }

    fn handshake(&self, session: &Session) -> Value {
        let token: &SecretText = &self.discord.token;
        match (&session.id, session.seq) {
            (Some(id), Some(seq)) => json!({
                "op": 6,
                "d": { "token": token.expose(), "session_id": id, "seq": seq }
            }),
            _ => json!({
                "op": 2,
                "d": {
                    "token": token.expose(),
                    "intents": 0,
                    "properties": { "os": "linux", "browser": "saka", "device": "saka" }
                }
            }),
        }
    }

    async fn dispatch(&self, message: &Value, session: &mut Session) {
        let data = message.get("d").unwrap_or(&Value::Null);
        match message.get("t").and_then(Value::as_str) {
            Some("READY") => {
                session.id = data
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                session.resume_url = data
                    .get("resume_gateway_url")
                    .and_then(Value::as_str)
                    .map(|u| u.trim_end_matches('/').to_string());
                tracing::info!(channel = %self.channel, "notify.gateway.ready");
            }
            Some("INTERACTION_CREATE") => self.interaction(data, session).await,
            _ => {}
        }
    }

    async fn refuse(&self, id: &str, token: &str, reply: Reply) {
        let body = json!({
            "type": RESPONSE_MESSAGE,
            "data": {
                "content": reply.text(self.lang),
                "flags": EPHEMERAL,
                "allowed_mentions": { "parse": [] }
            }
        });
        self.answer(id, token, &body).await;
    }

    async fn answer(&self, id: &str, token: &str, body: &Value) {
        if let Err(PortError { code, .. }) = self.discord.respond(id, token, body).await {
            tracing::warn!(channel = %self.channel, code = %code, "notify.answer.failed");
        }
    }

    async fn interaction(&self, data: &Value, session: &mut Session) {
        let (Some(id), Some(token)) = (
            data.get("id").and_then(Value::as_str),
            data.get("token").and_then(Value::as_str),
        ) else {
            return;
        };
        if data.get("type").and_then(Value::as_u64) != Some(COMPONENT_INTERACTION) {
            return;
        }
        if session.seen.iter().any(|seen| seen == id) {
            return;
        }
        session.seen.push_back(id.to_string());
        if session.seen.len() > SEEN_MAX {
            session.seen.pop_front();
        }
        let user = data
            .pointer("/member/user/id")
            .or_else(|| data.pointer("/user/id"))
            .and_then(Value::as_str);
        let in_channel =
            data.get("channel_id").and_then(Value::as_str) == Some(self.discord.channel.as_str());
        let Some(user) = user.filter(|u| in_channel && self.allowed.contains(*u)) else {
            self.refuse(id, token, Reply::NotAllowed).await;
            return;
        };
        let parsed = data
            .pointer("/data/custom_id")
            .and_then(Value::as_str)
            .and_then(parse_callback);
        let Some((kind, blocker_id)) = parsed else {
            self.refuse(id, token, Reply::Invalid).await;
            return;
        };
        let outcome = self
            .handler
            .act(&ChatAction {
                platform: "discord",
                user_id: user.to_string(),
                kind,
                blocker_id,
            })
            .await;
        let reply = Reply::of(outcome, kind);
        tracing::info!(channel = %self.channel, platform = "discord", outcome = ?outcome, "chat.action");
        let original = data.pointer("/message/content").and_then(Value::as_str);
        match (reply.settles(), original) {
            (true, Some(original)) => {
                let content = format!("{original}\n\n{}", reply.text(self.lang));
                let body = json!({
                    "type": RESPONSE_UPDATE,
                    "data": {
                        "content": content,
                        "components": [],
                        "allowed_mentions": { "parse": [] }
                    }
                });
                self.answer(id, token, &body).await;
            }
            _ => self.refuse(id, token, reply).await,
        }
    }
}
