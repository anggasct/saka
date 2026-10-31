//! Codex `app-server`: JSON-RPC over stdio. Approvals are server requests
//! the host must answer; steer and interrupt address the active turn.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::agent::{Ev, Event, Proc, policy};

pub const MODEL: &str = "gpt-5.6-luna";

pub struct Codex {
    pub p: Proc,
    pub thread: Option<String>,
    turn: Option<String>,
    worktree: PathBuf,
    next_id: u64,
    pending: HashMap<u64, &'static str>,
    last_text: String,
}

impl Codex {
    pub async fn start(worktree: &Path, resume: Option<&str>, logs: &Path) -> Result<Codex> {
        let p = Proc::spawn(
            "codex",
            worktree,
            "codex",
            &["app-server".to_string()],
            logs,
        )?;
        let mut c = Codex {
            p,
            thread: None,
            turn: None,
            worktree: worktree.to_path_buf(),
            next_id: 0,
            pending: HashMap::new(),
            last_text: String::new(),
        };
        let init = c
            .request("initialize", json!({ "clientInfo": { "name": "saka-s1", "title": "Saka S1", "version": "0.0.1" }, "capabilities": null }))
            .await?;
        c.wait_for(init).await?;
        c.p.send(&json!({ "method": "initialized" })).await?;
        let params = json!({
            "threadId": resume,
            "cwd": worktree,
            "model": MODEL,
            "approvalPolicy": "untrusted",
            "sandbox": "danger-full-access",
        });
        let id = match resume {
            Some(_) => c.request("thread/resume", params).await?,
            None => c.request("thread/start", params).await?,
        };
        let result = c.wait_for(id).await?;
        c.thread = result["thread"]["id"].as_str().map(String::from);
        c.thread.as_ref().context("no thread id")?;
        Ok(c)
    }

    async fn request(&mut self, method: &'static str, params: Value) -> Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        self.pending.insert(id, method);
        self.p
            .send(&json!({ "id": id, "method": method, "params": params }))
            .await?;
        Ok(id)
    }

    /// Startup only: read until the response to `id`, dropping notifications.
    async fn wait_for(&mut self, id: u64) -> Result<Value> {
        loop {
            let (_, v) = tokio::time::timeout(Duration::from_secs(60), self.p.lines.recv())
                .await
                .context("app-server did not answer")?
                .context("app-server exited")?;
            if v["id"].as_u64() == Some(id) && v.get("method").is_none() {
                self.pending.remove(&id);
                if let Some(e) = v.get("error") {
                    bail!("request {id}: {e}");
                }
                return Ok(v["result"].clone());
            }
        }
    }

    fn input(text: &str) -> Value {
        json!([{ "type": "text", "text": text, "text_elements": [] }])
    }

    pub async fn send(&mut self, text: &str) -> Result<()> {
        let params =
            json!({ "threadId": self.thread, "input": Self::input(text), "effort": "low" });
        self.request("turn/start", params).await.map(|_| ())
    }

    pub async fn steer(&mut self, text: &str) -> Result<()> {
        let params = json!({ "threadId": self.thread, "input": Self::input(text), "expectedTurnId": self.turn });
        self.request("turn/steer", params).await.map(|_| ())
    }

    pub async fn interrupt(&mut self) -> Result<()> {
        let params = json!({ "threadId": self.thread, "turnId": self.turn });
        self.request("turn/interrupt", params).await.map(|_| ())
    }

    pub async fn recv(&mut self) -> Option<Event> {
        let (at, v) = self.p.lines.recv().await?;
        let Some(method) = v["method"].as_str() else {
            // Response to one of our requests.
            let id = v["id"].as_u64().unwrap_or(0);
            let method = self.pending.remove(&id).unwrap_or("");
            if let Some(e) = v.get("error") {
                return Some(Event {
                    at,
                    ev: Ev::Error(format!("{method}: {e}")),
                });
            }
            if method == "turn/start"
                && let Some(t) = v["result"]["turn"]["id"].as_str()
            {
                self.turn = Some(t.to_string());
            }
            return Some(Event {
                at,
                ev: Ev::Activity("response"),
            });
        };
        let params = &v["params"];
        if v.get("id").is_some() {
            let ev = self.answer(method, &v).await;
            return Some(Event { at, ev });
        }
        let ev = match method {
            "turn/started" => {
                self.turn = params["turn"]["id"].as_str().map(String::from);
                Ev::Activity("turn")
            }
            "turn/completed" => {
                let turn = &params["turn"];
                self.turn = None;
                let text = turn["items"]
                    .as_array()
                    .and_then(|items| items.iter().rev().find(|i| i["type"] == "agentMessage"))
                    .and_then(|i| i["text"].as_str())
                    .map(String::from)
                    .unwrap_or_else(|| self.last_text.clone());
                Ev::TurnEnd {
                    status: turn["status"].as_str().unwrap_or("").to_string(),
                    text,
                    cost: None,
                }
            }
            "item/started" if params["item"]["type"] == "commandExecution" => {
                Ev::Tool(params["item"]["command"].as_str().unwrap_or("").to_string())
            }
            "item/completed" if params["item"]["type"] == "agentMessage" => {
                self.last_text = params["item"]["text"].as_str().unwrap_or("").to_string();
                Ev::Activity("response")
            }
            "item/started" | "item/completed" => Ev::Activity("item"),
            "thread/tokenUsage/updated" => Ev::Usage(params["tokenUsage"].clone()),
            "account/rateLimits/updated" => Ev::RateLimit(params["rateLimits"].clone()),
            "error" => Ev::Error(params.to_string()),
            m if m.contains("delta") || m.contains("Delta") => Ev::Activity("stream"),
            _ => Ev::Activity("other"),
        };
        Some(Event { at, ev })
    }

    async fn answer(&mut self, method: &str, v: &Value) -> Ev {
        let params = &v["params"];
        let (tool, detail, verdict) = match method {
            "item/commandExecution/requestApproval" => {
                let actions: Vec<String> = params["commandActions"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x["command"].as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let detail = if actions.is_empty() {
                    params["command"].as_str().unwrap_or("").to_string()
                } else {
                    actions.join(" && ")
                };
                let verdict = policy::command(&detail);
                ("command".to_string(), detail, verdict)
            }
            "item/fileChange/requestApproval" => {
                let root = params["grantRoot"].as_str().unwrap_or("");
                let verdict = if root.is_empty() {
                    Ok(())
                } else {
                    policy::path(root, &self.worktree)
                };
                ("fileChange".to_string(), root.to_string(), verdict)
            }
            other => (
                other.to_string(),
                String::new(),
                Err("not supported by this host".to_string()),
            ),
        };
        let reply = match (method, &verdict) {
            (
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval",
                Ok(()),
            ) => {
                json!({ "id": v["id"], "result": { "decision": "accept" } })
            }
            (
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval",
                Err(_),
            ) => {
                json!({ "id": v["id"], "result": { "decision": "decline" } })
            }
            _ => {
                json!({ "id": v["id"], "error": { "code": -32601, "message": "not supported by this host" } })
            }
        };
        if let Err(e) = self.p.send(&reply).await {
            return Ev::Error(format!("approval reply: {e}"));
        }
        Ev::Approval {
            tool,
            detail,
            allowed: verdict.is_ok(),
        }
    }
}
