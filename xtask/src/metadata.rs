use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

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

pub fn product_packages(meta: &Value) -> Result<Vec<String>> {
    members_under(meta, &["crates", "xtask"])
}

pub fn spike_packages(meta: &Value) -> Result<Vec<String>> {
    members_under(meta, &["spikes"])
}

pub fn target_directory(meta: &Value) -> Result<PathBuf> {
    meta["target_directory"]
        .as_str()
        .map(PathBuf::from)
        .context("metadata has no target_directory")
}

fn members_under(meta: &Value, dirs: &[&str]) -> Result<Vec<String>> {
    let root = meta["workspace_root"]
        .as_str()
        .context("metadata has no workspace_root")?;
    let dirs: Vec<PathBuf> = dirs.iter().map(|dir| Path::new(root).join(dir)).collect();
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
        if members.contains(&id) && dirs.iter().any(|dir| manifest.starts_with(dir)) {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}
