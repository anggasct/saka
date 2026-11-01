//! The web app: built files under `/`, `index.html` for every client route, and never the
//! app for an `/api/*` path.

use std::borrow::Cow;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

use crate::problem::ApiError;
use crate::state::AppState;

/// One built file.
#[derive(Debug, Clone)]
pub struct Asset {
    pub bytes: Cow<'static, [u8]>,
    pub mime: String,
}

/// Where the built web app comes from (embedded in release builds, a map in tests).
pub trait Assets: Send + Sync {
    /// `path` is relative, e.g. `index.html` or `assets/index-1a2b3c.js`.
    fn get(&self, path: &str) -> Option<Asset>;
}

const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const NO_CACHE: &str = "no-cache";

fn file(asset: Asset, cache: &'static str) -> Response {
    let mut response = (StatusCode::OK, Body::from(asset.bytes)).into_response();
    let headers = response.headers_mut();
    if let Ok(mime) = HeaderValue::from_str(&asset.mime) {
        headers.insert(header::CONTENT_TYPE, mime);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}

pub async fn serve(State(state): State<AppState>, method: Method, uri: Uri) -> Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return ApiError::not_found().into_response();
    }
    let Some(assets) = state.assets() else {
        return ApiError::not_found().into_response();
    };
    if method != Method::GET && method != Method::HEAD {
        return ApiError::not_found().into_response();
    }
    let relative = path.trim_start_matches('/');
    if !relative.is_empty()
        && !relative.split('/').any(|s| s == ".." || s.is_empty())
        && let Some(asset) = assets.get(relative)
    {
        // Vite puts content-hashed files under `assets/`.
        let cache = if relative.starts_with("assets/") {
            IMMUTABLE
        } else {
            NO_CACHE
        };
        return file(asset, cache);
    }
    let last = relative.rsplit('/').next().unwrap_or_default();
    if last.contains('.') {
        return ApiError::not_found().into_response();
    }
    match assets.get("index.html") {
        Some(index) => file(index, NO_CACHE),
        None => ApiError::not_found().into_response(),
    }
}

/// The app built into the binary (`--features embed-web`, after `pnpm --dir web build`).
#[cfg(feature = "embed-web")]
mod embedded {
    use super::{Asset, Assets};

    #[derive(rust_embed::Embed)]
    #[folder = "../../web/dist"]
    struct WebDist;

    pub struct Embedded;

    impl Assets for Embedded {
        fn get(&self, path: &str) -> Option<Asset> {
            let file = WebDist::get(path)?;
            Some(Asset {
                mime: file.metadata.mimetype().to_string(),
                bytes: file.data,
            })
        }
    }
}

/// The embedded web app, when this build has one.
pub fn embedded() -> Option<std::sync::Arc<dyn Assets>> {
    #[cfg(feature = "embed-web")]
    {
        Some(std::sync::Arc::new(embedded::Embedded))
    }
    #[cfg(not(feature = "embed-web"))]
    {
        None
    }
}
