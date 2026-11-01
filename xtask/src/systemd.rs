use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::metadata;

pub fn run() -> Result<()> {
    let active = Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .context("run systemctl --user")?;
    let state = String::from_utf8_lossy(&active.stdout).trim().to_string();
    if !matches!(state.as_str(), "running" | "degraded") {
        bail!("no user systemd instance (state: {state:?})");
    }
    let status = Command::new(metadata::cargo())
        .args(["test", "-p", "saka", "--test", "runner", "--", "systemd"])
        .env("SAKA_TEST_SYSTEMD", "1")
        .current_dir(metadata::workspace_root()?)
        .status()
        .context("run the systemd test")?;
    if !status.success() {
        bail!("systemd test failed: {status}");
    }
    Ok(())
}
