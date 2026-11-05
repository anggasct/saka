use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, RETRY_AFTER};
use reqwest::{Method, StatusCode, Url};
use saka_core::time::Timestamp;
use saka_ports::{Clock, GitAccess, GitRemote, PortError, PortResult, RepoRef};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::github::app::unavailable;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PAGES: usize = 20;
const PAGE_ITEMS: usize = 100;
const PAGE_SIZE: &str = "100";
const MAX_PROJECT_SEGMENTS: usize = 21;

#[derive(Debug, Clone)]
pub struct GitLabConfig {
    pub integration_id: String,
    pub api_url: String,
    pub web_url: String,
    pub token_file: PathBuf,
    pub user_agent: String,
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
        let field = |key: &str| match &self.body[key] {
            Value::String(s) => Some(s.clone()),
            Value::Array(items) => Some(
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            Value::Object(_) => Some(self.body[key].to_string()),
            _ => None,
        };
        field("message")
            .or_else(|| field("error"))
            .unwrap_or_default()
    }
}

pub struct GitLabClient {
    cfg: GitLabConfig,
    api: Url,
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
}

pub(crate) fn project_name_ok(full_name: &str) -> bool {
    let segment = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    let mut count = 0;
    let all_plain = full_name.split('/').all(|s| {
        count += 1;
        segment(s)
    });
    all_plain && (2..=MAX_PROJECT_SEGMENTS).contains(&count)
}

pub(crate) fn check_project(full_name: &str) -> PortResult<()> {
    if project_name_ok(full_name) {
        Ok(())
    } else {
        Err(PortError::permanent(
            "scm_repo_name_invalid",
            json!({ "repo": full_name }),
        ))
    }
}

fn retry_at(headers: &HeaderMap, now: Timestamp) -> Option<Timestamp> {
    let number = |name: &str| headers.get(name)?.to_str().ok()?.trim().parse::<i64>().ok();
    if let Some(secs) = number(RETRY_AFTER.as_str()) {
        return Some(now.plus_ms(secs * 1000));
    }
    number("ratelimit-reset").map(|epoch_secs| Timestamp(epoch_secs * 1000))
}

impl GitLabClient {
    pub fn new(cfg: GitLabConfig, clock: Arc<dyn Clock>) -> PortResult<GitLabClient> {
        let invalid =
            |field: &str| PortError::permanent("scm_config_invalid", json!({ "field": field }));
        let mut api =
            Url::parse(cfg.api_url.trim_end_matches('/')).map_err(|_| invalid("api_url"))?;
        if api.cannot_be_a_base() {
            return Err(invalid("api_url"));
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
        Ok(GitLabClient {
            cfg,
            api,
            http,
            clock,
        })
    }

    pub fn config(&self) -> &GitLabConfig {
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

    pub(crate) fn project_url(&self, full_name: &str, tail: &[&str]) -> PortResult<Url> {
        check_project(full_name)?;
        let mut segments = vec!["projects", full_name];
        segments.extend_from_slice(tail);
        self.url(&segments)
    }

    pub(crate) fn token(&self) -> PortResult<Zeroizing<String>> {
        let path = &self.cfg.token_file;
        let unreadable = |reason: &str| {
            PortError::permanent(
                "config_key_file_unreadable",
                json!({ "path": path.to_string_lossy(), "reason": reason }),
            )
        };
        let meta = std::fs::metadata(path).map_err(|_| unreadable("missing"))?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(PortError::permanent(
                "config_key_file_mode",
                json!({ "path": path.to_string_lossy() }),
            ));
        }
        let raw =
            Zeroizing::new(std::fs::read_to_string(path).map_err(|_| unreadable("unreadable"))?);
        let token = raw.trim();
        if token.is_empty() {
            return Err(unreadable("empty"));
        }
        if !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(unreadable("invalid"));
        }
        Ok(Zeroizing::new(token.to_string()))
    }

    pub(crate) async fn call(
        &self,
        full_name: &str,
        method: Method,
        url: Url,
        body: Option<&Value>,
    ) -> PortResult<Reply> {
        let reply = self.send(method, url, body).await?;
        if reply.status == StatusCode::UNAUTHORIZED {
            return Err(PortError::permanent(
                "scm_auth_refused",
                json!({ "repo": full_name, "status": 401 }),
            ));
        }
        Ok(reply)
    }

    pub(crate) async fn pages(&self, full_name: &str, first: Url) -> PortResult<Vec<Value>> {
        self.pages_up_to(full_name, first, MAX_PAGES * PAGE_ITEMS)
            .await
    }

    pub(crate) async fn pages_up_to(
        &self,
        full_name: &str,
        first: Url,
        max_items: usize,
    ) -> PortResult<Vec<Value>> {
        let mut all = Vec::new();
        let mut url = first;
        url.query_pairs_mut().append_pair("per_page", PAGE_SIZE);
        let mut page = 1_u32;
        for _ in 0..max_items.div_ceil(PAGE_ITEMS).max(1) {
            let mut current = url.clone();
            if page > 1 {
                current
                    .query_pairs_mut()
                    .append_pair("page", &page.to_string());
            }
            let reply = self.call(full_name, Method::GET, current, None).await?;
            if !reply.status.is_success() {
                return Err(self.unexpected("list", &reply));
            }
            all.extend(reply.body.as_array().cloned().unwrap_or_default());
            if all.len() >= max_items {
                all.truncate(max_items);
                break;
            }
            let next = reply
                .headers
                .get("x-next-page")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u32>().ok());
            match next {
                Some(n) if n > page => page = n,
                _ => break,
            }
        }
        Ok(all)
    }

    pub(crate) async fn send(
        &self,
        method: Method,
        url: Url,
        body: Option<&Value>,
    ) -> PortResult<Reply> {
        let mut token = HeaderValue::from_str(&self.token()?).map_err(|_| {
            PortError::permanent(
                "config_key_file_unreadable",
                json!({ "path": self.cfg.token_file.to_string_lossy(), "reason": "invalid" }),
            )
        })?;
        token.set_sensitive(true);
        let path = url.path().to_string();
        let mut request = self
            .http
            .request(method.clone(), url)
            .header("PRIVATE-TOKEN", token)
            .header(ACCEPT, "application/json");
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|e| {
            let detail = if e.is_timeout() {
                "timeout"
            } else {
                "transport"
            };
            tracing::warn!(method = %method, path = %path, detail, "gitlab request failed");
            unavailable(None, detail)
        })?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .bytes()
            .await
            .map_err(|_| unavailable(Some(status.as_u16()), "body"))?;
        let is_json = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("json"));
        let body: Value = if is_json {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        tracing::debug!(method = %method, path = %path, status = status.as_u16(), "gitlab request");
        if status == StatusCode::TOO_MANY_REQUESTS {
            let mut e = unavailable(Some(429), "rate_limited");
            e.retry_at = retry_at(&headers, self.clock.now());
            return Err(e);
        }
        if status.is_server_error() {
            return Err(unavailable(Some(status.as_u16()), "server_error"));
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
}

#[async_trait]
impl GitAccess for GitLabClient {
    async fn remote(&self, repo: &RepoRef) -> PortResult<GitRemote> {
        check_project(&repo.full_name)?;
        let token = self.token()?;
        let basic =
            base64::engine::general_purpose::STANDARD.encode(format!("oauth2:{}", token.as_str()));
        let web = self.cfg.web_url.trim_end_matches('/');
        Ok(GitRemote {
            url: format!("{web}/{}.git", repo.full_name),
            config: vec![(
                format!("http.{web}/.extraheader"),
                format!("AUTHORIZATION: basic {basic}"),
            )],
        })
    }

    async fn refused(&self, _repo: &RepoRef) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_are_two_or_more_plain_segments() {
        for ok in ["acme/app", "group/sub/app", "a.b/c_d-e"] {
            assert!(project_name_ok(ok), "{ok}");
        }
        for bad in [
            "acme",
            "acme/",
            "/app",
            "acme/../x",
            "acme//x",
            "acme/a b",
            "acme/a%2Fb",
            "../app",
        ] {
            assert!(!project_name_ok(bad), "{bad}");
        }
    }

    #[test]
    fn rate_limit_reset_reads_retry_after_then_epoch_headers() {
        let mut h = HeaderMap::new();
        h.insert("ratelimit-reset", "1767230000".parse().unwrap());
        assert_eq!(
            retry_at(&h, Timestamp(0)),
            Some(Timestamp(1_767_230_000_000))
        );
        h.insert(RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(retry_at(&h, Timestamp(1000)), Some(Timestamp(31_000)));
        assert_eq!(retry_at(&HeaderMap::new(), Timestamp(0)), None);
    }
}
