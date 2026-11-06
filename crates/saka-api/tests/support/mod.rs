#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "each test file uses part of the harness; tests fail loudly on a broken setup"
)]

pub mod softtoken;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use saka_api::{ApiConfig, AppState, AuthSettings, Authenticator, router};
use saka_core::identity::parse_origin;
use saka_testkit::{Sim, SimConfig};
use serde_json::{Value, json};
use tower::ServiceExt;

pub const ORIGIN: &str = "https://saka.example.com";
pub const HOST: &str = "saka.example.com";
pub const PEER: &str = "10.1.2.3:5555";

#[derive(Clone, Default)]
pub struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub text: String,
    pub body: Value,
}

pub struct Call<'a> {
    pub method: Method,
    pub path: &'a str,
    pub host: &'a str,
    pub peer: Option<&'a str>,
    pub headers: Vec<(&'a str, String)>,
    pub body: Option<Value>,
}

pub fn get<'a>(path: &'a str) -> Call<'a> {
    Call {
        method: Method::GET,
        path,
        host: HOST,
        peer: Some(PEER),
        headers: Vec::new(),
        body: None,
    }
}

pub fn post<'a>(path: &'a str, body: Value) -> Call<'a> {
    Call {
        method: Method::POST,
        body: Some(body),
        ..get(path)
    }
}

impl<'a> Call<'a> {
    pub fn host(mut self, host: &'a str) -> Call<'a> {
        self.host = host;
        self
    }

    pub fn peer(mut self, peer: &'a str) -> Call<'a> {
        self.peer = Some(peer);
        self
    }

    pub fn header(mut self, name: &'a str, value: impl Into<String>) -> Call<'a> {
        self.headers.push((name, value.into()));
        self
    }

    pub fn bearer(self, token: &str) -> Call<'a> {
        self.header("authorization", format!("Bearer {token}"))
    }

    pub fn key(self, key: &str) -> Call<'a> {
        self.header("idempotency-key", key)
    }
}

pub struct Gate {
    pub sim: Sim,
    pub app: Router,
}

pub fn gate_with(auth: bool, tune: impl FnOnce(&mut ApiConfig)) -> Gate {
    saka_testkit::logs::check_callsites_per_thread();
    let sim = Sim::new(SimConfig::default());
    let mut cfg = ApiConfig::new("0.1.0-test");
    cfg.allowed_origins = vec!["https://app.example.com".into()];
    if auth {
        cfg.auth = Some(
            Authenticator::new(AuthSettings {
                origin: parse_origin(ORIGIN).unwrap(),
                rp_id: "example.com".into(),
                rp_name: "Saka".into(),
                session_hours: 168,
            })
            .unwrap(),
        );
    }
    tune(&mut cfg);
    let state = AppState::new(cfg);
    state.set_engine(sim.engine());
    let app = router(state);
    Gate { sim, app }
}

pub fn gate() -> Gate {
    gate_with(true, |_| {})
}

impl Gate {
    pub fn send(&self, call: Call<'_>) -> Reply {
        let mut req = Request::builder()
            .method(call.method)
            .uri(call.path)
            .header("host", call.host)
            .header("content-type", "application/json");
        for (name, value) in call.headers {
            req = req.header(name, value);
        }
        let body = call
            .body
            .map_or_else(Body::empty, |b| Body::from(b.to_string()));
        let mut req = req.body(body).unwrap();
        if let Some(peer) = call.peer {
            let addr: SocketAddr = peer.parse().unwrap();
            req.extensions_mut().insert(ConnectInfo(addr));
        }
        self.sim.block_on(async {
            let response = self.app.clone().oneshot(req).await.unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            Reply {
                status,
                headers,
                text: String::from_utf8_lossy(&bytes).into_owned(),
                body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            }
        })
    }

    pub fn cli_token(&self) -> String {
        self.sim
            .engine()
            .issue_local_cli_token()
            .unwrap()
            .expose()
            .to_string()
    }

    pub fn pairing_code(&self, token: &str, purpose: &str, key: &str) -> String {
        let r = self.send(
            post("/api/v1/auth/pairings", json!({ "purpose": purpose }))
                .bearer(token)
                .key(key),
        );
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        r.body["code"].as_str().unwrap().to_string()
    }

    pub fn paired_device(&self, token: &str, name: &str) -> (String, String) {
        let code = self.pairing_code(token, "token", &format!("pair-{name}"));
        let r = self.send(post(
            "/api/v1/auth/pair",
            json!({ "code": code, "name": name }),
        ));
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        (
            r.body["device_id"].as_str().unwrap().to_string(),
            r.body["token"].as_str().unwrap().to_string(),
        )
    }

    pub fn events(&self) -> Vec<saka_engine::StoredEvent> {
        self.sim.engine().events_after(0, 1000).unwrap()
    }
}

pub fn problem(r: &Reply, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.text);
    assert_eq!(r.body["code"], code, "{}", r.text);
}
