use std::env;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::check::{cargo, pnpm, step};
use crate::metadata;
use crate::tools::{self, Installer};

const SHELL_PACKAGE: &str = "saka-desktop";

pub fn run() -> Result<()> {
    let root = metadata::workspace_root()?;
    let meta = metadata::load()?;
    step("web build", || {
        pnpm(&root, &["install", "--frozen-lockfile"])?;
        pnpm(&root, &["build"])
    })?;
    step("cargo clippy (desktop)", || {
        cargo(
            &root,
            &[
                "clippy",
                "--locked",
                "-p",
                SHELL_PACKAGE,
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        )
    })?;
    step("cargo test (desktop)", || {
        cargo(&root, &["test", "--locked", "-p", SHELL_PACKAGE])
    })?;
    step("cargo tauri build", || {
        let version = tools::pinned(&meta, "tauri-cli")?;
        let target = metadata::target_directory(&meta)?;
        let installer = Installer::new(&target, false);
        let cli = installer.install("tauri-cli", "tauri-cli", &version, "cargo-tauri")?;
        tauri_build(&root, &cli)
    })?;
    println!("desktop-check: ok");
    Ok(())
}

fn tauri_build(root: &Path, cli: &Path) -> Result<()> {
    let bin = cli.parent().context("cargo-tauri has no directory")?;
    let path = env::join_paths(
        std::iter::once(bin.to_path_buf())
            .chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
    )?;
    let status = Command::new(metadata::cargo())
        .args(["tauri", "build", "--debug", "--no-bundle"])
        .current_dir(root.join("desktop").join("src-tauri"))
        .env("PATH", path)
        .status()
        .context("run cargo tauri build")?;
    if !status.success() {
        bail!("cargo tauri build exited with {status}");
    }
    Ok(())
}
