use std::env;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::check::{cargo, pnpm, step};
use crate::metadata;
use crate::tools::{self, Installer};
use crate::wasm;

const SHELL_PACKAGE: &str = "saka-desktop";

pub fn run() -> Result<()> {
    let root = metadata::workspace_root()?;
    let meta = metadata::load()?;
    step("saka-wasm bindings", wasm::run)?;
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
    let path = with_tool_on_path(cli)?;
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

pub fn bundle(debug: bool) -> Result<()> {
    let root = metadata::workspace_root()?;
    let meta = metadata::load()?;
    let target = metadata::target_directory(&meta)?;
    let profile = if debug { "debug" } else { "release" };
    step("saka-wasm bindings", wasm::run)?;
    step("web build", || {
        pnpm(&root, &["install", "--frozen-lockfile"])?;
        pnpm(&root, &["build"])
    })?;
    step("saka program", || {
        let mut args = vec!["build", "--locked", "-p", "saka"];
        if !debug {
            args.push("--release");
        }
        cargo(&root, &args)?;
        stage_sidecar(&root, &target.join(profile).join("saka"))
    })?;
    step("cargo tauri build", || {
        let version = tools::pinned(&meta, "tauri-cli")?;
        let installer = Installer::new(&target, false);
        let cli = installer.install("tauri-cli", "tauri-cli", &version, "cargo-tauri")?;
        let updater = updater_config(
            env::var("SAKA_UPDATER_PUBKEY").ok().as_deref(),
            env::var("SAKA_UPDATER_ENDPOINT").ok().as_deref(),
        )?;
        let extra = match updater {
            Some(config) => {
                let path = target.join("desktop-updater.conf.json");
                std::fs::write(&path, serde_json::to_vec(&config)?)?;
                Some(path)
            }
            None => None,
        };
        let bundles = bundle_targets(env::consts::OS)?;
        tauri_bundle(&root, &cli, debug, bundles, extra.as_deref())
    })?;
    println!("desktop-bundle: ok");
    Ok(())
}

pub fn bundle_targets(os: &str) -> Result<&'static str> {
    match os {
        "linux" => Ok("deb,appimage"),
        "macos" => Ok("dmg"),
        other => bail!("no desktop bundle is defined for {other}"),
    }
}

pub fn updater_config(pubkey: Option<&str>, endpoint: Option<&str>) -> Result<Option<Value>> {
    let pubkey = pubkey.map(str::trim).unwrap_or_default();
    if pubkey.is_empty() {
        return Ok(None);
    }
    let endpoint = endpoint.map(str::trim).unwrap_or_default();
    if !endpoint.starts_with("https://") {
        bail!("SAKA_UPDATER_ENDPOINT must be an https URL when SAKA_UPDATER_PUBKEY is set");
    }
    Ok(Some(json!({
        "bundle": { "createUpdaterArtifacts": true },
        "plugins": { "updater": { "pubkey": pubkey, "endpoints": [endpoint] } }
    })))
}

pub fn host_triple(rustc_version: &str) -> Result<String> {
    rustc_version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(|triple| triple.trim().to_string())
        .context("rustc -vV has no host line")
}

fn stage_sidecar(root: &Path, built: &Path) -> Result<()> {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .context("run rustc -vV")?;
    let triple = host_triple(&String::from_utf8_lossy(&out.stdout))?;
    let dir = root.join("desktop").join("src-tauri").join("binaries");
    std::fs::create_dir_all(&dir)?;
    std::fs::copy(built, dir.join(format!("saka-{triple}")))
        .with_context(|| format!("copy {}", built.display()))?;
    Ok(())
}

fn tauri_bundle(
    root: &Path,
    cli: &Path,
    debug: bool,
    bundles: &str,
    updater: Option<&Path>,
) -> Result<()> {
    let shell = root.join("desktop").join("src-tauri");
    let mut command = Command::new(metadata::cargo());
    command
        .args(["tauri", "build", "--config"])
        .arg(shell.join("tauri.bundle.conf.json"))
        .args(["--bundles", bundles]);
    if let Some(path) = updater {
        command.arg("--config").arg(path);
    }
    if debug {
        command.arg("--debug");
    }
    let status = command
        .current_dir(&shell)
        .env("PATH", with_tool_on_path(cli)?)
        .status()
        .context("run cargo tauri build")?;
    if !status.success() {
        bail!("cargo tauri build exited with {status}");
    }
    Ok(())
}

fn with_tool_on_path(cli: &Path) -> Result<std::ffi::OsString> {
    let bin = cli.parent().context("cargo-tauri has no directory")?;
    Ok(env::join_paths(std::iter::once(bin.to_path_buf()).chain(
        env::split_paths(&env::var_os("PATH").unwrap_or_default()),
    ))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_updater_stays_off_without_a_public_key() {
        assert_eq!(updater_config(None, Some("https://u.test")).unwrap(), None);
        assert_eq!(updater_config(Some("  "), None).unwrap(), None);
    }

    #[test]
    fn a_public_key_needs_an_https_endpoint_and_turns_the_updater_on() {
        assert!(updater_config(Some("key"), None).is_err());
        assert!(updater_config(Some("key"), Some("http://u.test")).is_err());
        let config = updater_config(Some(" key "), Some("https://u.test/latest.json"))
            .unwrap()
            .unwrap();
        assert_eq!(config["plugins"]["updater"]["pubkey"], "key");
        assert_eq!(
            config["plugins"]["updater"]["endpoints"],
            json!(["https://u.test/latest.json"])
        );
        assert_eq!(config["bundle"]["createUpdaterArtifacts"], true);
    }

    #[test]
    fn bundles_are_defined_for_linux_and_macos_only() {
        assert_eq!(bundle_targets("linux").unwrap(), "deb,appimage");
        assert_eq!(bundle_targets("macos").unwrap(), "dmg");
        assert!(bundle_targets("windows").is_err());
    }

    const WORKFLOW: &str = include_str!("../../.github/workflows/desktop.yml");
    const ALLOWED_ACTIONS: &[&str] = &[
        "actions/checkout",
        "actions/cache",
        "actions/upload-artifact",
        "Swatinem/rust-cache",
    ];
    const PUBLISHING: &[&str] = &[
        "release",
        "publish",
        "gh-pages",
        "pages",
        "docker push",
        "twine",
        "deploy",
    ];

    fn workflow() -> saphyr::Yaml<'static> {
        use saphyr::LoadableYamlNode;
        saphyr::Yaml::load_from_str(WORKFLOW)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    }

    #[test]
    fn the_desktop_workflow_is_manual_read_only_and_builds_both_systems() {
        let doc = workflow();
        let triggers = doc["on"].as_mapping().unwrap();
        assert_eq!(triggers.len(), 1);
        assert!(doc["on"].as_mapping_get("workflow_dispatch").is_some());
        let permissions = doc["permissions"].as_mapping().unwrap();
        assert_eq!(permissions.len(), 1);
        assert_eq!(doc["permissions"]["contents"].as_str(), Some("read"));
        let matrix = doc["jobs"]["bundle"]["strategy"]["matrix"]["include"]
            .as_sequence()
            .unwrap();
        let systems: Vec<&str> = matrix
            .iter()
            .filter_map(|m| m.as_mapping_get("os").and_then(|v| v.as_str()))
            .collect();
        assert!(systems.iter().any(|os| os.starts_with("ubuntu")));
        assert!(systems.iter().any(|os| os.starts_with("macos")));
    }

    #[test]
    fn the_desktop_workflow_has_no_publishing_step_and_only_uploads_artifacts() {
        let doc = workflow();
        let steps = doc["jobs"]["bundle"]["steps"].as_sequence().unwrap();
        let mut uploads = 0;
        for step in steps {
            if let Some(action) = step.as_mapping_get("uses").and_then(|v| v.as_str()) {
                let name = action.split('@').next().unwrap();
                assert!(ALLOWED_ACTIONS.contains(&name), "{action}");
                uploads += usize::from(name == "actions/upload-artifact");
            }
            if let Some(command) = step.as_mapping_get("run").and_then(|v| v.as_str()) {
                for word in PUBLISHING {
                    assert!(
                        !command.to_lowercase().contains(word),
                        "{word} in {command}"
                    );
                }
            }
        }
        assert_eq!(uploads, 1);
    }

    #[test]
    fn the_desktop_workflow_carries_no_key_material() {
        assert!(!WORKFLOW.contains("BEGIN"));
        assert!(!WORKFLOW.contains("untrusted comment"));
        for line in WORKFLOW
            .lines()
            .filter(|l| l.contains("SIGNING_PRIVATE_KEY"))
        {
            assert!(line.contains("${{ secrets."), "{line}");
        }
    }

    #[test]
    fn the_host_triple_comes_from_the_host_line() {
        let text = "rustc 1.99.1\nbinary: rustc\nhost: aarch64-apple-darwin\nrelease: 1.99.1\n";
        assert_eq!(host_triple(text).unwrap(), "aarch64-apple-darwin");
        assert!(host_triple("rustc 1.99.1\n").is_err());
    }
}
