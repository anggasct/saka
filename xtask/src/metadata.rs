//! Workspace layout read from `cargo metadata`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// Path to the cargo binary that started this task.
pub fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

pub fn workspace_root() -> Result<PathBuf> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").context("CARGO_MANIFEST_DIR is not set")?;
    Path::new(&dir)
        .parent()
        .map(Path::to_path_buf)
        .context("xtask has no parent directory")
}

pub fn load() -> Result<Value> {
    let out = Command::new(cargo())
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(workspace_root()?)
        .output()
        .context("run cargo metadata")?;
    if !out.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    serde_json::from_slice(&out.stdout).context("parse cargo metadata")
}

/// Product packages: workspace members under `crates/`, plus `xtask`.
/// Research spikes are left out of every check.
pub fn product_packages(meta: &Value) -> Result<Vec<String>> {
    let root = meta["workspace_root"]
        .as_str()
        .context("metadata has no workspace_root")?;
    let crates_dir = Path::new(root).join("crates");
    let xtask_dir = Path::new(root).join("xtask");
    let members: Vec<&str> = meta["workspace_members"]
        .as_array()
        .context("metadata has no workspace_members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mut names = Vec::new();
    for pkg in meta["packages"]
        .as_array()
        .context("metadata has no packages")?
    {
        let (Some(id), Some(name), Some(manifest)) = (
            pkg["id"].as_str(),
            pkg["name"].as_str(),
            pkg["manifest_path"].as_str(),
        ) else {
            continue;
        };
        let manifest = Path::new(manifest);
        if members.contains(&id)
            && (manifest.starts_with(&crates_dir) || manifest.starts_with(&xtask_dir))
        {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}
