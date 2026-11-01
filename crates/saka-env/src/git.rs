//! Runs Saka's own `git`: a clean environment (no global or system config, no prompt),
//! and remote credentials only through `GIT_CONFIG_*` variables of the child.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use saka_ports::{GitRemote, PortError};
use serde_json::json;
use tokio::process::Command;

/// Local commands; network commands get [`NETWORK_TIMEOUT`].
const LOCAL_TIMEOUT: Duration = Duration::from_secs(60);
const NETWORK_TIMEOUT: Duration = Duration::from_secs(300);
const STDERR_MAX: usize = 500;

#[derive(Debug, Clone)]
pub struct Git {
    program: PathBuf,
    path: String,
}

#[derive(Debug)]
pub struct GitOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl GitOutput {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }
}

fn tail(text: &str) -> String {
    let chars: Vec<char> = text.trim().chars().collect();
    let start = chars.len().saturating_sub(STDERR_MAX);
    chars[start..].iter().collect()
}

/// A git command that failed; `op` names what Saka was doing.
pub fn failed(op: &str, out: &GitOutput) -> PortError {
    PortError::retryable(
        "git_failed",
        json!({ "op": op, "exit_code": out.code, "stderr": tail(&out.stderr) }),
    )
}

impl Git {
    /// `program` is the `git` binary; `path` is the `PATH` its helpers see.
    pub fn new(program: impl Into<PathBuf>, path: impl Into<String>) -> Git {
        Git {
            program: program.into(),
            path: path.into(),
        }
    }

    /// Runs `git -C <dir> <args>` without network access to a remote.
    pub async fn local(&self, dir: &Path, args: &[&str]) -> Result<GitOutput, PortError> {
        self.run(dir, args, None, LOCAL_TIMEOUT).await
    }

    /// Runs `git -C <dir> <args>` against `remote`, whose config reaches only this child.
    pub async fn remote(
        &self,
        dir: &Path,
        args: &[&str],
        remote: &GitRemote,
    ) -> Result<GitOutput, PortError> {
        self.run(dir, args, Some(remote), NETWORK_TIMEOUT).await
    }

    async fn run(
        &self,
        dir: &Path,
        args: &[&str],
        remote: Option<&GitRemote>,
        timeout: Duration,
    ) -> Result<GitOutput, PortError> {
        let mut cmd = Command::new(&self.program);
        cmd.arg("-C")
            .arg(dir)
            .args(args)
            .env_clear()
            .env("PATH", &self.path)
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "")
            .env("SSH_ASKPASS", "")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(remote) = remote {
            cmd.env("GIT_CONFIG_COUNT", remote.config.len().to_string());
            for (i, (key, value)) in remote.config.iter().enumerate() {
                cmd.env(format!("GIT_CONFIG_KEY_{i}"), key)
                    .env(format!("GIT_CONFIG_VALUE_{i}"), value);
            }
        }
        let op = args.first().copied().unwrap_or("git");
        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| {
                PortError::retryable(
                    "git_timeout",
                    json!({ "op": op, "secs": timeout.as_secs() }),
                )
            })?
            .map_err(|e| {
                PortError::retryable(
                    "git_unavailable",
                    json!({ "op": op, "error": e.to_string() }),
                )
            })?;
        Ok(GitOutput {
            code: output.status.code(),
            stdout: output.stdout,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// The remote refused the credentials (as opposed to a network or repository error).
pub fn auth_refused(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("authentication failed")
        || s.contains("could not read username")
        || s.contains("returned error: 401")
        || s.contains("returned error: 403")
        || s.contains("invalid username or password")
}
