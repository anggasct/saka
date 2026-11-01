//! Shared server state: the engine (set once it is ready), settings, and HTTP counters.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::http::StatusCode;
use saka_engine::Engine;
use serde_json::json;

use crate::problem::ApiError;
use crate::spa::Assets;

#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub server_version: String,
    /// Browser origins allowed besides the server's own.
    pub allowed_origins: Vec<String>,
    /// Between `: keep-alive` comments on the event stream.
    pub keep_alive: Duration,
}

impl ApiConfig {
    pub fn new(server_version: impl Into<String>) -> ApiConfig {
        ApiConfig {
            server_version: server_version.into(),
            allowed_origins: Vec::new(),
            keep_alive: Duration::from_secs(15),
        }
    }
}

struct Inner {
    cfg: ApiConfig,
    engine: OnceLock<Arc<Engine>>,
    assets: OnceLock<Arc<dyn Assets>>,
    http: Mutex<BTreeMap<(String, u16), u64>>,
    keys: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

impl AppState {
    pub fn new(cfg: ApiConfig) -> AppState {
        AppState(Arc::new(Inner {
            cfg,
            engine: OnceLock::new(),
            assets: OnceLock::new(),
            http: Mutex::new(BTreeMap::new()),
            keys: Mutex::new(HashMap::new()),
        }))
    }

    /// Serves requests from now on (before this, they get `503 not_ready`).
    pub fn set_engine(&self, engine: Arc<Engine>) {
        let _ = self.0.engine.set(engine);
    }

    /// Serves the web app from `assets` for every path that is not the API.
    pub fn set_assets(&self, assets: Arc<dyn Assets>) {
        let _ = self.0.assets.set(assets);
    }

    pub(crate) fn assets(&self) -> Option<&Arc<dyn Assets>> {
        self.0.assets.get()
    }

    pub fn config(&self) -> &ApiConfig {
        &self.0.cfg
    }

    pub fn engine_if_ready(&self) -> Option<&Arc<Engine>> {
        self.0.engine.get()
    }

    pub fn engine(&self) -> Result<&Arc<Engine>, ApiError> {
        self.0
            .engine
            .get()
            .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "not_ready", json!({})))
    }

    pub(crate) fn count_request(&self, route: &str, status: u16) {
        if let Ok(mut m) = self.0.http.lock() {
            *m.entry((route.to_string(), status)).or_default() += 1;
        }
    }

    pub(crate) fn http_counts(&self) -> Vec<(String, u16, u64)> {
        self.0
            .http
            .lock()
            .map(|m| m.iter().map(|((r, s), n)| (r.clone(), *s, *n)).collect())
            .unwrap_or_default()
    }

    /// One lock per idempotency key, so two requests with the same key run one at a time.
    pub(crate) fn key_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut keys = self
            .0
            .keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        keys.entry(key.to_string()).or_default().clone()
    }

    /// Forgets the key's lock once nobody holds or waits for it.
    pub(crate) fn release_key(&self, key: &str) {
        let mut keys = self
            .0
            .keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if keys.get(key).is_some_and(|l| Arc::strong_count(l) == 1) {
            keys.remove(key);
        }
    }
}
