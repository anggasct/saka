//! Go through its own toolchain switching (Go >= 1.21): `GOTOOLCHAIN=go1.x.y`
//! makes any bootstrap `go` fetch that exact release from the module proxy,
//! checked against the Go checksum database, into GOMODCACHE.

use std::fs::{self, File};
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::Provision;

#[derive(serde::Deserialize)]
pub struct Release {
    version: String,
    stable: bool,
}

pub fn index() -> Result<Vec<Release>> {
    let mut resp = ureq::get("https://go.dev/dl/?mode=json&include=all").call()?;
    let text = resp
        .body_mut()
        .with_config()
        .limit(64 << 20)
        .read_to_string()?;
    Ok(serde_json::from_str(&text)?)
}

/// `1.22` means the newest 1.22.x (what setup-go installs for CI);
/// `1.26.10` means exactly that release.
pub fn resolve(spec: &str, releases: &[Release]) -> Result<String> {
    let spec = spec.trim().trim_start_matches("go");
    let parts: Vec<&str> = spec.split('.').collect();
    if parts.len() < 2 || parts.iter().any(|p| p.parse::<u32>().is_err()) {
        bail!("unsupported go version spec {spec:?}");
    }
    let mut best: Option<(u32, &str)> = None;
    for r in releases.iter().filter(|r| r.stable) {
        let v = r.version.trim_start_matches("go");
        let have: Vec<&str> = v.split('.').collect();
        if have.len() == 3 && parts.iter().zip(&have).all(|(a, b)| a == b) {
            let patch: u32 = have[2].parse().unwrap_or(0);
            if best.is_none_or(|(p, _)| patch > p) {
                best = Some((patch, v));
            }
        }
    }
    best.map(|(_, v)| v.to_string())
        .with_context(|| format!("no go release matches {spec:?}"))
}

pub fn module_dir(gomodcache: &Path, version: &str) -> std::path::PathBuf {
    gomodcache.join(format!(
        "golang.org/toolchain@v0.0.1-go{version}.linux-amd64"
    ))
}

/// Runs the bootstrap `go version` once under a per-version lock, so the
/// download happens before any environment starts using it.
pub fn provision(
    cache: &Path,
    gomodcache: &Path,
    version: &str,
    env: &[(String, String)],
) -> Result<Provision> {
    let locks = cache.join("locks");
    fs::create_dir_all(&locks)?;
    let lock = File::create(locks.join(format!("go{version}.lock")))?;
    let waited = Instant::now();
    lock.lock()?;
    let waited_ms = waited.elapsed().as_millis();

    let bootstrap = bootstrap_version()?;
    if bootstrap == version {
        return Ok(Provision {
            verified: "bootstrap go is this release".into(),
            ..Provision::cached(waited_ms)
        });
    }
    if module_dir(gomodcache, version).exists() {
        return Ok(Provision::cached(waited_ms));
    }
    let started = Instant::now();
    let out = Command::new("go")
        .arg("version")
        .env_clear()
        .envs(env.iter().cloned())
        .output()?;
    if !out.status.success() {
        bail!("go {version}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let zip = gomodcache.join(format!(
        "cache/download/golang.org/toolchain/@v/v0.0.1-go{version}.linux-amd64.zip"
    ));
    Ok(Provision {
        downloaded_bytes: fs::metadata(zip).map(|m| m.len()).unwrap_or(0),
        took_ms: started.elapsed().as_millis(),
        waited_ms,
        verified: "go checksum database (sum.golang.org)".into(),
    })
}

/// The host `go` that does the switching, e.g. `1.26.4`.
pub fn bootstrap_version() -> Result<String> {
    let out = Command::new("go")
        .args(["env", "GOVERSION"])
        .env("GOTOOLCHAIN", "local")
        .output()?;
    Ok(String::from_utf8(out.stdout)?
        .trim()
        .trim_start_matches("go")
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_patch_for_minor() {
        let idx: Vec<Release> = [
            "go1.26.10",
            "go1.26.4",
            "go1.22.12",
            "go1.22.2",
            "go1.27rc1",
        ]
        .iter()
        .map(|v| Release {
            version: v.to_string(),
            stable: !v.contains("rc"),
        })
        .collect();
        assert_eq!(resolve("1.22", &idx).unwrap(), "1.22.12");
        assert_eq!(resolve("1.26.4", &idx).unwrap(), "1.26.4");
        assert_eq!(resolve("1.26", &idx).unwrap(), "1.26.10");
        assert!(resolve("1.27", &idx).is_err());
    }
}
