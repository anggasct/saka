#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

mod support;

use axum::http::{Method, StatusCode};
use serde_json::json;
use support::{gate, gate_with, get, post, problem};

const APP: &str = "https://app.example.com";

fn preflight<'a>(path: &'a str, origin: &'a str) -> support::Call<'a> {
    let mut call = get(path)
        .host("127.0.0.1:7420")
        .header("origin", origin)
        .header("access-control-request-method", "POST")
        .header(
            "access-control-request-headers",
            "authorization, idempotency-key",
        );
    call.method = Method::OPTIONS;
    call
}

#[test]
fn a_preflight_from_an_allowed_origin_is_answered_before_the_gate() {
    let g = gate();
    let r = g.send(preflight("/api/v1/projects", APP));
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(r.headers["access-control-allow-origin"], APP);
    let allowed = r.headers["access-control-allow-headers"].to_str().unwrap();
    for name in [
        "authorization",
        "idempotency-key",
        "last-event-id",
        "if-match",
    ] {
        assert!(allowed.contains(name), "{name} in {allowed}");
    }
    assert!(r.headers.contains_key("access-control-allow-methods"));
    assert!(r.headers["vary"].to_str().unwrap().contains("origin"));
}

#[test]
fn a_preflight_from_another_origin_gets_no_permission() {
    let g = gate();
    let r = g.send(preflight("/api/v1/projects", "https://evil.example"));
    assert!(!r.headers.contains_key("access-control-allow-origin"));
    assert_ne!(r.status, StatusCode::NO_CONTENT);
}

#[test]
fn answers_to_an_allowed_origin_carry_the_origin_and_expose_the_etag() {
    let g = gate();
    let token = g.cli_token();
    let r = g.send(
        get("/api/v1/projects")
            .host("127.0.0.1:7420")
            .header("origin", APP)
            .bearer(&token),
    );
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.headers["access-control-allow-origin"], APP);
    assert!(
        r.headers["access-control-expose-headers"]
            .to_str()
            .unwrap()
            .contains("etag")
    );
    let refused = g.send(
        get("/api/v1/projects")
            .host("127.0.0.1:7420")
            .header("origin", APP),
    );
    problem(&refused, StatusCode::UNAUTHORIZED, "auth_required");
    assert_eq!(refused.headers["access-control-allow-origin"], APP);
}

#[test]
fn a_request_without_an_origin_or_from_a_stranger_gets_no_cors_headers() {
    let g = gate();
    let token = g.cli_token();
    let plain = g.send(
        get("/api/v1/projects")
            .host("127.0.0.1:7420")
            .bearer(&token),
    );
    assert!(!plain.headers.contains_key("access-control-allow-origin"));
    let stranger = g.send(
        get("/api/v1/projects")
            .host("127.0.0.1:7420")
            .header("origin", "https://evil.example")
            .bearer(&token),
    );
    assert!(!stranger.headers.contains_key("access-control-allow-origin"));
}

#[test]
fn without_an_auth_section_an_allowed_origin_may_post_and_others_may_not() {
    let g = gate_with(false, |cfg| {
        cfg.allowed_origins = vec!["tauri://localhost".into()];
    });
    let allowed = g.send(
        post("/api/v1/projects", json!({}))
            .host("127.0.0.1:7420")
            .header("origin", "tauri://localhost")
            .key("k"),
    );
    assert_ne!(allowed.status, StatusCode::FORBIDDEN, "{}", allowed.text);
    assert_eq!(
        allowed.headers["access-control-allow-origin"],
        "tauri://localhost"
    );
    let other = g.send(
        post("/api/v1/projects", json!({}))
            .host("127.0.0.1:7420")
            .header("origin", "https://evil.example")
            .key("k"),
    );
    problem(&other, StatusCode::FORBIDDEN, "origin_not_allowed");
}
