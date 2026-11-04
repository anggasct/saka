use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::metadata;
use crate::tools::Installer;

const CRATE: &str = "saka-wasm";
const TARGET: &str = "wasm32-unknown-unknown";
const PROFILE: &str = "wasm";
const OUT_DIR: &str = "web/src/wasm/pkg";
const OUT_NAME: &str = "saka_wasm";

pub fn run() -> Result<()> {
    let root = metadata::workspace_root()?;
    let meta = metadata::load()?;
    let target_dir = metadata::target_directory(&meta)?;
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).context("read Cargo.lock")?;
    let version = bindgen_version(&lock)?;
    let cli = Installer::new(&target_dir, false).install(
        "wasm-bindgen",
        "wasm-bindgen-cli",
        &version,
        "wasm-bindgen",
    )?;
    run_tool(
        Command::new(metadata::cargo())
            .args([
                "build",
                "--locked",
                "--profile",
                PROFILE,
                "--target",
                TARGET,
                "-p",
                CRATE,
            ])
            .current_dir(&root),
        "cargo build saka-wasm",
    )?;
    let wasm = target_dir
        .join(TARGET)
        .join(PROFILE)
        .join(format!("{}.wasm", CRATE.replace('-', "_")));
    let out_dir = root.join(OUT_DIR);
    if out_dir.exists() {
        std::fs::remove_dir_all(&out_dir)
            .with_context(|| format!("clear {}", out_dir.display()))?;
    }
    run_tool(
        Command::new(&cli)
            .args(["--target", "web", "--out-name", OUT_NAME, "--out-dir"])
            .arg(&out_dir)
            .arg(&wasm),
        "wasm-bindgen",
    )
}

fn run_tool(cmd: &mut Command, what: &str) -> Result<()> {
    let status = cmd.status().with_context(|| format!("run {what}"))?;
    if !status.success() {
        bail!("{what} exited with {status}");
    }
    Ok(())
}

pub fn bindgen_version(lock: &str) -> Result<String> {
    let mut versions = Vec::new();
    for block in lock.split("[[package]]").skip(1) {
        let field = |key: &str| {
            block.lines().find_map(|line| {
                line.strip_prefix(key)
                    .and_then(|rest| rest.trim().strip_prefix('='))
                    .map(|v| v.trim().trim_matches('"').to_string())
            })
        };
        if field("name").as_deref() == Some("wasm-bindgen")
            && let Some(version) = field("version")
        {
            versions.push(version);
        }
    }
    versions.sort();
    versions.dedup();
    match versions.as_slice() {
        [one] => Ok(one.clone()),
        [] => bail!("Cargo.lock has no wasm-bindgen package"),
        many => bail!(
            "Cargo.lock has more than one wasm-bindgen version ({}); the CLI must match exactly one",
            many.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, version: &str) -> String {
        format!(
            "[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n"
        )
    }

    #[test]
    fn the_cli_version_comes_from_the_one_locked_wasm_bindgen() {
        let lock = format!(
            "version = 4\n\n{}{}{}",
            package("wasm-bindgen-macro", "0.2.99"),
            package("wasm-bindgen", "0.2.130"),
            package("serde", "1.0.0"),
        );
        assert_eq!(bindgen_version(&lock).unwrap(), "0.2.130");
    }

    #[test]
    fn two_locked_versions_or_none_are_refused() {
        let two = format!(
            "{}{}",
            package("wasm-bindgen", "0.2.130"),
            package("wasm-bindgen", "0.2.129"),
        );
        let err = bindgen_version(&two).unwrap_err().to_string();
        assert!(err.contains("0.2.129, 0.2.130"), "{err}");
        assert!(bindgen_version(&package("serde", "1.0.0")).is_err());
    }

    #[test]
    fn a_cli_of_another_version_is_refused() {
        let check = |out| crate::tools::check_version("wasm-bindgen", out, "0.2.130");
        assert!(check("wasm-bindgen 0.2.130\n").is_ok());
        assert!(check("wasm-bindgen 0.2.129\n").is_err());
        assert!(check("wasm-bindgen 0.2.1300\n").is_err());
        assert!(check("").is_err());
    }

    #[test]
    fn the_locked_workspace_has_exactly_one_wasm_bindgen() {
        let root = metadata::workspace_root().unwrap();
        let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
        assert!(bindgen_version(&lock).is_ok());
    }
}
