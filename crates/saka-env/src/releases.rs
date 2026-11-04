use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use saka_core::vocab::Tool;
use saka_ports::{PortError, PortResult, Release, ReleaseIndex};
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::json;

const INDEX_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const INDEX_MAX_BYTES: usize = 64 * 1024 * 1024;
const NPM_ABBREVIATED: &str = "application/vnd.npm.install-v1+json";

#[derive(Debug, Clone)]
pub struct ReleaseIndexConfig {
    pub go_dl_url: String,
    pub node_dist_url: String,
    pub npm_registry_url: String,
    pub user_agent: String,
}

pub struct HttpReleaseIndex {
    cfg: ReleaseIndexConfig,
    http: reqwest::Client,
    cache: Mutex<HashMap<Tool, Vec<Release>>>,
}

#[derive(Deserialize)]
struct GoRelease {
    version: String,
    stable: bool,
}

#[derive(Deserialize)]
struct NodeRelease {
    version: String,
    lts: serde_json::Value,
}

#[derive(Deserialize)]
struct NpmPackument {
    versions: BTreeMap<String, IgnoredAny>,
}

fn unavailable(tool: Tool, detail: String) -> PortError {
    PortError::retryable(
        "release_index_unavailable",
        json!({ "tool": tool, "detail": detail }),
    )
}

fn join(base: &str, rest: &str) -> String {
    format!("{}/{rest}", base.trim_end_matches('/'))
}

fn go_releases(body: &[u8]) -> serde_json::Result<Vec<Release>> {
    let releases: Vec<GoRelease> = serde_json::from_slice(body)?;
    Ok(releases
        .into_iter()
        .filter(|r| r.stable)
        .filter_map(|r| r.version.strip_prefix("go").map(String::from))
        .map(|version| Release { version, lts: None })
        .collect())
}

fn node_releases(body: &[u8]) -> serde_json::Result<Vec<Release>> {
    let releases: Vec<NodeRelease> = serde_json::from_slice(body)?;
    Ok(releases
        .into_iter()
        .filter_map(|r| {
            let version = r.version.strip_prefix('v')?.to_string();
            let lts = r.lts.as_str().map(String::from);
            Some(Release { version, lts })
        })
        .collect())
}

fn pnpm_releases(body: &[u8]) -> serde_json::Result<Vec<Release>> {
    let packument: NpmPackument = serde_json::from_slice(body)?;
    Ok(packument
        .versions
        .into_keys()
        .filter(|v| !v.contains('-'))
        .map(|version| Release { version, lts: None })
        .collect())
}

impl HttpReleaseIndex {
    pub fn new(cfg: ReleaseIndexConfig) -> PortResult<HttpReleaseIndex> {
        let http = reqwest::Client::builder()
            .user_agent(cfg.user_agent.clone())
            .timeout(INDEX_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| {
                PortError::permanent(
                    "release_index_config_invalid",
                    json!({ "error": e.to_string() }),
                )
            })?;
        Ok(HttpReleaseIndex {
            cfg,
            http,
            cache: Mutex::new(HashMap::new()),
        })
    }

    fn cached(&self, tool: Tool) -> Option<Vec<Release>> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&tool)
            .cloned()
    }

    async fn fetch(&self, tool: Tool) -> PortResult<Vec<u8>> {
        let request = match tool {
            Tool::Go => self
                .http
                .get(join(&self.cfg.go_dl_url, "?mode=json&include=all")),
            Tool::Node => self.http.get(join(&self.cfg.node_dist_url, "index.json")),
            Tool::Pnpm => self
                .http
                .get(join(&self.cfg.npm_registry_url, "pnpm"))
                .header(reqwest::header::ACCEPT, NPM_ABBREVIATED),
        };
        let mut response = request
            .send()
            .await
            .map_err(|e| unavailable(tool, e.without_url().to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(unavailable(tool, format!("HTTP {}", status.as_u16())));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| unavailable(tool, e.without_url().to_string()))?
        {
            body.extend_from_slice(&chunk);
            if body.len() > INDEX_MAX_BYTES {
                return Err(unavailable(tool, "index too large".into()));
            }
        }
        Ok(body)
    }
}

#[async_trait]
impl ReleaseIndex for HttpReleaseIndex {
    async fn releases(&self, tool: Tool) -> PortResult<Vec<Release>> {
        if let Some(releases) = self.cached(tool) {
            return Ok(releases);
        }
        let body = self.fetch(tool).await?;
        let releases = match tool {
            Tool::Go => go_releases(&body),
            Tool::Node => node_releases(&body),
            Tool::Pnpm => pnpm_releases(&body),
        }
        .map_err(|e| unavailable(tool, e.to_string()))?;
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(tool, releases.clone());
        Ok(releases)
    }
}
