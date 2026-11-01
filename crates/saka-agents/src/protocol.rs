//! Runner protocol v1: one JSON object per line over a unix socket. Every frame carries
//! `"v": 1` and `"t": "<type>"`; unknown fields are ignored, unknown types are answered
//! with `error{code:"frame_unknown"}`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const PROTOCOL_VERSION: u32 = 1;

/// Server → runner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerFrame {
    Hello {
        protocol_version: u32,
        server_version: String,
        from_offset: u64,
    },
    Turn {
        turn: u32,
        text: String,
    },
    PermissionAnswer {
        request_id: String,
        /// `allow` or `deny`.
        behavior: String,
        #[serde(default)]
        message: Option<String>,
    },
    Interrupt,
    Shutdown,
}

/// Runner → server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum RunnerFrame {
    Hello {
        protocol_version: u32,
        runner_version: String,
        session_id: String,
        next_offset: u64,
    },
    Event(LogEvent),
    Error {
        code: String,
    },
}

/// One line of `events.ndjson`, and the body of an `event` frame. `data` is redacted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEvent {
    /// 0-based line number.
    pub offset: u64,
    /// Milliseconds since the Unix epoch (UTC).
    pub at: i64,
    pub kind: String,
    pub data: Value,
}

/// Why a line is not a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Malformed {
    /// Valid JSON with a type this version does not know.
    Unknown(String),
    Invalid,
}

/// The frame as one line, ending in `\n`.
pub fn encode<T: Serialize>(frame: &T) -> String {
    let mut value = serde_json::to_value(frame).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("v".into(), Value::from(PROTOCOL_VERSION));
    }
    let mut line = value.to_string();
    line.push('\n');
    line
}

pub fn decode<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, Malformed> {
    let value: Value = serde_json::from_str(line).map_err(|_| Malformed::Invalid)?;
    let Some(t) = value.get("t").and_then(Value::as_str).map(String::from) else {
        return Err(Malformed::Invalid);
    };
    serde_json::from_value(strip_version(value)).map_err(|_| Malformed::Unknown(t))
}

fn strip_version(value: Value) -> Value {
    match value {
        Value::Object(mut map) => {
            map.remove("v");
            Value::Object(map)
        }
        other => other,
    }
}

/// An `event` frame's fields as a map (for tests and logs).
pub fn event_fields(event: &LogEvent) -> Map<String, Value> {
    match serde_json::to_value(event) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn frames_carry_version_and_type() {
        let line = encode(&ServerFrame::Hello {
            protocol_version: 1,
            server_version: "0.1.0".into(),
            from_offset: 0,
        });
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["t"], "hello");
        assert_eq!(v["from_offset"], 0);

        let event = RunnerFrame::Event(LogEvent {
            offset: 3,
            at: 10,
            kind: "turn_ended".into(),
            data: json!({ "turn": 1 }),
        });
        let line = encode(&event);
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            (v["t"].clone(), v["offset"].clone()),
            (json!("event"), json!(3))
        );
        assert_eq!(decode::<RunnerFrame>(&line), Ok(event));
    }

    #[test]
    fn unknown_fields_are_ignored_and_unknown_types_named() {
        let f: ServerFrame =
            decode(r#"{"v":1,"t":"turn","turn":2,"text":"hi","extra":true}"#).unwrap();
        assert_eq!(
            f,
            ServerFrame::Turn {
                turn: 2,
                text: "hi".into()
            }
        );
        assert_eq!(
            decode::<ServerFrame>(r#"{"v":1,"t":"teleport"}"#),
            Err(Malformed::Unknown("teleport".into()))
        );
        assert_eq!(decode::<ServerFrame>("not json"), Err(Malformed::Invalid));
    }
}
