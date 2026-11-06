use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use saka_core::diagnostics::{AgentCli, AgentLogin, FoundCli};
use serde::Deserialize;
use tokio::process::Command;

use crate::launch::ClaudeSettings;

pub const STATUS_TIMEOUT: Duration = Duration::from_secs(20);
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    logged_in: bool,
    #[serde(default)]
    auth_method: Option<String>,
}

pub fn search_path(settings: &ClaudeSettings) -> String {
    settings.path.join(":")
}

fn isolated(program: &Path, settings: &ClaudeSettings) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", search_path(settings))
        .env("HOME", &settings.account_dir)
        .env("CLAUDE_CONFIG_DIR", &settings.account_dir)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    command
}

pub async fn version(program: &Path, settings: &ClaudeSettings) -> Option<String> {
    let mut command = isolated(program, settings);
    command.arg("--version").stderr(Stdio::null());
    let output = tokio::time::timeout(VERSION_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace().next().map(String::from)
}

pub async fn cli(settings: &ClaudeSettings) -> AgentCli {
    let found = match settings.program() {
        Some(program) => Some(FoundCli {
            path: program.to_string_lossy().into_owned(),
            version: version(&program, settings).await,
        }),
        None => None,
    };
    AgentCli {
        command: settings.command.clone(),
        path_dirs: settings.path.clone(),
        found,
    }
}

pub async fn login(settings: &ClaudeSettings, timeout: Duration) -> AgentLogin {
    let Some(program) = settings.program() else {
        return AgentLogin::CliNotFound {
            command: settings.command.clone(),
        };
    };
    let mut command = isolated(&program, settings);
    command
        .args(["auth", "status", "--json"])
        .stderr(Stdio::null());
    let output = match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(_)) | Err(_) => return AgentLogin::Unavailable { exit_code: None },
    };
    let exit_code = output.status.code();
    match (
        exit_code,
        serde_json::from_slice::<AuthStatus>(&output.stdout),
    ) {
        (Some(0), Ok(s)) if s.logged_in => AgentLogin::LoggedIn {
            auth_method: s.auth_method.unwrap_or_else(|| "unknown".into()),
        },
        (Some(1), Ok(s)) if !s.logged_in => AgentLogin::NotLoggedIn,
        _ => AgentLogin::Unavailable { exit_code },
    }
}
