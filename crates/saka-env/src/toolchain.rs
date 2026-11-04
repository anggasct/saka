use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use saka_core::diagnostics::{InstalledToolchain, ToolchainCache, host_version_of};
use saka_core::model::ToolchainUse;
use saka_core::vocab::{HostTool, Tool};
use saka_ports::{PortError, PortResult, Toolchains};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256, Sha512};

const DOWNLOAD_MAX_BYTES: u64 = 512 * 1024 * 1024;
const INDEX_MAX_BYTES: u64 = 64 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const MARKER: &str = ".saka-toolchain.json";
const HOST_VERSION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    pub os: &'static str,
    pub arch: &'static str,
}

impl Platform {
    pub fn current() -> Option<Platform> {
        let os = match std::env::consts::OS {
            "linux" => "linux",
            "macos" => "darwin",
            _ => return None,
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            _ => return None,
        };
        Some(Platform { os, arch })
    }

    fn go_arch(self) -> &'static str {
        if self.arch == "x64" {
            "amd64"
        } else {
            self.arch
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolchainConfig {
    pub root: PathBuf,
    pub caches: PathBuf,
    pub go_dl_url: String,
    pub node_dist_url: String,
    pub npm_registry_url: String,
    pub timeout: Duration,
    pub user_agent: String,
    pub platform: Option<Platform>,
    pub host_path: Vec<String>,
}

pub struct ToolchainInstaller {
    cfg: ToolchainConfig,
    http: reqwest::Client,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

enum Checksum {
    Sha256(Vec<u8>),
    Sha512(Vec<u8>),
}

struct Artifact {
    url: String,
    checksum: Checksum,
}

#[derive(Deserialize)]
struct GoRelease {
    version: String,
    files: Vec<GoFile>,
}

#[derive(Deserialize)]
struct GoFile {
    filename: String,
    os: String,
    arch: String,
    sha256: String,
    kind: String,
}

#[derive(Deserialize)]
struct NpmVersion {
    dist: NpmDist,
}

#[derive(Deserialize)]
struct NpmDist {
    tarball: String,
    integrity: Option<String>,
}

fn unavailable(tool: Tool, version: &str, cause: &str, detail: Option<String>) -> PortError {
    let params = json!({ "tool": tool, "version": version, "cause": cause, "detail": detail });
    if cause == "download_failed" {
        PortError::retryable("toolchain_unavailable", params)
    } else {
        PortError::permanent("toolchain_unavailable", params)
    }
}

fn io(op: &str, e: &std::io::Error) -> PortError {
    PortError::retryable("env_io_failed", json!({ "op": op, "error": e.to_string() }))
}

fn join(base: &str, rest: &str) -> String {
    format!("{}/{rest}", base.trim_end_matches('/'))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            text.get(i..i + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
        })
        .collect()
}

fn version_ok(tool: Tool, version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    let numeric = parts
        .iter()
        .all(|p| !p.is_empty() && p.len() <= 6 && p.bytes().all(|b| b.is_ascii_digit()));
    numeric && (parts.len() == 3 || (tool == Tool::Go && parts.len() == 2))
}

fn inside(link: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let mut depth: Vec<Component<'_>> = link
        .parent()
        .map(|p| p.components().collect())
        .unwrap_or_default();
    for c in target.components() {
        match c {
            Component::Normal(_) => depth.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                if depth.pop().is_none() {
                    return false;
                }
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

fn extract(archive: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    tar.set_preserve_permissions(true);
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() || kind.is_pax_local_extensions() {
            continue;
        }
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let rest: PathBuf = path.components().skip(1).collect();
        if rest.as_os_str().is_empty() {
            continue;
        }
        if !rest.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(format!(
                "entry outside the archive root: {}",
                path.display()
            ));
        }
        if kind.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|e| e.to_string())?
                .ok_or("symlink without a target")?
                .into_owned();
            if !inside(&rest, &target) {
                return Err(format!("symlink leaves the archive: {}", rest.display()));
            }
        } else if !(kind.is_file() || kind.is_dir()) {
            return Err(format!("unsupported entry type at {}", rest.display()));
        }
        let target = dest.join(&rest);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        entry.unpack(&target).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn make_read_only(root: &Path) -> std::io::Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut dirs = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let meta = entry.path().symlink_metadata()?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                let mode = meta.permissions().mode() & !0o222;
                std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(mode))?;
            }
        }
        dirs.push(dir);
    }
    for dir in dirs.iter().rev() {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555))?;
    }
    Ok(())
}

fn write_shim(path: &Path, script: &Path) -> std::io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    writeln!(file, "#!/bin/sh")?;
    writeln!(file, "exec node \"{}\" \"$@\"", script.display())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

impl ToolchainInstaller {
    pub fn new(cfg: ToolchainConfig) -> PortResult<ToolchainInstaller> {
        let http = reqwest::Client::builder()
            .user_agent(cfg.user_agent.clone())
            .timeout(cfg.timeout)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| {
                PortError::permanent(
                    "toolchain_config_invalid",
                    json!({ "error": e.to_string() }),
                )
            })?;
        Ok(ToolchainInstaller {
            cfg,
            http,
            locks: Mutex::new(HashMap::new()),
        })
    }

    pub fn install_dir(&self, tool: Tool, version: &str) -> Option<PathBuf> {
        let p = self.cfg.platform?;
        Some(
            self.cfg
                .root
                .join(tool.as_str())
                .join(format!("{version}-{}-{}", p.os, p.arch)),
        )
    }

    fn lock(&self, key: String) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks.entry(key).or_default().clone()
    }

    fn env(&self, tool: Tool) -> PortResult<BTreeMap<String, String>> {
        let caches = &self.cfg.caches;
        let pairs: Vec<(&str, PathBuf)> = match tool {
            Tool::Go => vec![
                ("GOMODCACHE", caches.join("go").join("mod")),
                ("GOCACHE", caches.join("go").join("build")),
            ],
            Tool::Pnpm => vec![("npm_config_store_dir", caches.join("pnpm-store"))],
            Tool::Node => Vec::new(),
        };
        let mut env = BTreeMap::new();
        for (name, dir) in pairs {
            std::fs::create_dir_all(&dir).map_err(|e| io("create_cache_dir", &e))?;
            env.insert(name.to_string(), dir.to_string_lossy().into_owned());
        }
        match tool {
            Tool::Go => {
                env.insert("GOTOOLCHAIN".into(), "local".into());
            }
            Tool::Pnpm => {
                env.insert("COREPACK_ENABLE_DOWNLOAD_PROMPT".into(), "0".into());
            }
            Tool::Node => {}
        }
        Ok(env)
    }

    fn used(
        &self,
        tool: Tool,
        version: &str,
        dir: &Path,
        downloaded: bool,
    ) -> PortResult<ToolchainUse> {
        Ok(ToolchainUse {
            tool,
            version: version.to_string(),
            bin_dir: dir.join("bin").to_string_lossy().into_owned(),
            env: self.env(tool)?,
            downloaded,
        })
    }

    async fn get(&self, tool: Tool, version: &str, url: &str) -> PortResult<reqwest::Response> {
        let failed = |detail: String| unavailable(tool, version, "download_failed", Some(detail));
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| failed(e.without_url().to_string()))?;
        match response.status() {
            s if s.is_success() => Ok(response),
            reqwest::StatusCode::NOT_FOUND => Err(unavailable(tool, version, "not_found", None)),
            s => Err(failed(format!("{url}: HTTP {}", s.as_u16()))),
        }
    }

    async fn body(&self, tool: Tool, version: &str, url: &str) -> PortResult<Vec<u8>> {
        let mut response = self.get(tool, version, url).await?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            unavailable(
                tool,
                version,
                "download_failed",
                Some(e.without_url().to_string()),
            )
        })? {
            body.extend_from_slice(&chunk);
            if body.len() as u64 > INDEX_MAX_BYTES {
                return Err(unavailable(
                    tool,
                    version,
                    "download_failed",
                    Some(format!("{url}: index too large")),
                ));
            }
        }
        Ok(body)
    }

    async fn artifact(&self, tool: Tool, version: &str, p: Platform) -> PortResult<Artifact> {
        let not_found = || unavailable(tool, version, "not_found", None);
        match tool {
            Tool::Go => {
                let index = join(&self.cfg.go_dl_url, "?mode=json&include=all");
                let body = self.body(tool, version, &index).await?;
                let releases: Vec<GoRelease> = serde_json::from_slice(&body).map_err(|e| {
                    unavailable(tool, version, "download_failed", Some(e.to_string()))
                })?;
                let wanted = format!("go{version}");
                let file = releases
                    .into_iter()
                    .filter(|r| r.version == wanted)
                    .flat_map(|r| r.files)
                    .find(|f| f.kind == "archive" && f.os == p.os && f.arch == p.go_arch())
                    .ok_or_else(not_found)?;
                let digest = unhex(&file.sha256).ok_or_else(not_found)?;
                Ok(Artifact {
                    url: join(&self.cfg.go_dl_url, &file.filename),
                    checksum: Checksum::Sha256(digest),
                })
            }
            Tool::Node => {
                let filename = format!("node-v{version}-{}-{}.tar.gz", p.os, p.arch);
                let base = join(&self.cfg.node_dist_url, &format!("v{version}"));
                let sums = self
                    .body(tool, version, &join(&base, "SHASUMS256.txt"))
                    .await?;
                let sums = String::from_utf8_lossy(&sums);
                let digest = sums
                    .lines()
                    .filter_map(|l| l.split_once("  "))
                    .find(|(_, name)| name.trim() == filename)
                    .and_then(|(sum, _)| unhex(sum.trim()))
                    .ok_or_else(not_found)?;
                Ok(Artifact {
                    url: join(&base, &filename),
                    checksum: Checksum::Sha256(digest),
                })
            }
            Tool::Pnpm => {
                let meta = join(&self.cfg.npm_registry_url, &format!("pnpm/{version}"));
                let body = self.body(tool, version, &meta).await?;
                let meta: NpmVersion = serde_json::from_slice(&body).map_err(|e| {
                    unavailable(tool, version, "download_failed", Some(e.to_string()))
                })?;
                let digest = meta
                    .dist
                    .integrity
                    .as_deref()
                    .and_then(|i| i.strip_prefix("sha512-"))
                    .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
                    .ok_or_else(not_found)?;
                let url = meta.dist.tarball;
                let trusted =
                    url.starts_with("https://") || url.starts_with(&self.cfg.npm_registry_url);
                if !trusted {
                    return Err(unavailable(tool, version, "not_found", Some(url)));
                }
                Ok(Artifact {
                    url,
                    checksum: Checksum::Sha512(digest),
                })
            }
        }
    }

    async fn download(
        &self,
        tool: Tool,
        version: &str,
        artifact: &Artifact,
        to: &Path,
    ) -> PortResult<()> {
        let failed = |detail: String| unavailable(tool, version, "download_failed", Some(detail));
        let mut response = self.get(tool, version, &artifact.url).await?;
        let mut file = std::fs::File::create(to).map_err(|e| io("create_download", &e))?;
        let mut sha256 = Sha256::new();
        let mut sha512 = Sha512::new();
        let mut size = 0u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| failed(e.without_url().to_string()))?
        {
            size += chunk.len() as u64;
            if size > DOWNLOAD_MAX_BYTES {
                return Err(failed(format!(
                    "{} is larger than {DOWNLOAD_MAX_BYTES} bytes",
                    artifact.url
                )));
            }
            match artifact.checksum {
                Checksum::Sha256(_) => sha256.update(&chunk),
                Checksum::Sha512(_) => sha512.update(&chunk),
            }
            file.write_all(&chunk)
                .map_err(|e| io("write_download", &e))?;
        }
        file.sync_all().map_err(|e| io("write_download", &e))?;
        let matches = match &artifact.checksum {
            Checksum::Sha256(want) => sha256.finalize().as_slice() == want.as_slice(),
            Checksum::Sha512(want) => sha512.finalize().as_slice() == want.as_slice(),
        };
        if matches {
            Ok(())
        } else {
            Err(unavailable(
                tool,
                version,
                "checksum_mismatch",
                Some(artifact.url.clone()),
            ))
        }
    }

    async fn fetch(&self, tool: Tool, version: &str, p: Platform, dir: &Path) -> PortResult<()> {
        let staging = self.cfg.root.join(".tmp");
        std::fs::create_dir_all(&staging).map_err(|e| io("create_toolchain_dir", &e))?;
        let tmp = tempfile::Builder::new()
            .prefix(&format!("{}-{version}-", tool.as_str()))
            .tempdir_in(&staging)
            .map_err(|e| io("create_toolchain_dir", &e))?;
        let artifact = self.artifact(tool, version, p).await?;
        let archive = tmp.path().join("archive.tar.gz");
        self.download(tool, version, &artifact, &archive).await?;
        let out = tmp.path().join("out");
        let unpack_to = if tool == Tool::Pnpm {
            out.join("lib")
        } else {
            out.clone()
        };
        let digest = match &artifact.checksum {
            Checksum::Sha256(d) => format!("sha256:{}", hex(d)),
            Checksum::Sha512(d) => format!("sha512:{}", hex(d)),
        };
        let (final_dir, marker) = (
            dir.to_path_buf(),
            json!({ "tool": tool, "version": version, "digest": digest }),
        );
        let staged = out.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            extract(&archive, &unpack_to)?;
            if tool == Tool::Pnpm {
                let bin = staged.join("bin");
                std::fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
                for name in ["pnpm", "pnpx"] {
                    let script = final_dir
                        .join("lib")
                        .join("bin")
                        .join(format!("{name}.cjs"));
                    write_shim(&bin.join(name), &script).map_err(|e| e.to_string())?;
                }
            }
            std::fs::write(staged.join(MARKER), marker.to_string()).map_err(|e| e.to_string())?;
            make_read_only(&staged).map_err(|e| e.to_string())?;
            // Moving a directory to another parent rewrites its `..`, so the top stays writable until then.
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| {
            PortError::retryable(
                "env_io_failed",
                json!({ "op": "unpack_toolchain", "error": e.to_string() }),
            )
        })?
        .map_err(|e| unavailable(tool, version, "archive_invalid", Some(e)))?;
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io("create_toolchain_dir", &e))?;
        }
        let renamed = std::fs::rename(&out, dir);
        if renamed.is_err() {
            // The staged tree is read-only; the temp dir can only remove it once it is writable again.
            let _ = make_dirs_writable(&out);
        }
        match renamed {
            Ok(()) => std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555))
                .map_err(|e| io("install_toolchain", &e)),
            Err(_) if dir.join(MARKER).exists() => Ok(()),
            Err(e) => Err(io("install_toolchain", &e)),
        }
    }
}

fn make_dirs_writable(root: &Path) -> std::io::Result<()> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))?;
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

#[async_trait]
impl Toolchains for ToolchainInstaller {
    async fn install(&self, tool: Tool, version: &str) -> PortResult<ToolchainUse> {
        if !version_ok(tool, version) {
            return Err(unavailable(tool, version, "not_found", None));
        }
        let (Some(p), Some(dir)) = (self.cfg.platform, self.install_dir(tool, version)) else {
            return Err(unavailable(tool, version, "unsupported_platform", None));
        };
        let lock = self.lock(dir.to_string_lossy().into_owned());
        let _held = lock.lock().await;
        if dir.join(MARKER).exists() {
            return self.used(tool, version, &dir, false);
        }
        self.fetch(tool, version, p, &dir).await?;
        self.used(tool, version, &dir, true)
    }

    async fn host_version(&self, tool: HostTool) -> PortResult<Option<String>> {
        host_version(&self.cfg.host_path, tool).await
    }

    async fn cache(&self) -> PortResult<ToolchainCache> {
        let root = self.cfg.root.clone();
        tokio::task::spawn_blocking(move || cache_status(&root))
            .await
            .map_err(|e| {
                PortError::retryable(
                    "env_io_failed",
                    json!({ "op": "read_toolchain_cache", "error": e.to_string() }),
                )
            })
    }
}

fn on_path(dirs: &[String], program: &str) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| Path::new(dir).join(program))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

pub async fn host_version(dirs: &[String], tool: HostTool) -> PortResult<Option<String>> {
    let Some(program) = on_path(dirs, tool.program()) else {
        return Ok(None);
    };
    let mut command = tokio::process::Command::new(&program);
    command
        .arg("--version")
        .env_clear()
        .env("PATH", dirs.join(":"))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let unavailable = |detail: &str| {
        PortError::retryable(
            "host_toolchain_unavailable",
            json!({ "tool": tool, "detail": detail }),
        )
    };
    let output = tokio::time::timeout(HOST_VERSION_TIMEOUT, command.output())
        .await
        .map_err(|_| unavailable("timeout"))?
        .map_err(|_| unavailable("spawn_failed"))?;
    if !output.status.success() {
        return Err(unavailable("exit_status"));
    }
    host_version_of(&String::from_utf8_lossy(&output.stdout))
        .map(Some)
        .ok_or_else(|| unavailable("unparsable"))
}

fn writable(root: &Path) -> bool {
    let staging = root.join(".tmp");
    std::fs::create_dir_all(&staging).is_ok()
        && tempfile::NamedTempFile::new_in(&staging)
            .and_then(|mut f| f.write_all(b"saka").and_then(|()| f.as_file().sync_all()))
            .is_ok()
}

#[derive(Deserialize)]
struct Marker {
    tool: Tool,
    version: String,
}

pub fn cache_status(root: &Path) -> ToolchainCache {
    let mut installed = Vec::new();
    for tool in Tool::ALL {
        let Ok(entries) = std::fs::read_dir(root.join(tool.as_str())) else {
            continue;
        };
        for entry in entries.flatten() {
            let marker = std::fs::read(entry.path().join(MARKER))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok());
            if let Some(m) = marker.filter(|m| m.tool == *tool) {
                installed.push(InstalledToolchain {
                    tool: m.tool,
                    version: m.version,
                });
            }
        }
    }
    ToolchainCache {
        path: root.to_string_lossy().into_owned(),
        writable: writable(root),
        installed,
    }
}
