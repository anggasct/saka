#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use saka_core::vocab::Tool;
use saka_env::{Platform, ToolchainConfig, ToolchainInstaller};
use saka_ports::Toolchains;
use serde_json::json;
use sha2::{Digest, Sha256, Sha512};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LINUX_X64: Platform = Platform {
    os: "linux",
    arch: "x64",
};

enum Entry<'a> {
    File(&'a str, &'a str, u32),
    Link(&'a str, &'a str),
}

fn tarball(entries: &[Entry<'_>]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        match entry {
            Entry::File(name, body, mode) => {
                header.set_size(body.len() as u64);
                header.set_mode(*mode);
                header.set_entry_type(tar::EntryType::Regular);
                tar.append_data(&mut header, name, body.as_bytes()).unwrap();
            }
            Entry::Link(name, target) => {
                header.set_size(0);
                header.set_mode(0o777);
                header.set_entry_type(tar::EntryType::Symlink);
                tar.append_link(&mut header, name, target).unwrap();
            }
        }
    }
    tar.into_inner().unwrap().finish().unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    caches: PathBuf,
    server: MockServer,
}

impl Fixture {
    async fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        Fixture {
            root: dir.path().join("toolchains"),
            caches: dir.path().join("caches"),
            _dir: dir,
            server: MockServer::start().await,
        }
    }

    fn installer(&self, platform: Option<Platform>) -> ToolchainInstaller {
        let base = self.server.uri();
        ToolchainInstaller::new(ToolchainConfig {
            root: self.root.clone(),
            caches: self.caches.clone(),
            go_dl_url: format!("{base}/dl"),
            node_dist_url: format!("{base}/dist"),
            npm_registry_url: format!("{base}/npm"),
            timeout: Duration::from_secs(10),
            user_agent: "saka-test".into(),
            platform,
        })
        .unwrap()
    }

    async fn serve_node(&self, version: &str, archive: Vec<u8>, sum: &str, downloads: u64) {
        let file = format!("node-v{version}-linux-x64.tar.gz");
        Mock::given(method("GET"))
            .and(path(format!("/dist/v{version}/SHASUMS256.txt")))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                "{}  node-v{version}-darwin-arm64.tar.gz\n{sum}  {file}\n",
                "0".repeat(64)
            )))
            .mount(&self.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/dist/v{version}/{file}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
            .expect(downloads)
            .mount(&self.server)
            .await;
    }

    fn leftovers(&self) -> Vec<PathBuf> {
        let staging = self.root.join(".tmp");
        std::fs::read_dir(&staging)
            .map(|d| d.map(|e| e.unwrap().path()).collect())
            .unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let mut pending = vec![self.root.clone()];
        while let Some(dir) = pending.pop() {
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    pending.push(entry.path());
                }
            }
        }
    }
}

fn node_archive(version: &str) -> Vec<u8> {
    let top = format!("node-v{version}-linux-x64");
    tarball(&[
        Entry::File(
            &format!("{top}/bin/node"),
            "#!/bin/sh\necho v26.13.0\n",
            0o755,
        ),
        Entry::File(
            &format!("{top}/lib/node_modules/npm/bin/npm-cli.js"),
            "console.log('npm')\n",
            0o644,
        ),
        Entry::Link(
            &format!("{top}/bin/npm"),
            "../lib/node_modules/npm/bin/npm-cli.js",
        ),
    ])
}

fn writable(p: &Path) -> bool {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o222 != 0
}

#[tokio::test]
async fn go_is_installed_once_from_the_release_index_and_left_read_only() {
    let fx = Fixture::new().await;
    let archive = tarball(&[
        Entry::File("go/bin/go", "#!/bin/sh\necho go version go1.26.3\n", 0o755),
        Entry::File("go/VERSION", "go1.26.3\n", 0o644),
    ]);
    Mock::given(method("GET"))
        .and(path("/dl/"))
        .and(query_param("mode", "json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "version": "go1.26.3", "files": [
                { "filename": "go1.26.3.src.tar.gz", "os": "", "arch": "", "sha256": "00", "kind": "source" },
                { "filename": "go1.26.3.darwin-arm64.tar.gz", "os": "darwin", "arch": "arm64", "sha256": "00", "kind": "archive" },
                { "filename": "go1.26.3.linux-amd64.tar.gz", "os": "linux", "arch": "amd64", "sha256": sha256(&archive), "kind": "archive" },
            ]},
        ])))
        .mount(&fx.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/dl/go1.26.3.linux-amd64.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
        .expect(1)
        .mount(&fx.server)
        .await;
    let installer = fx.installer(Some(LINUX_X64));

    let go = installer.install(Tool::Go, "1.26.3").await.unwrap();
    let dir = fx.root.join("go").join("1.26.3-linux-x64");
    assert_eq!(go.bin_dir, dir.join("bin").to_string_lossy());
    assert!(go.downloaded);
    assert_eq!(go.env["GOTOOLCHAIN"], "local");
    assert_eq!(
        go.env["GOMODCACHE"],
        fx.caches.join("go/mod").to_string_lossy()
    );
    assert!(fx.caches.join("go/build").is_dir());
    let out = std::process::Command::new(dir.join("bin/go"))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "go version go1.26.3\n"
    );
    assert!(!writable(&dir.join("bin/go")) && !writable(&dir.join("VERSION")));
    assert!(!writable(&dir) && !writable(&dir.join("bin")));
    assert!(fx.leftovers().is_empty());

    let again = installer.install(Tool::Go, "1.26.3").await.unwrap();
    assert!(!again.downloaded);
    assert_eq!(again.bin_dir, go.bin_dir);
}

#[tokio::test]
async fn two_parallel_installs_of_one_version_download_once() {
    let fx = Fixture::new().await;
    let archive = node_archive("26.13.0");
    fx.serve_node("26.13.0", archive.clone(), &sha256(&archive), 1)
        .await;
    let installer = fx.installer(Some(LINUX_X64));

    let (a, b) = tokio::join!(
        installer.install(Tool::Node, "26.13.0"),
        installer.install(Tool::Node, "26.13.0")
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.bin_dir, b.bin_dir);
    assert_ne!(a.downloaded, b.downloaded, "exactly one of them downloaded");
    let bin = PathBuf::from(&a.bin_dir);
    assert_eq!(
        std::fs::read_link(bin.join("npm")).unwrap(),
        Path::new("../lib/node_modules/npm/bin/npm-cli.js")
    );
    assert!(
        bin.join("npm").exists(),
        "the symlink resolves inside the install"
    );
}

#[tokio::test]
async fn a_checksum_mismatch_leaves_nothing_behind_and_is_permanent() {
    let fx = Fixture::new().await;
    let archive = node_archive("26.13.0");
    fx.serve_node("26.13.0", archive, &"ab".repeat(32), 1).await;

    let err = fx
        .installer(Some(LINUX_X64))
        .install(Tool::Node, "26.13.0")
        .await
        .unwrap_err();
    assert_eq!(err.code, "toolchain_unavailable");
    assert!(!err.retryable);
    assert_eq!(err.params["cause"], "checksum_mismatch");
    assert_eq!(err.params["tool"], "node");
    assert!(!fx.root.join("node").join("26.13.0-linux-x64").exists());
    assert!(fx.leftovers().is_empty(), "{:?}", fx.leftovers());
}

#[tokio::test]
async fn missing_versions_are_permanent_and_server_errors_are_retried() {
    let fx = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path("/dist/v99.0.0/SHASUMS256.txt"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&fx.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/dist/v26.12.0/SHASUMS256.txt"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&fx.server)
        .await;
    let installer = fx.installer(Some(LINUX_X64));

    let missing = installer.install(Tool::Node, "99.0.0").await.unwrap_err();
    assert_eq!(
        (missing.params["cause"].as_str(), missing.retryable),
        (Some("not_found"), false)
    );
    let flaky = installer.install(Tool::Node, "26.12.0").await.unwrap_err();
    assert_eq!(
        (flaky.params["cause"].as_str(), flaky.retryable),
        (Some("download_failed"), true)
    );
    let other = fx
        .installer(None)
        .install(Tool::Go, "1.26.3")
        .await
        .unwrap_err();
    assert_eq!(other.params["cause"], "unsupported_platform");
    let range = installer.install(Tool::Node, ">=26").await.unwrap_err();
    assert_eq!(range.params["cause"], "not_found");
}

#[tokio::test]
async fn pnpm_comes_from_the_registry_with_a_shim_that_runs_on_the_declared_node() {
    let fx = Fixture::new().await;
    let archive = tarball(&[Entry::File(
        "package/bin/pnpm.cjs",
        "console.log('12.16.1')\n",
        0o644,
    )]);
    let integrity = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(Sha512::digest(&archive))
    );
    let tarball_url = format!("{}/npm/pnpm/-/pnpm-12.16.1.tgz", fx.server.uri());
    Mock::given(method("GET"))
        .and(path("/npm/pnpm/12.16.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "pnpm", "version": "12.16.1",
            "dist": { "tarball": tarball_url, "integrity": integrity, "shasum": "ignored" },
        })))
        .mount(&fx.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/npm/pnpm/-/pnpm-12.16.1.tgz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
        .expect(1)
        .mount(&fx.server)
        .await;

    let pnpm = fx
        .installer(Some(LINUX_X64))
        .install(Tool::Pnpm, "12.16.1")
        .await
        .unwrap();
    let dir = fx.root.join("pnpm").join("12.16.1-linux-x64");
    let shim = std::fs::read_to_string(dir.join("bin/pnpm")).unwrap();
    assert_eq!(
        shim,
        format!(
            "#!/bin/sh\nexec node \"{}\" \"$@\"\n",
            dir.join("lib/bin/pnpm.cjs").display()
        )
    );
    assert!(dir.join("lib/bin/pnpm.cjs").is_file());
    assert!(
        std::fs::metadata(dir.join("bin/pnpm"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111
            != 0
    );
    assert_eq!(
        pnpm.env["npm_config_store_dir"],
        fx.caches.join("pnpm-store").to_string_lossy()
    );
    assert_eq!(pnpm.env["COREPACK_ENABLE_DOWNLOAD_PROMPT"], "0");
}

#[tokio::test]
async fn an_archive_that_links_outside_its_root_is_refused() {
    let fx = Fixture::new().await;
    let archive = tarball(&[
        Entry::File("node-v26.13.0-linux-x64/bin/node", "x", 0o755),
        Entry::Link("node-v26.13.0-linux-x64/bin/evil", "../../../../etc/passwd"),
    ]);
    fx.serve_node("26.13.0", archive.clone(), &sha256(&archive), 1)
        .await;

    let err = fx
        .installer(Some(LINUX_X64))
        .install(Tool::Node, "26.13.0")
        .await
        .unwrap_err();
    assert_eq!(err.params["cause"], "archive_invalid");
    assert!(!err.retryable);
    assert!(!fx.root.join("node").join("26.13.0-linux-x64").exists());
    assert!(fx.leftovers().is_empty(), "{:?}", fx.leftovers());
}
