use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, StatusCode, header};
use saka_core::identity::OriginParts;
use saka_core::secret::SecretText;
use saka_engine::Principal;
use serde_json::json;
use webauthn_rs::prelude::{
    PasskeyAuthentication, PasskeyRegistration, Url, Webauthn, WebauthnBuilder,
};

use crate::guard::loopback_host;
use crate::problem::ApiError;
use crate::state::AppState;

pub const SESSION_COOKIE: &str = "saka_session";
pub const ANONYMOUS: &str = "anonymous";

const CEREMONY_TTL_MS: i64 = 5 * 60 * 1000;
const MAX_CEREMONIES: usize = 256;
const WINDOW_MS: i64 = 60_000;
const MAX_FAILURES: usize = 10;
const MAX_TRACKED_CLIENTS: usize = 4096;

tokio::task_local! {
    pub static ACTOR: String;
}

pub fn actor() -> String {
    ACTOR
        .try_with(Clone::clone)
        .unwrap_or_else(|_| ANONYMOUS.to_string())
}

#[derive(Debug, Clone)]
pub struct Caller(pub Option<Principal>);

#[derive(Debug, Clone)]
pub struct AuthSettings {
    pub origin: OriginParts,
    pub rp_id: String,
    pub rp_name: String,
    pub session_hours: u32,
}

pub(crate) enum CeremonyKind {
    Register {
        state: Box<PasskeyRegistration>,
        name: String,
        code: Option<SecretText>,
    },
    Login {
        state: Box<PasskeyAuthentication>,
    },
}

struct Ceremony {
    kind: CeremonyKind,
    started_ms: i64,
}

pub struct Authenticator {
    settings: AuthSettings,
    limiter: Mutex<Limiter>,
    webauthn: Webauthn,
    ceremonies: Mutex<HashMap<String, Ceremony>>,
}

impl std::fmt::Debug for Authenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authenticator")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl Authenticator {
    pub fn new(settings: AuthSettings) -> Result<Arc<Authenticator>, String> {
        let origin = Url::parse(&settings.origin.serialized()).map_err(|e| e.to_string())?;
        let webauthn = WebauthnBuilder::new(&settings.rp_id, &origin)
            .map(|b| b.rp_name(&settings.rp_name))
            .and_then(WebauthnBuilder::build)
            .map_err(|e| format!("{e:?}"))?;
        Ok(Arc::new(Authenticator {
            settings,
            limiter: Mutex::new(Limiter::default()),
            webauthn,
            ceremonies: Mutex::new(HashMap::new()),
        }))
    }

    pub(crate) fn webauthn(&self) -> &Webauthn {
        &self.webauthn
    }

    pub(crate) fn begin(&self, id: String, kind: CeremonyKind, now_ms: i64) {
        let mut held = self
            .ceremonies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.retain(|_, c| now_ms.saturating_sub(c.started_ms) < CEREMONY_TTL_MS);
        if held.len() >= MAX_CEREMONIES
            && let Some(oldest) = held
                .iter()
                .min_by_key(|(_, c)| c.started_ms)
                .map(|(k, _)| k.clone())
        {
            held.remove(&oldest);
        }
        held.insert(
            id,
            Ceremony {
                kind,
                started_ms: now_ms,
            },
        );
    }

    pub(crate) fn finish(&self, id: &str, now_ms: i64) -> Option<CeremonyKind> {
        let ceremony = self
            .ceremonies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id)?;
        (now_ms.saturating_sub(ceremony.started_ms) < CEREMONY_TTL_MS).then_some(ceremony.kind)
    }

    pub fn settings(&self) -> &AuthSettings {
        &self.settings
    }

    fn host_allowed(&self, host: &str) -> bool {
        if loopback_host(host) {
            return true;
        }
        let (name, port) = split_host(host);
        let own = &self.settings.origin;
        name.eq_ignore_ascii_case(&own.host)
            && port.unwrap_or(own_default_port(own)) == own.effective_port()
    }

    fn origin_allowed(&self, origin: &str, allowed: &[String]) -> bool {
        let origin = origin.trim_end_matches('/');
        origin == self.settings.origin.serialized()
            || allowed.iter().any(|o| o.trim_end_matches('/') == origin)
    }

    pub(crate) fn blocked(&self, client: ClientKey, now_ms: i64) -> Option<u64> {
        self.limiter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .blocked(client, now_ms)
    }

    pub(crate) fn failed(&self, client: ClientKey, now_ms: i64) {
        self.limiter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(client, now_ms);
    }
}

fn own_default_port(own: &OriginParts) -> u16 {
    match own.scheme {
        saka_core::identity::Scheme::Http => 80,
        saka_core::identity::Scheme::Https => 443,
    }
}

fn split_host(host: &str) -> (&str, Option<u16>) {
    match host.rsplit_once(':') {
        Some((name, port)) if !host.ends_with(']') => (name, port.parse().ok()),
        _ => (host, None),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ClientKey {
    V4([u8; 4]),
    V6Prefix([u8; 8]),
    Unknown,
}

impl ClientKey {
    pub(crate) fn of(addr: Option<SocketAddr>) -> ClientKey {
        match addr.map(|a| a.ip()) {
            Some(IpAddr::V4(ip)) => ClientKey::V4(ip.octets()),
            Some(IpAddr::V6(ip)) => match ip.to_ipv4_mapped() {
                Some(v4) => ClientKey::V4(v4.octets()),
                None => {
                    let o = ip.octets();
                    ClientKey::V6Prefix([o[0], o[1], o[2], o[3], o[4], o[5], o[6], o[7]])
                }
            },
            None => ClientKey::Unknown,
        }
    }
}

#[derive(Debug, Default)]
struct Limiter {
    failures: HashMap<ClientKey, VecDeque<i64>>,
}

impl Limiter {
    fn prune(queue: &mut VecDeque<i64>, now_ms: i64) {
        while queue
            .front()
            .is_some_and(|t| now_ms.saturating_sub(*t) >= WINDOW_MS)
        {
            queue.pop_front();
        }
    }

    fn blocked(&mut self, client: ClientKey, now_ms: i64) -> Option<u64> {
        let queue = self.failures.get_mut(&client)?;
        Limiter::prune(queue, now_ms);
        if queue.len() < MAX_FAILURES {
            return None;
        }
        let oldest = queue.front().copied().unwrap_or(now_ms);
        let wait_ms = (oldest + WINDOW_MS - now_ms).max(1);
        Some(u64::try_from((wait_ms + 999) / 1000).unwrap_or(1))
    }

    fn record(&mut self, client: ClientKey, now_ms: i64) {
        if self.failures.len() >= MAX_TRACKED_CLIENTS && !self.failures.contains_key(&client) {
            self.failures.retain(|_, q| {
                Limiter::prune(q, now_ms);
                !q.is_empty()
            });
            if self.failures.len() >= MAX_TRACKED_CLIENTS
                && let Some(oldest) = self
                    .failures
                    .iter()
                    .min_by_key(|(_, q)| q.back().copied().unwrap_or(0))
                    .map(|(k, _)| *k)
            {
                self.failures.remove(&oldest);
            }
        }
        let queue = self.failures.entry(client).or_default();
        Limiter::prune(queue, now_ms);
        queue.push_back(now_ms);
    }
}

pub(crate) fn limited_path(path: &str) -> bool {
    path == "/api/v1/auth/pair"
        || path.starts_with("/api/v1/auth/login/")
        || path.starts_with("/api/v1/auth/passkeys/")
}

pub(crate) fn peer(req: &axum::extract::Request) -> Option<SocketAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0)
}

const OPEN_GET: &[&str] = &["/healthz", "/readyz", "/api/v1/meta", "/api/v1/auth/status"];

const OPEN_POST: &[&str] = &[
    "/api/v1/auth/pair",
    "/api/v1/auth/passkeys/register/start",
    "/api/v1/auth/passkeys/register/finish",
    "/api/v1/auth/login/start",
    "/api/v1/auth/login/finish",
];

pub(crate) fn open_route(method: &Method, matched: Option<&str>, path: &str) -> bool {
    let read = method == Method::GET || method == Method::HEAD;
    match matched {
        Some(route) => {
            (read && OPEN_GET.contains(&route))
                || (method == Method::POST && OPEN_POST.contains(&route))
        }
        None => read && path != "/metrics" && path != "/api" && !path.starts_with("/api/"),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Presented {
    Nothing,
    Bearer(String),
    Cookie(String),
    Malformed,
}

pub(crate) fn presented(headers: &HeaderMap) -> Presented {
    let mut authorization = headers.get_all(header::AUTHORIZATION).iter();
    if let Some(value) = authorization.next() {
        if authorization.next().is_some() {
            return Presented::Malformed;
        }
        let Ok(value) = value.to_str() else {
            return Presented::Malformed;
        };
        return match value.split_once(' ') {
            Some((scheme, token))
                if scheme.eq_ignore_ascii_case("bearer")
                    && !token.is_empty()
                    && !token.contains(char::is_whitespace) =>
            {
                Presented::Bearer(token.to_string())
            }
            _ => Presented::Malformed,
        };
    }
    let mut found: Option<&str> = None;
    for value in headers.get_all(header::COOKIE) {
        let Ok(value) = value.to_str() else {
            return Presented::Malformed;
        };
        for pair in value.split(';') {
            let Some((name, cookie)) = pair.trim().split_once('=') else {
                continue;
            };
            if name != SESSION_COOKIE {
                continue;
            }
            if found.is_some_and(|f| f != cookie) {
                return Presented::Malformed;
            }
            found = Some(cookie);
        }
    }
    match found {
        Some(cookie) if !cookie.is_empty() => Presented::Cookie(cookie.to_string()),
        Some(_) => Presented::Malformed,
        None => Presented::Nothing,
    }
}

enum Identity {
    Nothing,
    Valid(Principal),
    Invalid,
}

fn identify(state: &AppState, presented: &Presented) -> Result<Identity, ApiError> {
    match presented {
        Presented::Nothing => Ok(Identity::Nothing),
        Presented::Malformed => Ok(Identity::Invalid),
        Presented::Bearer(token) => {
            let engine = state.engine()?;
            Ok(engine
                .authenticate_token(token)?
                .map_or(Identity::Invalid, Identity::Valid))
        }
        Presented::Cookie(cookie) => {
            let engine = state.engine()?;
            Ok(engine
                .authenticate_session(cookie)?
                .map_or(Identity::Invalid, Identity::Valid))
        }
    }
}

fn refusal(status: StatusCode, code: &str) -> ApiError {
    ApiError::new(status, code, json!({}))
}

pub(crate) fn admit(
    state: &AppState,
    auth: &Authenticator,
    req: &axum::extract::Request,
) -> Result<Option<Principal>, ApiError> {
    let headers = req.headers();
    let host_ok = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|h| auth.host_allowed(h));
    if !host_ok {
        return Err(refusal(StatusCode::FORBIDDEN, "host_not_allowed"));
    }
    let presented = presented(headers);
    if req.method() == Method::POST
        && !matches!(presented, Presented::Bearer(_) | Presented::Malformed)
    {
        let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
        let cookie = matches!(presented, Presented::Cookie(_));
        let allowed = match origin {
            Some(o) => auth.origin_allowed(o, &state.config().allowed_origins),
            None => !cookie,
        };
        if !allowed {
            return Err(refusal(StatusCode::FORBIDDEN, "origin_not_allowed"));
        }
    }
    let matched = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(axum::extract::MatchedPath::as_str);
    let open = open_route(req.method(), matched, req.uri().path());
    let identity = match identify(state, &presented) {
        Ok(identity) => identity,
        Err(e) if !open => return Err(e),
        Err(_) => Identity::Nothing,
    };
    match (identity, open) {
        (Identity::Valid(principal), _) => Ok(Some(principal)),
        (_, true) => Ok(None),
        (Identity::Nothing, false) => Err(refusal(StatusCode::UNAUTHORIZED, "auth_required")),
        (Identity::Invalid, false) => Err(refusal(StatusCode::UNAUTHORIZED, "auth_invalid")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(k.clone(), HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn a_bearer_token_wins_over_a_cookie() {
        let h = headers(&[
            (header::AUTHORIZATION, "Bearer sdt_abc"),
            (header::COOKIE, "saka_session=sss_x"),
        ]);
        assert_eq!(presented(&h), Presented::Bearer("sdt_abc".into()));
    }

    #[test]
    fn anything_but_one_well_formed_bearer_is_malformed() {
        for value in ["Basic abc", "Bearer", "Bearer  x", "Bearer a b", "abc"] {
            let h = headers(&[(header::AUTHORIZATION, value)]);
            assert_eq!(presented(&h), Presented::Malformed, "{value}");
        }
        let twice = headers(&[
            (header::AUTHORIZATION, "Bearer a"),
            (header::AUTHORIZATION, "Bearer a"),
        ]);
        assert_eq!(presented(&twice), Presented::Malformed);
    }

    #[test]
    fn two_different_session_cookies_are_malformed() {
        let h = headers(&[(header::COOKIE, "saka_session=a; other=1; saka_session=b")]);
        assert_eq!(presented(&h), Presented::Malformed);
        let same = headers(&[(header::COOKIE, "other=1; saka_session=a")]);
        assert_eq!(presented(&same), Presented::Cookie("a".into()));
        assert_eq!(presented(&HeaderMap::new()), Presented::Nothing);
    }

    #[test]
    fn only_listed_routes_and_static_files_are_open() {
        assert!(open_route(&Method::GET, Some("/healthz"), "/healthz"));
        assert!(open_route(
            &Method::GET,
            Some("/api/v1/meta"),
            "/api/v1/meta"
        ));
        assert!(open_route(&Method::GET, None, "/settings/abc"));
        assert!(open_route(
            &Method::POST,
            Some("/api/v1/auth/pair"),
            "/api/v1/auth/pair"
        ));
        assert!(!open_route(&Method::GET, Some("/metrics"), "/metrics"));
        assert!(!open_route(&Method::GET, None, "/metrics"));
        assert!(!open_route(&Method::GET, None, "/api/v1/nope"));
        assert!(!open_route(
            &Method::GET,
            Some("/api/{*rest}"),
            "/api/v1/meta2"
        ));
        assert!(!open_route(
            &Method::GET,
            Some("/api/v1/projects"),
            "/api/v1/projects"
        ));
        assert!(!open_route(&Method::POST, None, "/anything"));
        assert!(!open_route(
            &Method::POST,
            Some("/api/v1/meta"),
            "/api/v1/meta"
        ));
    }

    #[test]
    fn the_eleventh_failure_in_a_minute_is_blocked_until_the_oldest_ages_out() {
        let mut limiter = Limiter::default();
        let client = ClientKey::V4([10, 0, 0, 1]);
        for i in 0..10 {
            assert_eq!(limiter.blocked(client, i * 1000), None);
            limiter.record(client, i * 1000);
        }
        assert_eq!(limiter.blocked(client, 10_000), Some(50));
        assert_eq!(limiter.blocked(ClientKey::V4([10, 0, 0, 2]), 10_000), None);
        assert_eq!(limiter.blocked(client, 60_000), None);
    }

    #[test]
    fn ipv6_clients_share_a_slot_per_sixty_four_bit_prefix() {
        let a: SocketAddr = "[2001:db8:1:2::1]:80".parse().unwrap();
        let b: SocketAddr = "[2001:db8:1:2:ffff::9]:80".parse().unwrap();
        let c: SocketAddr = "[2001:db8:1:3::1]:80".parse().unwrap();
        assert_eq!(ClientKey::of(Some(a)), ClientKey::of(Some(b)));
        assert_ne!(ClientKey::of(Some(a)), ClientKey::of(Some(c)));
        let mapped: SocketAddr = "[::ffff:10.0.0.1]:80".parse().unwrap();
        assert_eq!(ClientKey::of(Some(mapped)), ClientKey::V4([10, 0, 0, 1]));
    }

    #[test]
    fn the_tracked_clients_stay_bounded() {
        let mut limiter = Limiter::default();
        for n in 0..(MAX_TRACKED_CLIENTS as u32 + 50) {
            let b = n.to_be_bytes();
            limiter.record(ClientKey::V4(b), i64::from(n));
        }
        assert!(limiter.failures.len() <= MAX_TRACKED_CLIENTS);
    }
}
