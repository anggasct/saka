//! Claude Code headless: `claude -p` with stream-json in both directions.
//! Permission prompts arrive as `can_use_tool` control requests and are
//! answered by the host policy; interrupt is a control request.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

use crate::agent::{Ev, Event, Proc, policy};

pub struct Claude {
    pub p: Proc,
    pub session: Option<String>,
    worktree: PathBuf,
    req: u64,
}

impl Claude {
    pub async fn start(worktree: &Path, resume: Option<&str>, logs: &Path) -> Result<Claude> {
        let mut args: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--model",
            "haiku",
            "--permission-mode",
            "manual",
            "--permission-prompt-tool",
            "stdio",
            "--setting-sources",
            "project",
            "--strict-mcp-config",
        ]
        .map(String::from)
        .to_vec();
        if let Some(id) = resume {
            args.extend(["--resume".to_string(), id.to_string()]);
        }
        let p = Proc::spawn("claude", worktree, "claude", &args, logs)?;
        let mut c = Claude {
            p,
            session: resume.map(String::from),
            worktree: worktree.to_path_buf(),
            req: 0,
        };
        c.control(json!({ "subtype": "initialize", "hooks": null }))
            .await?;
        Ok(c)
    }

    async fn control(&mut self, request: Value) -> Result<()> {
        self.req += 1;
        let id = format!("saka-{}", self.req);
        self.p
            .send(&json!({ "type": "control_request", "request_id": id, "request": request }))
            .await
    }

    /// A user message. Sent while a turn runs, it is the steering input.
    pub async fn send(&mut self, text: &str) -> Result<()> {
        let session = self.session.clone().unwrap_or_default();
        let msg = json!({
            "type": "user",
            "message": { "role": "user", "content": text },
            "parent_tool_use_id": null,
            "session_id": session,
        });
        self.p.send(&msg).await
    }

    pub async fn interrupt(&mut self) -> Result<()> {
        self.control(json!({ "subtype": "interrupt" })).await
    }

    pub async fn recv(&mut self) -> Option<Event> {
        let (at, v) = self.p.lines.recv().await?;
        let ev = match v["type"].as_str().unwrap_or("") {
            "system" if v["subtype"] == "init" => {
                let id = v["session_id"].as_str().unwrap_or("").to_string();
                self.session = Some(id.clone());
                Ev::Session(id)
            }
            "stream_event" => Ev::Activity("stream"),
            "assistant" => match first_tool(&v) {
                Some(cmd) => Ev::Tool(cmd),
                None => Ev::Activity("response"),
            },
            "user" => Ev::Activity("tool_result"),
            "rate_limit_event" => Ev::RateLimit(v["rate_limit_info"].clone()),
            "result" => Ev::TurnEnd {
                status: v["subtype"].as_str().unwrap_or("").to_string(),
                text: v["result"].as_str().unwrap_or("").to_string(),
                cost: v["total_cost_usd"].as_f64(),
            },
            "control_request" if v["request"]["subtype"] == "can_use_tool" => {
                let r = &v["request"];
                let tool = r["tool_name"].as_str().unwrap_or("").to_string();
                let input = r["input"].clone();
                let verdict = match tool.as_str() {
                    "Bash" => policy::command(input["command"].as_str().unwrap_or("")),
                    "Write" | "Edit" | "MultiEdit" => {
                        policy::path(input["file_path"].as_str().unwrap_or(""), &self.worktree)
                    }
                    "Read" | "Glob" | "Grep" | "LS" | "TodoWrite" => Ok(()),
                    other => Err(format!("denied by policy: tool {other} is not allowed")),
                };
                let response = match &verdict {
                    Ok(()) => json!({ "behavior": "allow", "updatedInput": input }),
                    Err(why) => json!({ "behavior": "deny", "message": why }),
                };
                let reply = json!({
                    "type": "control_response",
                    "response": { "subtype": "success", "request_id": v["request_id"], "response": response },
                });
                if let Err(e) = self.p.send(&reply).await {
                    return Some(Event {
                        at,
                        ev: Ev::Error(format!("approval reply: {e}")),
                    });
                }
                let detail = input["command"]
                    .as_str()
                    .or(input["file_path"].as_str())
                    .unwrap_or("")
                    .to_string();
                Ev::Approval {
                    tool,
                    detail,
                    allowed: verdict.is_ok(),
                }
            }
            "control_response" if v["response"]["subtype"] == "error" => {
                Ev::Error(format!("control: {}", v["response"]["error"]))
            }
            _ => Ev::Activity("other"),
        };
        Some(Event { at, ev })
    }
}

fn first_tool(v: &Value) -> Option<String> {
    v["message"]["content"].as_array()?.iter().find_map(|b| {
        (b["type"] == "tool_use").then(|| {
            let name = b["name"].as_str().unwrap_or("");
            let cmd = b["input"]["command"]
                .as_str()
                .or(b["input"]["file_path"].as_str())
                .unwrap_or("");
            format!("{name}: {cmd}")
        })
    })
}
