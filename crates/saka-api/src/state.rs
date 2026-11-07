use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::http::StatusCode;
use saka_engine::Engine;
use saka_ports::BestEffort;
use serde_json::json;

use crate::auth::Authenticator;
use crate::problem::ApiError;
use crate::spa::Assets;

#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub server_version: String,
    pub allowed_origins: Vec<String>,
    pub keep_alive: Duration,
    pub auth: Option<Arc<Authenticator>>,
}

impl ApiConfig {
    pub fn new(server_version: impl Into<String>) -> ApiConfig {
        ApiConfig {
            server_version: server_version.into(),
            allowed_origins: Vec::new(),
            keep_alive: Duration::from_secs(15),
            auth: None,
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

    pub fn set_engine(&self, engine: Arc<Engine>) {
        self.0
            .engine
            .set(engine)
            .map_err(|_| "already set")
            .best_effort("set the engine");
    }

    pub fn set_assets(&self, assets: Arc<dyn Assets>) {
        self.0
            .assets
            .set(assets)
            .map_err(|_| "already set")
            .best_effort("set the assets");
    }

    pub(crate) fn assets(&self) -> Option<&Arc<dyn Assets>> {
        self.0.assets.get()
    }

    pub fn authenticator(&self) -> Option<&Arc<Authenticator>> {
        self.0.cfg.auth.as_ref()
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

    pub(crate) fn key_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut keys = self
            .0
            .keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        keys.entry(key.to_string()).or_default().clone()
    }

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
