use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use saka_core::ids::SessionId;
use saka_core::time::Timestamp;
use saka_ports::{PortError, PortResult, RunnerEvent, RunnerEventKind};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Mutex, mpsc};

use crate::protocol::{self, LogEvent, RunnerFrame, ServerFrame};

pub struct RunnerConnection {
    writer: Mutex<OwnedWriteHalf>,
    pub next_offset: u64,
    pub runner_version: String,
    last_frame_ms: Arc<AtomicI64>,
    closed: Arc<AtomicBool>,
}

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

fn unreachable(session: SessionId, why: &str) -> PortError {
    PortError::retryable(
        "runner_unreachable",
        json!({ "session_id": session, "reason": why }),
    )
}

impl RunnerConnection {
    pub async fn connect(
        socket: &Path,
        session: SessionId,
        from_offset: u64,
        server_version: &str,
        sink: mpsc::UnboundedSender<RunnerEvent>,
    ) -> PortResult<RunnerConnection> {
        let stream = UnixStream::connect(socket)
            .await
            .map_err(|e| unreachable(session, &e.to_string()))?;
        let (read, mut writer) = stream.into_split();
        let hello = ServerFrame::Hello {
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
        let (next_offset, runner_version) = match protocol::decode::<RunnerFrame>(&first) {
            Ok(RunnerFrame::Hello {
                next_offset,
                runner_version,
                ..
            }) => (next_offset, runner_version),
            Ok(RunnerFrame::Error { code }) => {
                return Err(PortError::permanent(code, json!({ "session_id": session })));
            }
            _ => return Err(unreachable(session, "no hello")),
        };
        let last_frame_ms = Arc::new(AtomicI64::new(wall_ms()));
        let closed = Arc::new(AtomicBool::new(false));
        let (seen, gone) = (Arc::clone(&last_frame_ms), Arc::clone(&closed));
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                seen.store(wall_ms(), Ordering::SeqCst);
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
            gone.store(true, Ordering::SeqCst);
        });
        Ok(RunnerConnection {
            writer: Mutex::new(writer),
            next_offset,
            runner_version,
            last_frame_ms,
            closed,
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn silent_ms(&self) -> i64 {
        (wall_ms() - self.last_frame_ms.load(Ordering::SeqCst)).max(0)
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
            tool: get(d, "tool"),
            is_error: get(d, "is_error"),
        },
        "steer" => RunnerEventKind::Steer {
            n: get(d, "n")?,
            turn: get(d, "turn").unwrap_or_default(),
            text: text("text"),
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
