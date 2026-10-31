//! Node.js from the official nodejs.org downloads, verified against
//! SHASUMS256.txt and unpacked once into the shared cache.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::Provision;

const DIST: &str = "https://nodejs.org/dist";

#[derive(serde::Deserialize)]
pub struct Release {
    version: String,
    lts: serde_json::Value,
}

pub fn index() -> Result<Vec<Release>> {
    let mut resp = ureq::get(&format!("{DIST}/index.json")).call()?;
    let text = resp
        .body_mut()
        .with_config()
        .limit(32 << 20)
        .read_to_string()?;
    Ok(serde_json::from_str(&text)?)
}

/// Resolves what a repo declares (`22`, `v24`, `24.x`, `>=22`, `lts/*`,
/// `22.11.0`) to one release, the way setup-node does: the newest release
/// that matches the given prefix.
pub fn resolve(spec: &str, releases: &[Release]) -> Result<String> {
    let spec = spec.trim();
    if let Some(name) = spec.strip_prefix("lts/") {
        let hit = releases.iter().find(|r| match r.lts.as_str() {
            Some(codename) => name == "*" || codename.eq_ignore_ascii_case(name),
            None => false,
        });
        return hit
            .map(|r| r.version[1..].to_string())
            .context("no matching LTS release");
    }
    // A lower bound declares the minimum: use that major, like the CI matrix does.
    let spec = spec.strip_prefix(">=").unwrap_or(spec).trim();
    let parts: Vec<&str> = spec
        .trim_start_matches('v')
        .split('.')
        .take_while(|p| *p != "x" && *p != "*")
        .collect();
    if parts.is_empty() || parts.iter().any(|p| p.parse::<u32>().is_err()) {
        bail!("unsupported node version spec {spec:?}");
    }
    // index.json is ordered newest first.
    releases
        .iter()
        .map(|r| &r.version[1..])
        .find(|v| {
            let have: Vec<&str> = v.split('.').collect();
            parts.iter().zip(&have).all(|(want, got)| want == got)
        })
        .map(str::to_string)
        .with_context(|| format!("no node release matches {spec:?}"))
}

pub fn dir(cache: &Path, version: &str) -> PathBuf {
    cache.join("toolchains/node").join(version)
}

/// Makes `<cache>/toolchains/node/<version>` available. Concurrent callers
/// for the same version wait on one lock, so only the first one downloads.
pub fn provision(cache: &Path, version: &str) -> Result<Provision> {
    let root = cache.join("toolchains/node");
    fs::create_dir_all(&root)?;
    let lock = File::create(root.join(format!(".{version}.lock")))?;
    let waited = Instant::now();
    lock.lock()?;
    let waited_ms = waited.elapsed().as_millis();

    let target = dir(cache, version);
    if target.join(".complete").exists() {
        return Ok(Provision::cached(waited_ms));
    }

    let started = Instant::now();
    let name = format!("node-v{version}-linux-x64");
    let file = format!("{name}.tar.gz");

    let mut resp = ureq::get(&format!("{DIST}/v{version}/SHASUMS256.txt")).call()?;
    let sums = resp
        .body_mut()
        .with_config()
        .limit(1 << 20)
        .read_to_string()?;
    let want = sums
        .lines()
        .find_map(|l| l.strip_suffix(&format!("  {file}")))
        .with_context(|| format!("{file} missing from SHASUMS256.txt"))?
        .to_string();

    let tmp = root.join(format!(".tmp-{version}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    let tarball = tmp.join(&file);
    let mut body = ureq::get(&format!("{DIST}/v{version}/{file}"))
        .call()?
        .into_body()
        .into_reader();
    let (bytes, got) = copy_hashing(&mut body, &mut File::create(&tarball)?)?;
    if got != want {
        fs::remove_dir_all(&tmp)?;
        bail!("checksum mismatch for {file}: want {want}, got {got}");
    }

    let gz = flate2::read::GzDecoder::new(File::open(&tarball)?);
    tar::Archive::new(gz).unpack(&tmp)?;
    let _ = fs::remove_dir_all(&target);
    fs::rename(tmp.join(&name), &target)?;
    fs::remove_dir_all(&tmp)?;
    File::create(target.join(".complete"))?;

    Ok(Provision {
        downloaded_bytes: bytes,
        took_ms: started.elapsed().as_millis(),
        waited_ms,
        verified: format!("sha256 {}…", &want[..16]),
    })
}

fn copy_hashing(src: &mut impl Read, dst: &mut impl Write) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        dst.write_all(&buf[..n])?;
        total += n as u64;
    }
    let hex = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((total, hex))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(v: &str, lts: serde_json::Value) -> Release {
        Release {
            version: v.into(),
            lts,
        }
    }

    #[test]
    fn resolves_like_setup_node() {
        let idx = vec![
            rel("v26.13.0", serde_json::json!("Kestrel")),
            rel("v24.11.2", serde_json::json!("Krypton")),
            rel("v24.11.1", serde_json::json!("Krypton")),
            rel("v22.21.0", serde_json::json!("Jod")),
            rel("v22.9.0", serde_json::json!(false)),
        ];
        assert_eq!(resolve("22", &idx).unwrap(), "22.21.0");
        assert_eq!(resolve("v24", &idx).unwrap(), "24.11.2");
        assert_eq!(resolve("24.x", &idx).unwrap(), "24.11.2");
        assert_eq!(resolve(">=22", &idx).unwrap(), "22.21.0");
        assert_eq!(resolve("24.11.1", &idx).unwrap(), "24.11.1");
        assert_eq!(resolve("22.9", &idx).unwrap(), "22.9.0");
        assert_eq!(resolve("lts/jod", &idx).unwrap(), "22.21.0");
        assert_eq!(resolve("lts/*", &idx).unwrap(), "26.13.0");
        assert!(resolve("^22 || ^24", &idx).is_err());
    }
}
