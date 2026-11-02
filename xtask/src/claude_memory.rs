use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::metadata;

pub fn run() -> Result<()> {
    let status = Command::new(metadata::cargo())
        .args(["test", "-p", "saka-agents", "--test", "claude_memory"])
        .env("SAKA_TEST_CLAUDE", "1")
        .current_dir(metadata::workspace_root()?)
        .status()
        .context("run the Claude memory test")?;
    if !status.success() {
        bail!("Claude memory test failed: {status}");
    }
    Ok(())
}
