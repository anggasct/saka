//! What the runtime sees of any agent CLI: one process in its own cgroup
//! scope, a stream of normalized events, and a few controls.

use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use crate::claude::Claude;
use crate::codex::Codex;

pub struct Event {
    pub at: Instant,
    pub ev: Ev,
}

#[derive(Debug, Clone)]
pub enum Ev {
    Session(String),
    Activity(&'static str),
    /// A tool or command started (Claude tool_use, Codex commandExecution).
    Tool(String),
    Approval {
        tool: String,
        detail: String,
        allowed: bool,
    },
    TurnEnd {
        status: String,
        text: String,
        cost: Option<f64>,
    },
    Usage(Value),
    RateLimit(Value),
    Error(String),
}

pub struct Proc {
    pub unit: String,
    child: Child,
    stdin: ChildStdin,
    pub lines: mpsc::UnboundedReceiver<(Instant, Value)>,
}

static SEQ: AtomicU64 = AtomicU64::new(0);

impl Proc {
    /// Starts `program` in a transient user scope (its own cgroup) with a
    /// minimal environment instead of the operator's.
    pub fn spawn(
        label: &str,
        cwd: &Path,
        program: &str,
        args: &[String],
        logs: &Path,
    ) -> Result<Proc> {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let unit = format!("saka-s1-{label}-{}-{seq}", std::process::id());
        let home = std::env::var("HOME")?;
        let vars = [
            format!("HOME={home}"),
            format!("USER={}", std::env::var("USER").unwrap_or_default()),
            "LANG=C.UTF-8".to_string(),
            "TERM=dumb".to_string(),
            format!("PATH={home}/.local/bin:/usr/local/bin:/usr/bin:/bin"),
        ];
        let stderr = std::fs::File::create(logs.join(format!("{unit}.stderr")))?;
        let mut child = Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "--collect"])
            .arg(format!("--unit={unit}"))
            .args([
                "-p",
                "MemoryMax=2G",
                "-p",
                "CPUQuota=200%",
                "-p",
                "TasksMax=256",
            ])
            .args(["--", "env", "-i"])
            .args(&vars)
            .arg(program)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .context("spawn agent")?;
        let stdin = child.stdin.take().context("stdin")?;
        let stdout = child.stdout.take().context("stdout")?;
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && tx.send((Instant::now(), v)).is_err()
                {
                    break;
                }
            }
        });
        Ok(Proc {
            unit,
            child,
            stdin,
            lines: rx,
        })
    }

    pub async fn send(&mut self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// SIGKILL every process in the scope, the CLI and whatever it spawned,
    /// through the cgroup v2 `cgroup.kill` file.
    pub async fn kill_scope(&mut self) -> Result<bool> {
        let cg = control_group(&self.unit).await;
        let written = !cg.is_empty()
            && std::fs::write(format!("/sys/fs/cgroup{cg}/cgroup.kill"), "1").is_ok();
        if !written {
            let _ = Command::new("systemctl")
                .args(["--user", "kill", "--signal=SIGKILL"])
                .arg(format!("{}.scope", self.unit))
                .status()
                .await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        Ok(written)
    }

    /// Processes still in the scope, as (pid, command line).
    pub async fn procs(&self) -> Vec<(u32, String)> {
        let cg = control_group(&self.unit).await;
        if cg.is_empty() {
            return vec![];
        }
        let pids =
            std::fs::read_to_string(format!("/sys/fs/cgroup{cg}/cgroup.procs")).unwrap_or_default();
        pids.lines()
            .filter_map(|p| p.trim().parse::<u32>().ok())
            .filter_map(|pid| {
                let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
                Some((pid, String::from_utf8_lossy(&raw).replace('\0', " ")))
            })
            .collect()
    }

    /// Kills processes in the scope whose command line contains `needle`;
    /// returns how many were found.
    pub async fn kill_matching(&self, needle: &str) -> usize {
        let hits: Vec<u32> = self
            .procs()
            .await
            .into_iter()
            .filter(|(_, c)| c.contains(needle))
            .map(|(p, _)| p)
            .collect();
        for pid in &hits {
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status()
                .await;
        }
        hits.len()
    }

    pub async fn scope_active(&self) -> bool {
        let out = Command::new("systemctl")
            .args(["--user", "is-active"])
            .arg(format!("{}.scope", self.unit))
            .output()
            .await;
        matches!(out, Ok(o) if String::from_utf8_lossy(&o.stdout).trim() == "active")
    }
}

async fn control_group(unit: &str) -> String {
    let out = Command::new("systemctl")
        .args(["--user", "show", "-p", "ControlGroup", "--value"])
        .arg(format!("{unit}.scope"))
        .output()
        .await;
    out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub enum Agent {
    Claude(Claude),
    Codex(Codex),
}

impl Agent {
    pub async fn start(
        cli: &str,
        worktree: &Path,
        resume: Option<&str>,
        logs: &Path,
    ) -> Result<Agent> {
        Ok(match cli {
            "claude" => Agent::Claude(Claude::start(worktree, resume, logs).await?),
            _ => Agent::Codex(Codex::start(worktree, resume, logs).await?),
        })
    }

    pub async fn send(&mut self, text: &str) -> Result<()> {
        match self {
            Agent::Claude(a) => a.send(text).await,
            Agent::Codex(a) => a.send(text).await,
        }
    }

    pub async fn steer(&mut self, text: &str) -> Result<()> {
        match self {
            Agent::Claude(a) => a.send(text).await,
            Agent::Codex(a) => a.steer(text).await,
        }
    }

    pub async fn interrupt(&mut self) -> Result<()> {
        match self {
            Agent::Claude(a) => a.interrupt().await,
            Agent::Codex(a) => a.interrupt().await,
        }
    }

    pub async fn recv(&mut self) -> Option<Event> {
        match self {
            Agent::Claude(a) => a.recv().await,
            Agent::Codex(a) => a.recv().await,
        }
    }

    pub fn session(&self) -> Option<String> {
        match self {
            Agent::Claude(a) => a.session.clone(),
            Agent::Codex(a) => a.thread.clone(),
        }
    }

    pub fn proc(&mut self) -> &mut Proc {
        match self {
            Agent::Claude(a) => &mut a.p,
            Agent::Codex(a) => &mut a.p,
        }
    }
}

/// Host-side approval policy: allow a short list of commands and file writes
/// inside the worktree; everything else is denied and the agent is told why.
pub mod policy {
    use std::path::Path;

    const ALLOWED: &[&str] = &[
        "git",
        "sleep",
        "mkdir",
        "echo",
        "cat",
        "ls",
        "printf",
        "touch",
        "pwd",
        "test",
        "true",
        "cd",
        "tee",
        "head",
        "tail",
        "wc",
        "sed",
        "grep",
        "[",
        "./tick.sh",
    ];

    pub fn command(cmd: &str) -> Result<(), String> {
        let inner = unwrap_shell(cmd);
        for seg in inner.split([';', '\n', '|', '&']) {
            let mut words = seg
                .split_whitespace()
                .skip_while(|w| w.contains('=') && !w.starts_with('-'));
            let Some(first) = words.next() else { continue };
            let first = first.trim_start_matches(['(', '{']);
            // `2>&1` splits into `2>` and `1`: redirections, not commands.
            if first
                .chars()
                .all(|c| c.is_ascii_digit() || c == '>' || c == '<')
            {
                continue;
            }
            if !ALLOWED.contains(&first) {
                return Err(format!(
                    "denied by policy: `{first}` is not allowed in this environment"
                ));
            }
        }
        Ok(())
    }

    pub fn path(p: &str, worktree: &Path) -> Result<(), String> {
        let path = Path::new(p);
        if path.is_relative() || path.starts_with(worktree) {
            Ok(())
        } else {
            Err(format!("denied by policy: {p} is outside the worktree"))
        }
    }

    /// `/usr/bin/zsh -lc 'touch x'` → `touch x`.
    fn unwrap_shell(cmd: &str) -> String {
        for flag in [" -lc ", " -c "] {
            if let Some(i) = cmd.find(flag) {
                let rest = cmd[i + flag.len()..].trim();
                return rest.trim_matches(['\'', '"']).to_string();
            }
        }
        cmd.to_string()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn commands() {
            assert!(command("git add -A && git commit -m x && git push origin t").is_ok());
            assert!(
                command("/usr/bin/zsh -lc 'mkdir -p notes && printf \"x\" > notes/a.md'").is_ok()
            );
            assert!(command("curl -s https://example.com").is_err());
            assert!(command("echo hi | nc host 1").is_err());
            assert!(command("GIT_TRACE=1 git status").is_ok());
            assert!(command("git push origin t 2>&1").is_ok());
        }
    }
}
