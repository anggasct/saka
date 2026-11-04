use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::header::{ACCEPT, HeaderMap, LINK, RETRY_AFTER};
use reqwest::{Method, StatusCode, Url};
use saka_core::time::Timestamp;
use saka_ports::{Clock, GitAccess, GitRemote, PortError, PortResult, RepoRef};
use serde::Serialize;
use serde_json::{Value, json};

pub const API_VERSION: &str = "2026-03-10";
const JWT_BACKDATE_SECS: i64 = 60;
const JWT_LIFETIME_SECS: i64 = 540;
const TOKEN_MARGIN_MS: i64 = 5 * 60 * 1000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PAGES: usize = 20;

#[derive(Debug, Clone)]
pub struct GitHubConfig {
    pub integration_id: String,
    pub api_url: String,
    pub web_url: String,
    pub app_id: u64,
    pub private_key_file: PathBuf,
    pub user_agent: String,
    pub client_id: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    expires_at: Timestamp,
}

pub struct GitHubApp {
    pub(super) cfg: GitHubConfig,
    api: Url,
    pub(super) http: reqwest::Client,
    pub(super) clock: Arc<dyn Clock>,
    tokens: Mutex<HashMap<String, CachedToken>>,
}

#[derive(Debug)]
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
    pub raw: Vec<u8>,
}

impl Reply {
    pub fn message(&self) -> String {
        let mut text = self.body["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if let Some(errors) = self.body["errors"].as_array() {
            for e in errors {
                if let Some(m) = e["message"].as_str() {
                    text.push_str("; ");
                    text.push_str(m);
                }
            }
        }
        text
    }
}

#[derive(Serialize)]
struct Claims {
    iat: i64,
    exp: i64,
    iss: String,
}

pub const JSON_MEDIA_TYPE: &str = "application/vnd.github+json";
pub const RAW_MEDIA_TYPE: &str = "application/vnd.github.raw";

pub(super) fn unavailable(status: Option<u16>, detail: &str) -> PortError {
    PortError::retryable(
        "scm_unavailable",
        json!({ "status": status, "detail": detail }),
    )
}

pub(crate) fn inaccessible(full_name: &str, status: u16) -> PortError {
    PortError::permanent(
        "scm_repo_inaccessible",
        json!({ "repo": full_name, "status": status }),
    )
}

pub(crate) fn split_repo(full_name: &str) -> PortResult<(&str, &str)> {
    let ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    match full_name.split_once('/') {
        Some((owner, name)) if ok(owner) && ok(name) => Ok((owner, name)),
        _ => Err(PortError::permanent(
            "scm_repo_name_invalid",
            json!({ "repo": full_name }),
        )),
    }
}

fn next_link(headers: &HeaderMap) -> Option<String> {
    let link = headers.get(LINK)?.to_str().ok()?;
    link.split(',').find_map(|part| {
        let (url, rest) = part.trim().split_once(';')?;
        rest.contains("rel=\"next\"").then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
    })
}

pub(super) fn rate_limit_reset(headers: &HeaderMap, now: Timestamp) -> Option<Timestamp> {
    let header = |name: &str| headers.get(name)?.to_str().ok()?.trim().parse::<i64>().ok();
    if let Some(secs) = headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok())
    {
        return Some(now.plus_ms(secs * 1000));
    }
    if header("x-ratelimit-remaining") == Some(0) {
        return header("x-ratelimit-reset").map(|s| Timestamp(s * 1000));
    }
    None
}

pub(crate) fn parse_time(value: &Value) -> Option<Timestamp> {
    let ts: jiff::Timestamp = value.as_str()?.parse().ok()?;
    Some(Timestamp(ts.as_millisecond()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum As {
    App,
    Installation,
    User,
}

impl GitHubApp {
    pub fn new(cfg: GitHubConfig, clock: Arc<dyn Clock>) -> PortResult<GitHubApp> {
        let mut api = Url::parse(cfg.api_url.trim_end_matches('/')).map_err(|_| {
            PortError::permanent("scm_config_invalid", json!({ "field": "api_url" }))
        })?;
        if api.cannot_be_a_base() {
            return Err(PortError::permanent(
                "scm_config_invalid",
                json!({ "field": "api_url" }),
            ));
        }
        if !api.path().ends_with('/') {
            let path = format!("{}/", api.path());
            api.set_path(&path);
        }
        let http = reqwest::Client::builder()
            .user_agent(cfg.user_agent.clone())
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| {
                PortError::permanent("scm_config_invalid", json!({ "error": e.to_string() }))
            })?;
        Ok(GitHubApp {
            cfg,
            api,
            http,
            clock,
            tokens: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &GitHubConfig {
        &self.cfg
    }

    pub(crate) fn url(&self, segments: &[&str]) -> PortResult<Url> {
        let mut url = self.api.clone();
        url.path_segments_mut()
            .map_err(|_| PortError::permanent("scm_config_invalid", json!({ "field": "api_url" })))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    pub fn jwt(&self) -> PortResult<String> {
        let path = &self.cfg.private_key_file;
        let key_error = |reason: &str| {
            PortError::permanent(
                "config_key_file_unreadable",
                json!({ "path": path.to_string_lossy(), "reason": reason }),
            )
        };
        let meta = std::fs::metadata(path).map_err(|_| key_error("missing"))?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(PortError::permanent(
                "config_key_file_mode",
                json!({ "path": path.to_string_lossy() }),
            ));
        }
        let pem = std::fs::read(path).map_err(|_| key_error("unreadable"))?;
        let key = EncodingKey::from_rsa_pem(&pem).map_err(|_| key_error("not_an_rsa_key"))?;
        let now_secs = self.clock.now().0.div_euclid(1000);
        let claims = Claims {
            iat: now_secs - JWT_BACKDATE_SECS,
            exp: now_secs + JWT_LIFETIME_SECS,
            iss: self.cfg.app_id.to_string(),
        };
        jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &key)
            .map_err(|_| key_error("signing_failed"))
    }

    pub async fn token(&self, full_name: &str) -> PortResult<String> {
        let now = self.clock.now();
        if let Some(cached) = self.cached(full_name)
            && now.0 < cached.expires_at.0 - TOKEN_MARGIN_MS
        {
            return Ok(cached.token);
        }
        let (owner, name) = split_repo(full_name)?;
        let found = self
            .send(
                Method::GET,
                self.url(&["repos", owner, name, "installation"])?,
                As::App,
                self.jwt()?,
                None,
            )
            .await?;
        if found.status == StatusCode::NOT_FOUND {
            return Err(inaccessible(full_name, 404));
        }
        if !found.status.is_success() {
            return Err(self.unexpected("installation", &found));
        }
        let id = found.body["id"]
            .as_u64()
            .ok_or_else(|| unavailable(Some(found.status.as_u16()), "installation_id_missing"))?;
        let id = id.to_string();
        let url = self.url(&["app", "installations", &id, "access_tokens"])?;
        let body = json!({ "repositories": [name] });
        let issued = self
            .send(Method::POST, url, As::App, self.jwt()?, Some(&body))
            .await?;
        if matches!(
            issued.status,
            StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY
        ) {
            return Err(inaccessible(full_name, issued.status.as_u16()));
        }
        if !issued.status.is_success() {
            return Err(self.unexpected("access_token", &issued));
        }
        let token = issued.body["token"]
            .as_str()
            .ok_or_else(|| unavailable(Some(issued.status.as_u16()), "token_missing"))?
            .to_string();
        let expires_at = parse_time(&issued.body["expires_at"])
            .unwrap_or_else(|| now.plus_ms(TOKEN_MARGIN_MS * 2));
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.insert(
                full_name.to_string(),
                CachedToken {
                    token: token.clone(),
                    expires_at,
                },
            );
        }
        Ok(token)
    }

    fn cached(&self, full_name: &str) -> Option<CachedToken> {
        self.tokens.lock().ok()?.get(full_name).cloned()
    }

    pub fn forget_token(&self, full_name: &str) {
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.remove(full_name);
        }
    }

    pub async fn call(
        &self,
        full_name: &str,
        method: Method,
        url: Url,
        body: Option<&Value>,
    ) -> PortResult<Reply> {
        self.call_accepting(full_name, method, url, body, JSON_MEDIA_TYPE)
            .await
    }

    pub async fn call_accepting(
        &self,
        full_name: &str,
        method: Method,
        url: Url,
        body: Option<&Value>,
        accept: &'static str,
    ) -> PortResult<Reply> {
        for attempt in 0..2 {
            let token = self.token(full_name).await?;
            let reply = self
                .send_accepting(
                    method.clone(),
                    url.clone(),
                    As::Installation,
                    token,
                    body,
                    accept,
                )
                .await?;
            if reply.status != StatusCode::UNAUTHORIZED {
                return Ok(reply);
            }
            self.forget_token(full_name);
            if attempt == 1 {
                return Err(PortError::permanent(
                    "scm_auth_refused",
                    json!({ "repo": full_name, "status": 401 }),
                ));
            }
        }
        Err(PortError::permanent(
            "scm_auth_refused",
            json!({ "repo": full_name, "status": 401 }),
        ))
    }

    pub async fn pages(
        &self,
        full_name: &str,
        first: Url,
        items: Option<&str>,
    ) -> PortResult<Vec<Value>> {
        let mut all = Vec::new();
        let mut url = Some(first);
        for _ in 0..MAX_PAGES {
            let Some(current) = url.take() else { break };
            let reply = self.call(full_name, Method::GET, current, None).await?;
            if !reply.status.is_success() {
                return Err(self.unexpected("list", &reply));
            }
            let list = match items {
                Some(key) => reply.body[key].as_array().cloned(),
                None => reply.body.as_array().cloned(),
            };
            all.extend(list.unwrap_or_default());
            url = next_link(&reply.headers)
                .and_then(|n| Url::parse(&n).ok())
                .filter(|n| {
                    n.origin() == self.api.origin() && n.path().starts_with(self.api.path())
                });
        }
        Ok(all)
    }

    pub(super) async fn send(
        &self,
        method: Method,
        url: Url,
        who: As,
        bearer: String,
        body: Option<&Value>,
    ) -> PortResult<Reply> {
        self.send_accepting(method, url, who, bearer, body, JSON_MEDIA_TYPE)
            .await
    }

    async fn send_accepting(
        &self,
        method: Method,
        url: Url,
        who: As,
        bearer: String,
        body: Option<&Value>,
        accept: &'static str,
    ) -> PortResult<Reply> {
        let path = url.path().to_string();
        let mut request = self
            .http
            .request(method.clone(), url)
            .bearer_auth(bearer)
            .header(ACCEPT, accept)
            .header("X-GitHub-Api-Version", API_VERSION);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|e| {
            let detail = if e.is_timeout() {
                "timeout"
            } else {
                "transport"
            };
            tracing::warn!(method = %method, path = %path, detail, "github request failed");
            unavailable(None, detail)
        })?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .bytes()
            .await
            .map_err(|_| unavailable(Some(status.as_u16()), "body"))?;
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        tracing::debug!(
            method = %method,
            path = %path,
            status = status.as_u16(),
            ratelimit_remaining = headers.get("x-ratelimit-remaining").and_then(|v| v.to_str().ok()),
            "github request"
        );
        let limited = status == StatusCode::TOO_MANY_REQUESTS
            || (status == StatusCode::FORBIDDEN
                && (headers.contains_key(RETRY_AFTER)
                    || headers
                        .get("x-ratelimit-remaining")
                        .and_then(|v| v.to_str().ok())
                        == Some("0")));
        if limited {
            let mut e = unavailable(Some(status.as_u16()), "rate_limited");
            e.retry_at = rate_limit_reset(&headers, self.clock.now());
            return Err(e);
        }
        if status.is_server_error() {
            return Err(unavailable(Some(status.as_u16()), "server_error"));
        }
        if status == StatusCode::UNAUTHORIZED && who == As::App {
            return Err(PortError::permanent(
                "scm_app_auth_refused",
                json!({ "app_id": self.cfg.app_id, "status": 401 }),
            ));
        }
        Ok(Reply {
            status,
            headers,
            body,
            raw: bytes.to_vec(),
        })
    }

    pub(crate) fn unexpected(&self, op: &str, reply: &Reply) -> PortError {
        PortError::permanent(
            "scm_rejected",
            json!({ "op": op, "status": reply.status.as_u16(), "message": reply.message() }),
        )
    }

    fn remote_url(&self, full_name: &str) -> String {
        format!("{}/{full_name}.git", self.cfg.web_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl GitAccess for GitHubApp {
    async fn remote(&self, repo: &RepoRef) -> PortResult<GitRemote> {
        split_repo(&repo.full_name)?;
        let token = self.token(&repo.full_name).await?;
        let basic =
            base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"));
        Ok(GitRemote {
            url: self.remote_url(&repo.full_name),
            config: vec![(
                format!(
                    "http.{}/.extraheader",
                    self.cfg.web_url.trim_end_matches('/')
                ),
                format!("AUTHORIZATION: basic {basic}"),
            )],
        })
    }

    async fn refused(&self, repo: &RepoRef) {
        self.forget_token(&repo.full_name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_rate_limits_are_read_from_headers() {
        let mut h = HeaderMap::new();
        h.insert(
            LINK,
            "<https://api.github.com/x?page=2>; rel=\"next\", <https://api.github.com/x?page=5>; rel=\"last\""
                .parse()
                .unwrap(),
        );
        assert_eq!(
            next_link(&h).as_deref(),
            Some("https://api.github.com/x?page=2")
        );
        h.insert("x-ratelimit-remaining", "0".parse().unwrap());
        h.insert("x-ratelimit-reset", "1767230000".parse().unwrap());
        assert_eq!(
            rate_limit_reset(&h, Timestamp(0)),
            Some(Timestamp(1_767_230_000_000))
        );
        h.insert(RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(
            rate_limit_reset(&h, Timestamp(1000)),
            Some(Timestamp(31_000))
        );
    }

    #[test]
    fn repo_names_are_two_plain_segments() {
        assert_eq!(split_repo("acme/app").unwrap(), ("acme", "app"));
        for bad in [
            "acme",
            "acme/",
            "/app",
            "acme/../x",
            "acme/app/x",
            "acme/a b",
            "../app",
        ] {
            assert!(split_repo(bad).is_err(), "{bad}");
        }
        assert_eq!(
            parse_time(&json!("2026-01-01T01:00:00Z")),
            Some(Timestamp(1_767_229_200_000))
        );
    }
}
