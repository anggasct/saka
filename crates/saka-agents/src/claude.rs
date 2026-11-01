use serde_json::{Value, json};

pub const SUMMARY_MAX: usize = 500;
pub const RESULT_SUMMARY_MAX: usize = 2000;

#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    Event(&'static str, Value),
    TurnEnded {
        data: Value,
        num_turns: Option<i64>,
    },
    Permission {
        request_id: String,
        tool: String,
        input: Value,
    },
    Nothing,
}

pub fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

pub fn parse(line: &str, turn: u32) -> Output {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Output::Event("activity", json!({ "category": "other", "summary": line }));
    };
    let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
    match v["type"].as_str().unwrap_or_default() {
        "system" if v["subtype"] == "init" => Output::Event(
            "session_init",
            json!({ "cli_session_id": text(&v["session_id"]), "model": v["model"].as_str() }),
        ),
        "assistant" => {
            let blocks = v["message"]["content"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let found = blocks.iter().find_map(|b| match b["type"].as_str() {
                Some("tool_use") => {
                    let input = &b["input"];
                    let detail = input["command"]
                        .as_str()
                        .or(input["file_path"].as_str())
                        .or(input["path"].as_str())
                        .or(input["pattern"].as_str())
                        .unwrap_or_default();
                    let tool = text(&b["name"]);
                    Some(json!({ "category": "tool_use", "summary": format!("{tool}: {detail}"), "tool": tool }))
                }
                Some("thinking") => {
                    Some(json!({ "category": "thinking", "summary": text(&b["thinking"]) }))
                }
                Some("text") => Some(json!({ "category": "text", "summary": text(&b["text"]) })),
                _ => None,
            });
            Output::Event(
                "activity",
                found.unwrap_or_else(|| json!({ "category": "other", "summary": "" })),
            )
        }
        "user" => {
            let blocks = v["message"]["content"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let result = blocks.iter().find(|b| b["type"] == "tool_result");
            Output::Event(
                "activity",
                json!({
                    "category": "tool_result",
                    "summary": result.map(|b| first_text(&b["content"])).unwrap_or_default(),
                    "is_error": result.is_some_and(|b| b["is_error"].as_bool() == Some(true)),
                }),
            )
        }
        "rate_limit_event" => Output::Event("rate_limit", json!({ "info": v["rate_limit_info"] })),
        "result" => {
            let micros = v["total_cost_usd"]
                .as_f64()
                .map(|usd| (usd * 1_000_000.0).round() as i64);
            Output::TurnEnded {
                data: json!({
                    "turn": turn,
                    "status": text(&v["subtype"]),
                    "cost_usd_micros": micros,
                    "input_tokens": v["usage"]["input_tokens"].as_i64(),
                    "output_tokens": v["usage"]["output_tokens"].as_i64(),
                    "result_summary": v["result"].as_str().unwrap_or_default(),
                }),
                num_turns: v["num_turns"].as_i64(),
            }
        }
        "control_request" if v["request"]["subtype"] == "can_use_tool" => Output::Permission {
            request_id: text(&v["request_id"]),
            tool: text(&v["request"]["tool_name"]),
            input: v["request"]["input"].clone(),
        },
        "stream_event" | "control_response" | "control_cancel_request" => Output::Nothing,
        _ => Output::Event("activity", json!({ "category": "other", "summary": "" })),
    }
}

fn first_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .find_map(|i| (i["type"] == "text").then(|| i["text"].as_str().map(String::from))?)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn initialize(request_id: &str) -> Value {
    json!({ "type": "control_request", "request_id": request_id, "request": { "subtype": "initialize", "hooks": null } })
}

pub fn interrupt(request_id: &str) -> Value {
    json!({ "type": "control_request", "request_id": request_id, "request": { "subtype": "interrupt" } })
}

pub fn user_message(text: &str, cli_session_id: Option<&str>) -> Value {
    json!({
        "type": "user",
        "message": { "role": "user", "content": text },
        "parent_tool_use_id": null,
        "session_id": cli_session_id.unwrap_or_default(),
    })
}

pub fn permission_response(
    request_id: &str,
    allow: bool,
    original_input: &Value,
    message: Option<&str>,
) -> Value {
    let response = if allow {
        json!({ "behavior": "allow", "updatedInput": original_input })
    } else {
        json!({ "behavior": "deny", "message": message.unwrap_or("Denied.") })
    };
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_json_lines_map_to_runner_events() {
        assert_eq!(
            parse(
                r#"{"type":"system","subtype":"init","session_id":"abc","model":"haiku"}"#,
                1
            ),
            Output::Event(
                "session_init",
                json!({ "cli_session_id": "abc", "model": "haiku" })
            )
        );
        assert_eq!(
            parse(
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"go test ./..."}}]}}"#,
                1
            ),
            Output::Event(
                "activity",
                json!({ "category": "tool_use", "summary": "Bash: go test ./...", "tool": "Bash" })
            )
        );
        assert_eq!(
            parse(
                r#"{"type":"result","subtype":"success","num_turns":3,"total_cost_usd":0.0123,"usage":{"input_tokens":10,"output_tokens":5},"result":"done"}"#,
                2
            ),
            Output::TurnEnded {
                data: json!({ "turn": 2, "status": "success", "cost_usd_micros": 12300, "input_tokens": 10, "output_tokens": 5, "result_summary": "done" }),
                num_turns: Some(3),
            }
        );
        assert_eq!(
            parse(
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":[{"type":"text","text":"FAIL: TestDone"}]}]}}"#,
                1
            ),
            Output::Event(
                "activity",
                json!({ "category": "tool_result", "summary": "FAIL: TestDone", "is_error": true })
            )
        );
        assert_eq!(
            parse(
                r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok  ./..."}]}}"#,
                1
            ),
            Output::Event(
                "activity",
                json!({ "category": "tool_result", "summary": "ok  ./...", "is_error": false })
            )
        );
        assert_eq!(
            parse(
                r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}"#,
                1
            ),
            Output::Permission {
                request_id: "r1".into(),
                tool: "Bash".into(),
                input: json!({ "command": "ls" })
            }
        );
        assert_eq!(
            parse("plain text", 1),
            Output::Event(
                "activity",
                json!({ "category": "other", "summary": "plain text" })
            )
        );
        assert_eq!(parse(r#"{"type":"stream_event"}"#, 1), Output::Nothing);
    }

    #[test]
    fn permission_answers_carry_the_original_input_or_the_reason() {
        let input = json!({ "command": "ls" });
        assert_eq!(
            permission_response("r1", true, &input, None)["response"]["response"],
            json!({ "behavior": "allow", "updatedInput": { "command": "ls" } })
        );
        assert_eq!(
            permission_response("r1", false, &input, Some("Denied by the operator."))["response"]["response"],
            json!({ "behavior": "deny", "message": "Denied by the operator." })
        );
    }
}
