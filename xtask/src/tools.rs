use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::metadata;

pub struct Installer {
    tools_dir: PathBuf,
    cargo: String,
    offline: bool,
}

impl Installer {
    pub fn new(target_dir: &Path, offline: bool) -> Self {
        Self {
            tools_dir: target_dir.join("tools"),
            cargo: metadata::cargo(),
            offline,
        }
    }

    fn path(&self, tool: &str, version: &str, bin: &str) -> PathBuf {
        self.tools_dir
            .join(format!("{tool}-{version}"))
            .join("bin")
            .join(bin)
    }

    pub fn install(
        &self,
        tool: &str,
        crate_name: &str,
        version: &str,
        bin: &str,
    ) -> Result<PathBuf> {
        let path = self.path(tool, version, bin);
        if !path.exists() {
            if self.offline {
                bail!(
                    "{tool} {version} is not installed; run cargo xtask supply-chain once with network"
                );
            }
            let root = self.tools_dir.join(format!("{tool}-{version}"));
            let status = Command::new(&self.cargo)
                .args(["install", crate_name, "--locked", "--version"])
                .arg(format!("={version}"))
                .arg("--root")
                .arg(&root)
                .status()
                .with_context(|| format!("run cargo install {crate_name}"))?;
            if !status.success() {
                bail!("cargo install {crate_name} {version} exited with {status}");
            }
        }
        let out = Command::new(&path)
            .arg("--version")
            .output()
            .with_context(|| format!("run {} --version", path.display()))?;
        check_version(tool, &String::from_utf8_lossy(&out.stdout), version)?;
        Ok(path)
    }
}

pub fn pinned(meta: &Value, tool: &str) -> Result<String> {
    let root = meta["workspace_root"].as_str().unwrap_or("the workspace");
    meta["metadata"]["tools"][tool]
        .as_str()
        .map(str::to_string)
        .with_context(|| {
            format!("{root}/Cargo.toml has no string [workspace.metadata.tools] {tool} pin")
        })
}

pub fn check_version(tool: &str, output: &str, version: &str) -> Result<()> {
    if output.split_whitespace().any(|token| token == version) {
        Ok(())
    } else {
        bail!(
            "{tool} reports {:?}, expected version {version}",
            output.trim()
        )
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_pin_comes_from_workspace_metadata_tools() {
        let meta = json!({
            "workspace_root": "/repo",
            "metadata": { "tools": { "cargo-deny": "0.20.2", "broken": 3 } }
        });
        assert_eq!(pinned(&meta, "cargo-deny").unwrap(), "0.20.2");
        let missing = pinned(&meta, "cargo-shear").unwrap_err().to_string();
        assert!(missing.contains("cargo-shear"), "{missing}");
        assert!(pinned(&meta, "broken").is_err());
        assert!(pinned(&json!({}), "cargo-deny").is_err());
    }

    #[test]
    fn the_workspace_pins_every_supply_chain_tool() {
        let meta = metadata::load().unwrap();
        for tool in ["cargo-deny", "cargo-shear", "tauri-cli"] {
            assert!(pinned(&meta, tool).is_ok(), "{tool}");
        }
    }

    #[test]
    fn only_the_exact_pinned_version_is_accepted() {
        assert!(check_version("cargo-deny", "cargo-deny 0.20.2\n", "0.20.2").is_ok());
        assert!(check_version("cargo-deny", "cargo-deny 0.20.1\n", "0.20.2").is_err());
        assert!(check_version("cargo-deny", "cargo-deny 0.20.20\n", "0.20.2").is_err());
        assert!(check_version("cargo-deny", "cargo-deny 10.20.2\n", "0.20.2").is_err());
        let err = check_version("cargo-deny", "", "0.20.2")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cargo-deny") && err.contains("0.20.2"),
            "{err}"
        );
    }

    #[test]
    fn a_tool_installs_under_its_own_versioned_root() {
        let installer = Installer::new(Path::new("/t"), false);
        assert_eq!(
            installer.path("cargo-deny", "0.20.2", "cargo-deny"),
            Path::new("/t/tools/cargo-deny-0.20.2/bin/cargo-deny")
        );
    }

    #[test]
    fn offline_a_missing_tool_fails_without_running_cargo_install() {
        let dir = tempfile::tempdir().unwrap();
        let installer = Installer {
            tools_dir: dir.path().join("tools"),
            cargo: dir.path().join("no-cargo").display().to_string(),
            offline: true,
        };
        let err = installer
            .install("cargo-deny", "cargo-deny", "0.20.2", "cargo-deny")
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "cargo-deny 0.20.2 is not installed; run cargo xtask supply-chain once with network"
        );
        assert!(!dir.path().join("tools").exists());
    }
}
