#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use saka_api::spa::{Asset, Assets};
use saka_api::{ApiConfig, AppState, router};
use tower::ServiceExt;

struct Map(HashMap<&'static str, (&'static str, &'static str)>);

impl Assets for Map {
    fn get(&self, path: &str) -> Option<Asset> {
        self.0.get(path).map(|(mime, text)| Asset {
            bytes: Cow::Borrowed(text.as_bytes()),
            mime: (*mime).to_string(),
        })
    }
}

struct Got {
    status: StatusCode,
    content_type: String,
    cache: String,
    body: String,
}

fn get(state: &AppState, path: &str) -> Got {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let r = router(state.clone())
            .oneshot(
                Request::get(path)
                    .header("host", "127.0.0.1:7420")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let header = |name: &str| {
            r.headers()
                .get(name)
                .map(|v| v.to_str().unwrap().to_string())
                .unwrap_or_default()
        };
        let (status, content_type, cache) =
            (r.status(), header("content-type"), header("cache-control"));
        let bytes = r.into_body().collect().await.unwrap().to_bytes();
        Got {
            status,
            content_type,
            cache,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    })
}

fn check(state: &AppState, asset: &str) {
    for route in ["/", "/delivery", "/delivery/anything/deeper"] {
        let page = get(state, route);
        assert_eq!(page.status, StatusCode::OK, "{route}");
        assert!(
            page.content_type.starts_with("text/html"),
            "{route}: {}",
            page.content_type
        );
        assert_eq!(page.cache, "no-cache");
        assert!(page.body.contains("<div id=\"root\">"), "{route}");
    }
    let file = get(state, asset);
    assert_eq!(file.status, StatusCode::OK, "{asset}");
    assert_eq!(file.cache, "public, max-age=31536000, immutable");
    assert!(
        file.content_type.contains("javascript"),
        "{}",
        file.content_type
    );

    for api in ["/api/v1/nothing", "/api/other", "/api"] {
        let r = get(state, api);
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{api}");
        assert_eq!(r.content_type, "application/problem+json", "{api}");
        assert!(r.body.contains("\"code\":\"not_found\""), "{api}");
    }
    let missing = get(state, "/assets/gone-123.js");
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.content_type, "application/problem+json");
}

#[test]
fn client_routes_get_the_app_and_api_paths_never_do() {
    let state = AppState::new(ApiConfig::new("0.1.0-test"));
    state.set_assets(Arc::new(Map(HashMap::from([
        (
            "index.html",
            ("text/html", "<!doctype html><div id=\"root\"></div><script src=\"/assets/index-1a2b.js\"></script>"),
        ),
        ("assets/index-1a2b.js", ("text/javascript", "console.log(1)")),
    ]))));
    check(&state, "/assets/index-1a2b.js");

    let bare = AppState::new(ApiConfig::new("0.1.0-test"));
    assert_eq!(get(&bare, "/delivery").status, StatusCode::NOT_FOUND);
}

#[cfg(feature = "embed-web")]
#[test]
fn the_embedded_build_is_served() {
    let assets = saka_api::spa::embedded().expect("built with embed-web");
    let index = assets.get("index.html").expect("web/dist/index.html");
    let html = String::from_utf8_lossy(&index.bytes).into_owned();
    let script = html
        .split("src=\"/")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("a script tag")
        .to_string();
    assert!(script.starts_with("assets/"), "{script}");
    let state = AppState::new(ApiConfig::new("0.1.0-test"));
    state.set_assets(assets);
    check(&state, &format!("/{script}"));
}
