use std::io::{BufRead, Write};

use serde_json::{Value, json};

pub const SCRIPT_VAR: &str = "SAKA_FAKE_CLAUDE";
pub const SECRETS_VAR: &str = "SAKA_FAKE_SECRETS";
pub const ASKED_COMMAND: &str = "make deploy";
pub const TOOL_OUTPUT_SECRET: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

fn out(v: &Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{v}");
    let _ = stdout.flush();
}

fn text(t: &str) -> Value {
    json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": t }] } })
}

fn result(subtype: &str) -> Value {
    json!({
        "type": "result",
        "subtype": subtype,
        "num_turns": 1,
        "total_cost_usd": 0.001,
        "usage": { "input_tokens": 10, "output_tokens": 5 },
        "result": "done",
    })
}

pub fn main() -> i32 {
    let script: Vec<String> = std::env::var(SCRIPT_VAR)
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let has = |a: &str| script.iter().any(|s| s == a);
    let secrets: Vec<String> = std::env::var(SECRETS_VAR)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let resumed = std::env::args().any(|a| a == "--resume");
    let mut turn = 0;
    while let Some(Ok(line)) = lines.next() {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v["type"].as_str() {
            Some("control_request") => {
                out(
                    &json!({ "type": "control_response", "response": { "subtype": "success", "request_id": v["request_id"] } }),
                );
                if v["request"]["subtype"] == "interrupt" {
                    out(&result("error_during_execution"));
                }
            }
            Some("user") => {
                turn += 1;
                if turn == 1 {
                    out(
                        &json!({ "type": "system", "subtype": "init", "session_id": "fake-session", "model": "fake-model" }),
                    );
                }
                if has("exit") {
                    return 1;
                }
                if has("resume-empty") && resumed && turn == 1 {
                    out(
                        &json!({ "type": "result", "subtype": "success", "num_turns": 0, "result": "" }),
                    );
                }
                if has("tools") {
                    out(
                        &json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "Bash", "input": { "command": "go test ./..." } }] } }),
                    );
                    let long = format!("token {TOOL_OUTPUT_SECRET} {}", "x".repeat(800));
                    out(
                        &json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "is_error": true, "content": [{ "type": "text", "text": long }] }] } }),
                    );
                }
                if has("ask") && turn == 1 {
                    out(&json!({
                        "type": "control_request",
                        "request_id": "tool-1",
                        "request": { "subtype": "can_use_tool", "tool_name": "Bash", "input": { "command": ASKED_COMMAND } },
                    }));
                    let answer = loop {
                        match lines.next() {
                            Some(Ok(l)) => {
                                let a: Value = serde_json::from_str(&l).unwrap_or_default();
                                if a["type"] == "control_response"
                                    && a["response"]["request_id"] == "tool-1"
                                {
                                    break a["response"]["response"].clone();
                                }
                            }
                            _ => return 0,
                        }
                    };
                    let detail = match answer["behavior"].as_str() {
                        Some("allow") => format!(
                            "allowed: {}",
                            answer["updatedInput"]["command"]
                                .as_str()
                                .unwrap_or_default()
                        ),
                        _ => format!("denied: {}", answer["message"].as_str().unwrap_or_default()),
                    };
                    out(&text(&detail));
                }
                if has("secrets") {
                    let all = secrets.join(" ");
                    out(&text(&format!("here: {all}")));
                    out(
                        &json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "Bash", "input": { "command": format!("echo {all}") } }] } }),
                    );
                    let mut stdout = std::io::stdout().lock();
                    let _ = writeln!(stdout, "not json {all}");
                    let mut stderr = std::io::stderr().lock();
                    for s in &secrets {
                        let _ = writeln!(stderr, "debug {s}");
                    }
                }
                if has("commit") {
                    let name = format!("fake-{turn}.txt");
                    let _ = std::fs::write(&name, format!("turn {turn}\n"));
                    let git = |args: &[&str]| std::process::Command::new("git").args(args).output();
                    let _ = git(&["add", "-A"]);
                    let _ = git(&["commit", "-q", "-m", "feat: fake change"]);
                }
                if has("hang") {
                    continue;
                }
                out(&result("success"));
            }
            _ => {}
        }
    }
    0
}
