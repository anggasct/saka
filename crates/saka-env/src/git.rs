use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use saka_ports::{GitRemote, PortError};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const LOCAL_TIMEOUT: Duration = Duration::from_secs(60);
const NETWORK_TIMEOUT: Duration = Duration::from_secs(300);
const STDERR_MAX: usize = 500;

pub const EXTRA_ENV: [&str; 8] = [
    "GIT_INDEX_FILE",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "GIT_LITERAL_PATHSPECS",
];

#[derive(Debug, Clone, Copy, Default)]
pub struct Extra<'a> {
    pub env: &'a [(&'a str, &'a str)],
    pub input: Option<&'a [u8]>,
}

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

pub fn failed(op: &str, out: &GitOutput) -> PortError {
    PortError::retryable(
        "git_failed",
        json!({ "op": op, "exit_code": out.code, "stderr": tail(&out.stderr) }),
    )
}

impl Git {
    pub fn new(program: impl Into<PathBuf>, path: impl Into<String>) -> Git {
        Git {
            program: program.into(),
            path: path.into(),
        }
    }

    pub async fn local(&self, dir: &Path, args: &[&str]) -> Result<GitOutput, PortError> {
        self.run(dir, args, None, Extra::default(), LOCAL_TIMEOUT)
            .await
    }

    pub async fn local_with(
        &self,
        dir: &Path,
        args: &[&str],
        extra: Extra<'_>,
    ) -> Result<GitOutput, PortError> {
        self.run(dir, args, None, extra, LOCAL_TIMEOUT).await
    }

    pub async fn remote(
        &self,
        dir: &Path,
        args: &[&str],
        remote: &GitRemote,
    ) -> Result<GitOutput, PortError> {
        self.run(dir, args, Some(remote), Extra::default(), NETWORK_TIMEOUT)
            .await
    }

    async fn run(
        &self,
        dir: &Path,
        args: &[&str],
        remote: Option<&GitRemote>,
        extra: Extra<'_>,
        timeout: Duration,
    ) -> Result<GitOutput, PortError> {
        let op = args.first().copied().unwrap_or("git");
        if let Some((name, _)) = extra.env.iter().find(|(n, _)| !EXTRA_ENV.contains(n)) {
            return Err(PortError::permanent(
                "git_env_refused",
                json!({ "op": op, "name": name }),
            ));
        }
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
            .envs(extra.env.iter().copied())
            .stdin(if extra.input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
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
        let output = tokio::time::timeout(timeout, output_of(cmd, extra.input))
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

async fn output_of(
    mut cmd: Command,
    input: Option<&[u8]>,
) -> std::io::Result<std::process::Output> {
    let mut child = cmd.spawn()?;
    let stdin = child.stdin.take();
    // Feed stdin while the output is drained, so neither pipe can fill up and stall the child.
    let feed = async move {
        if let (Some(mut stdin), Some(bytes)) = (stdin, input) {
            stdin.write_all(bytes).await?;
            stdin.shutdown().await?;
        }
        Ok::<(), std::io::Error>(())
    };
    let (fed, output) = tokio::join!(feed, child.wait_with_output());
    let output = output?;
    match fed {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e),
        _ => Ok(output),
    }
}

pub fn auth_refused(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("authentication failed")
        || s.contains("could not read username")
        || s.contains("returned error: 401")
        || s.contains("returned error: 403")
        || s.contains("invalid username or password")
}
