//! The server's end of a runner connection: hello, then frames out and events in.

use std::path::Path;

use saka_core::ids::SessionId;
use saka_core::time::Timestamp;
use saka_ports::{PortError, PortResult, RunnerEvent, RunnerEventKind};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Mutex, mpsc};

use crate::protocol::{self, LogEvent, PROTOCOL_VERSION, RunnerFrame, ServerFrame};

pub struct RunnerConnection {
    writer: Mutex<OwnedWriteHalf>,
    /// The runner's next offset when the connection opened.
    pub next_offset: u64,
}

fn unreachable(session: SessionId, why: &str) -> PortError {
    PortError::retryable(
        "runner_unreachable",
        json!({ "session_id": session, "reason": why }),
    )
}

impl RunnerConnection {
    /// Connects and says hello; events with `offset ≥ from_offset` (replayed, then live)
    /// go to `sink` in order.
    pub async fn connect(
        socket: &Path,
        session: SessionId,
        from_offset: u64,
        server_version: &str,
        sink: mpsc::UnboundedSender<RunnerEvent>,
    ) -> PortResult<RunnerConnection> {
        Self::connect_with(
            socket,
            session,
            PROTOCOL_VERSION,
            from_offset,
            server_version,
            sink,
        )
        .await
    }

    /// As [`RunnerConnection::connect`], speaking `protocol_version` (tests use others).
    pub async fn connect_with(
        socket: &Path,
        session: SessionId,
        protocol_version: u32,
        from_offset: u64,
        server_version: &str,
        sink: mpsc::UnboundedSender<RunnerEvent>,
    ) -> PortResult<RunnerConnection> {
        let stream = UnixStream::connect(socket)
            .await
            .map_err(|e| unreachable(session, &e.to_string()))?;
        let (read, mut writer) = stream.into_split();
        let hello = ServerFrame::Hello {
            protocol_version,
            server_version: server_version.to_string(),
            from_offset,
        };
        writer
            .write_all(protocol::encode(&hello).as_bytes())
            .await
            .map_err(|e| unreachable(session, &e.to_string()))?;
        let mut lines = BufReader::new(read).lines();
        let first = lines
            .next_line()
            .await
            .map_err(|e| unreachable(session, &e.to_string()))?
            .ok_or_else(|| unreachable(session, "closed"))?;
        let next_offset = match protocol::decode::<RunnerFrame>(&first) {
            Ok(RunnerFrame::Hello { next_offset, .. }) => next_offset,
            Ok(RunnerFrame::Error { code }) => {
                return Err(PortError::permanent(code, json!({ "session_id": session })));
            }
            _ => return Err(unreachable(session, "no hello")),
        };
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                match protocol::decode::<RunnerFrame>(&line) {
                    Ok(RunnerFrame::Event(event)) => {
                        if let Some(ev) = to_runner_event(session, event)
                            && sink.send(ev).is_err()
                        {
                            return;
                        }
                    }
                    Ok(RunnerFrame::Error { code }) => {
                        tracing::warn!(session_id = %session, code = %code, "runner reported a protocol error");
                    }
                    _ => {}
                }
            }
        });
        Ok(RunnerConnection {
            writer: Mutex::new(writer),
            next_offset,
        })
    }

    pub async fn send(&self, session: SessionId, frame: &ServerFrame) -> PortResult<()> {
        let mut writer = self.writer.lock().await;
        writer
            .write_all(protocol::encode(frame).as_bytes())
            .await
            .map_err(|e| unreachable(session, &e.to_string()))
    }
}

fn get<T: for<'de> serde::Deserialize<'de>>(data: &Value, key: &str) -> Option<T> {
    data.get(key)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// A logged event as the engine sees it. Kinds this version does not know are dropped.
pub fn to_runner_event(session_id: SessionId, event: LogEvent) -> Option<RunnerEvent> {
    let d = &event.data;
    let text = |key: &str| {
        d.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let kind = match event.kind.as_str() {
        "cli_started" => RunnerEventKind::CliStarted {
            pid: get(d, "pid"),
            cli_version: get(d, "cli_version"),
        },
        "session_init" => RunnerEventKind::SessionInit {
            cli_session_id: text("cli_session_id"),
            model: get(d, "model"),
        },
        "activity" => RunnerEventKind::Activity {
            category: text("category"),
            summary: text("summary"),
        },
        "permission_request" => RunnerEventKind::PermissionRequest {
            request_id: text("request_id"),
            tool: text("tool"),
            input: d.get("input").cloned().unwrap_or(Value::Null),
        },
        "permission_delivered" => RunnerEventKind::PermissionDelivered {
            request_id: text("request_id"),
            behavior: text("behavior"),
        },
        "rate_limit" => RunnerEventKind::RateLimit {
            info: d.get("info").cloned().unwrap_or(Value::Null),
        },
        "turn_ended" => RunnerEventKind::TurnEnded {
            turn: get(d, "turn")?,
            status: text("status"),
            cost_usd_micros: get(d, "cost_usd_micros"),
            input_tokens: get(d, "input_tokens"),
            output_tokens: get(d, "output_tokens"),
            result_summary: get(d, "result_summary"),
        },
        "cli_exited" => RunnerEventKind::CliExited {
            code: get(d, "code"),
            signal: get(d, "signal"),
        },
        "runner_error" => RunnerEventKind::RunnerError {
            code: text("code"),
            params: d.get("params").cloned().unwrap_or(Value::Null),
        },
        _ => return None,
    };
    Some(RunnerEvent {
        session_id,
        offset: event.offset,
        at: Timestamp(event.at),
        kind,
    })
}
