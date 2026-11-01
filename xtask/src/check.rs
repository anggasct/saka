//! `cargo xtask check`: every gate a change must pass, in a fixed order.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::{deps, metadata};

/// Pure crates share one clippy config; the first one is the source of truth.
const PURE_CLIPPY_CONFIGS: &[&str] = &[
    "crates/saka-core/clippy.toml",
    "crates/saka-spec/clippy.toml",
];

/// Paths the purity fixture must be rejected for.
const PURITY_FIXTURE_EXPECTS: &[&str] = &[
    "std::time::SystemTime::now",
    "std::time::Instant::now",
    "std::fs::read",
    "std::fs::File",
    "std::net::TcpStream",
];

pub fn run() -> Result<()> {
    let root = metadata::workspace_root()?;
    let meta = metadata::load()?;
    let packages = metadata::product_packages(&meta)?;
    let selected: Vec<String> = packages
        .iter()
        .flat_map(|p| ["-p".to_string(), p.clone()])
        .collect();

    step("cargo fmt", || {
        cargo(
            &root,
            &[&["fmt", "--check"], args(&selected).as_slice()].concat(),
        )
    })?;
    step("cargo clippy", || {
        cargo(
            &root,
            &[
                &["clippy", "--locked", "--all-targets"],
                args(&selected).as_slice(),
                &["--", "-D", "warnings"],
            ]
            .concat(),
        )
    })?;
    step("cargo test", || {
        cargo(
            &root,
            &[&["test", "--locked"], args(&selected).as_slice()].concat(),
        )
    })?;
    step("dependency direction", || {
        let violations = deps::check(&meta);
        for v in &violations {
            eprintln!("  {v}");
        }
        if violations.is_empty() {
            Ok(())
        } else {
            bail!("{} violation(s)", violations.len())
        }
    })?;
    step("OpenAPI drift", || {
        cargo(
            &root,
            &[
                "run",
                "--locked",
                "--quiet",
                "-p",
                "saka-api",
                "--example",
                "openapi",
                "--",
                "--check",
            ],
        )?;
        pnpm(&root, &["install", "--frozen-lockfile"])?;
        pnpm(&root, &["api:check"])
    })?;
    step("pnpm lint", || pnpm(&root, &["lint"]))?;
    step("pnpm typecheck", || pnpm(&root, &["typecheck"]))?;
    step("pnpm test", || pnpm(&root, &["test"]))?;
    step("pnpm build", || pnpm(&root, &["build"]))?;
    step("i18n catalog parity", || pnpm(&root, &["i18n:check"]))?;
    step("web app embedded in the binary", || {
        cargo(
            &root,
            &[
                "test",
                "--locked",
                "-p",
                "saka-api",
                "--features",
                "embed-web",
                "--test",
                "spa",
            ],
        )
    })?;
    step("web e2e (Playwright)", || pnpm(&root, &["e2e"]))?;
    step("pure crates share one clippy config", || {
        same_files(&root, PURE_CLIPPY_CONFIGS)
    })?;
    step("pure clippy config rejects I/O", || purity_fixture(&root))?;
    println!("check: ok");
    Ok(())
}

fn args(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn step(name: &str, f: impl FnOnce() -> Result<()>) -> Result<()> {
    println!("==> {name}");
    f().with_context(|| format!("step failed: {name}"))
}

fn cargo(root: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new(metadata::cargo())
        .args(args)
        .current_dir(root)
        .status()
        .with_context(|| format!("run cargo {}", args.join(" ")))?;
    if !status.success() {
        bail!(
            "cargo {} exited with {status}",
            args.first().copied().unwrap_or_default()
        );
    }
    Ok(())
}

/// `pnpm` at the version `web/package.json` pins, through corepack.
fn pnpm(root: &Path, args: &[&str]) -> Result<()> {
    // Run from `web/`: corepack picks the pnpm version from the nearest package.json.
    let status = Command::new("corepack")
        .arg("pnpm")
        .args(args)
        .current_dir(root.join("web"))
        .env("CI", "true")
        .status()
        .context("run corepack pnpm (is corepack installed?)")?;
    if !status.success() {
        bail!(
            "pnpm {} exited with {status}",
            args.first().copied().unwrap_or_default()
        );
    }
    Ok(())
}

fn same_files(root: &Path, files: &[&str]) -> Result<()> {
    let mut first: Option<(&str, Vec<u8>)> = None;
    for file in files {
        let bytes = std::fs::read(root.join(file)).with_context(|| format!("read {file}"))?;
        match &first {
            None => first = Some((file, bytes)),
            Some((name, expected)) if *expected != bytes => bail!("{file} differs from {name}"),
            Some(_) => {}
        }
    }
    Ok(())
}

/// Lints a crate full of forbidden calls with the pure-crate config and requires a
/// rejection for each one, so the config cannot silently stop working.
fn purity_fixture(root: &Path) -> Result<()> {
    let config_dir = root.join("crates/saka-core");
    let out = Command::new(metadata::cargo())
        .args([
            "clippy",
            "--quiet",
            "--manifest-path",
            "xtask/fixtures/impure-core/Cargo.toml",
            "--",
            "-D",
            "warnings",
        ])
        .env("CLIPPY_CONF_DIR", &config_dir)
        .env("CARGO_TARGET_DIR", root.join("target/xtask-fixtures"))
        .current_dir(root)
        .output()
        .context("run clippy on the purity fixture")?;
    if out.status.success() {
        bail!("clippy accepted the purity fixture");
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let missing: Vec<&str> = PURITY_FIXTURE_EXPECTS
        .iter()
        .copied()
        .filter(|path| !stderr.contains(&format!("`{path}`")))
        .collect();
    if !missing.is_empty() {
        bail!("clippy did not reject: {}\n{stderr}", missing.join(", "));
    }
    Ok(())
}
