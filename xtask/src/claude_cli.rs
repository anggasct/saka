use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::metadata;

pub fn memory() -> Result<()> {
    run("Claude memory", "saka-agents", "claude_memory")
}

pub fn mcp() -> Result<()> {
    run("Claude MCP", "saka", "claude_mcp")
}

fn run(name: &str, package: &str, test: &str) -> Result<()> {
    let status = Command::new(metadata::cargo())
        .args(["test", "-p", package, "--test", test])
        .env("SAKA_TEST_CLAUDE", "1")
        .current_dir(metadata::workspace_root()?)
        .status()
        .with_context(|| format!("run the {name} test"))?;
    if !status.success() {
        bail!("{name} test failed: {status}");
    }
    Ok(())
}
