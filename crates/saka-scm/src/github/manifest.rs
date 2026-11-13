use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use jsonwebtoken::EncodingKey;
use reqwest::header::ACCEPT;
use reqwest::{StatusCode, Url};
use serde_json::{Map, Value, json};
use zeroize::Zeroizing;

use super::app::{API_VERSION, JSON_MEDIA_TYPE, client_id_valid};
use super::requirements::{manifest_events, manifest_permissions};

pub const CONVERSION_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 1 << 20;
const WEBHOOK_SECRET_BYTES: std::ops::RangeInclusive<usize> = 32..=1024;
const HOMEPAGE_URL: &str = "https://github.com/anggasct/saka";

#[derive(Debug, Clone, Copy)]
pub struct ManifestRequest<'a> {
    pub name: &'a str,
    pub integration_id: &'a str,
    pub redirect_url: &'a str,
    pub webhook_url: Option<&'a str>,
}

#[must_use]
pub fn manifest(request: &ManifestRequest<'_>) -> Value {
    let mut doc = Map::new();
    doc.insert("name".into(), request.name.into());
    doc.insert("url".into(), HOMEPAGE_URL.into());
    doc.insert(
        "description".into(),
        format!(
            "Saka delivery loop (integration {})",
            request.integration_id
        )
        .into(),
    );
    doc.insert("public".into(), false.into());
    doc.insert("redirect_url".into(), request.redirect_url.into());
    doc.insert("default_permissions".into(), json!(manifest_permissions()));
    doc.insert("request_oauth_on_install".into(), false.into());
    doc.insert("setup_on_update".into(), false.into());
    if let Some(url) = request.webhook_url {
        doc.insert(
            "hook_attributes".into(),
            json!({ "url": url, "active": true }),
        );
        doc.insert("default_events".into(), json!(manifest_events()));
    }
    Value::Object(doc)
}

fn base(web_url: &str) -> &str {
    web_url.trim_end_matches('/')
}

fn apps_root(web_url: &str, org: Option<&str>) -> String {
    match org {
        Some(org) => format!("{}/organizations/{org}/settings/apps", base(web_url)),
        None => format!("{}/settings/apps", base(web_url)),
    }
}

pub fn form_action(web_url: &str, org: Option<&str>, state: &str) -> Result<String, ManifestError> {
    let mut url = Url::parse(&format!("{}/new", apps_root(web_url, org)))
        .map_err(|_| ManifestError::UrlInvalid { field: "web_url" })?;
    url.query_pairs_mut().append_pair("state", state);
    Ok(url.into())
}

#[must_use]
pub fn apps_url(web_url: &str, org: Option<&str>) -> String {
    apps_root(web_url, org)
}

#[must_use]
pub fn settings_url(web_url: &str, org: Option<&str>, slug: &str) -> String {
    format!("{}/{slug}", apps_root(web_url, org))
}

#[derive(Clone, PartialEq, Eq)]
pub struct CreatedApp {
    pub id: u64,
    pub slug: String,
    pub name: String,
    pub owner: String,
    pub html_url: String,
    pub client_id: String,
    pub pem: Zeroizing<String>,
    pub webhook_secret: Option<Zeroizing<String>>,
    pub permissions: BTreeMap<String, String>,
    pub events: Vec<String>,
}

impl fmt::Debug for CreatedApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatedApp")
            .field("id", &self.id)
            .field("slug", &self.slug)
            .field("name", &self.name)
            .field("owner", &self.owner)
            .field("html_url", &self.html_url)
            .field("client_id", &self.client_id)
            .field("pem", &"[redacted]")
            .field(
                "webhook_secret",
                &self.webhook_secret.as_ref().map(|_| "[redacted]"),
            )
            .field("permissions", &self.permissions)
            .field("events", &self.events)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("the manifest code was rejected")]
    CodeInvalid { status: u16 },
    #[error("the SCM provider is unavailable")]
    Unavailable {
        status: Option<u16>,
        reason: Option<&'static str>,
    },
    #[error("the manifest conversion response is invalid")]
    ResponseInvalid {
        field: &'static str,
        id: Option<u64>,
        slug: Option<String>,
    },
    #[error("a URL is invalid")]
    UrlInvalid { field: &'static str },
}

impl ManifestError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            ManifestError::CodeInvalid { .. } => "app_manifest_code_invalid",
            ManifestError::Unavailable { .. } => "scm_unavailable",
            ManifestError::ResponseInvalid { .. } => "app_manifest_response_invalid",
            ManifestError::UrlInvalid { .. } => "scm_config_invalid",
        }
    }

    #[must_use]
    pub fn params(&self) -> Map<String, Value> {
        let mut params = Map::new();
        match self {
            ManifestError::CodeInvalid { status } => {
                params.insert("status".into(), (*status).into());
            }
            ManifestError::Unavailable { status, reason } => {
                if let Some(status) = status {
                    params.insert("status".into(), (*status).into());
                }
                if let Some(reason) = reason {
                    params.insert("reason".into(), (*reason).into());
                }
            }
            ManifestError::ResponseInvalid { field, .. } | ManifestError::UrlInvalid { field } => {
                params.insert("field".into(), (*field).into());
            }
        }
        params
    }

    #[must_use]
    pub fn created(&self) -> Option<(u64, &str)> {
        match self {
            ManifestError::ResponseInvalid {
                id: Some(id),
                slug: Some(slug),
                ..
            } => Some((*id, slug)),
            _ => None,
        }
    }
}

fn unavailable_reason(reason: &'static str) -> ManifestError {
    ManifestError::Unavailable {
        status: None,
        reason: Some(reason),
    }
}

fn transport_reason(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect"
    } else {
        "transport"
    }
}

pub async fn convert(
    api_url: &str,
    user_agent: &str,
    code: &str,
    timeout: Duration,
) -> Result<CreatedApp, ManifestError> {
    let mut url = Url::parse(api_url.trim_end_matches('/'))
        .map_err(|_| ManifestError::UrlInvalid { field: "api_url" })?;
    url.path_segments_mut()
        .map_err(|_| ManifestError::UrlInvalid { field: "api_url" })?
        .pop_if_empty()
        .extend(["app-manifests", code, "conversions"]);
    let http = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(timeout)
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| unavailable_reason("client"))?;
    let mut response = http
        .post(url)
        .header(ACCEPT, JSON_MEDIA_TYPE)
        .header("X-GitHub-Api-Version", API_VERSION)
        .send()
        .await
        .map_err(|e| unavailable_reason(transport_reason(&e)))?;
    let status = response.status();
    if matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return Err(ManifestError::CodeInvalid {
            status: status.as_u16(),
        });
    }
    if status != StatusCode::CREATED {
        return Err(ManifestError::Unavailable {
            status: Some(status.as_u16()),
            reason: None,
        });
    }
    let mut raw = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| unavailable_reason(transport_reason(&e)))?
    {
        if raw.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(unavailable_reason("response_too_large"));
        }
        raw.extend_from_slice(&chunk);
    }
    created_app(&raw)
}

fn take_secret(body: &mut Map<String, Value>, field: &str) -> Option<Zeroizing<String>> {
    match body.remove(field) {
        Some(Value::String(s)) => Some(Zeroizing::new(s)),
        _ => None,
    }
}

fn created_app(raw: &[u8]) -> Result<CreatedApp, ManifestError> {
    let invalid = |field, id, slug| ManifestError::ResponseInvalid { field, id, slug };
    let Ok(Value::Object(mut body)) = serde_json::from_slice::<Value>(raw) else {
        return Err(invalid("body", None, None));
    };
    let pem = take_secret(&mut body, "pem");
    let webhook_secret = take_secret(&mut body, "webhook_secret");
    drop(take_secret(&mut body, "client_secret"));

    let id = body["id"].as_u64().filter(|id| *id > 0);
    let slug = body["slug"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(String::from);
    let fail = |field| invalid(field, id, slug.clone());
    let text = |field: &'static str| {
        body.get(field)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .ok_or_else(|| fail(field))
    };
    let Some(id) = id else {
        return Err(fail("id"));
    };
    let slug = text("slug")?;
    let name = text("name")?;
    let owner = body
        .get("owner")
        .and_then(|o| o.get("login"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| fail("owner.login"))?;
    let html_url = body
        .get("html_url")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| fail("html_url"))?;
    let client_id = body
        .get("client_id")
        .and_then(Value::as_str)
        .filter(|c| client_id_valid(c))
        .map(String::from)
        .ok_or_else(|| fail("client_id"))?;
    let pem = pem
        .filter(|p| EncodingKey::from_rsa_pem(p.as_bytes()).is_ok())
        .ok_or_else(|| fail("pem"))?;
    let permissions = body
        .get("permissions")
        .and_then(Value::as_object)
        .and_then(|map| {
            map.iter()
                .map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect::<Option<BTreeMap<_, _>>>()
        })
        .ok_or_else(|| fail("permissions"))?;
    let events = body
        .get("events")
        .and_then(Value::as_array)
        .and_then(|list| {
            list.iter()
                .map(|e| e.as_str().map(String::from))
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| fail("events"))?;
    let webhook_secret = webhook_secret
        .filter(|s| WEBHOOK_SECRET_BYTES.contains(&s.len()) && !s.as_bytes().contains(&0));
    Ok(CreatedApp {
        id,
        slug,
        name,
        owner,
        html_url,
        client_id,
        pem,
        webhook_secret,
        permissions,
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"{
  "name": "saka-3f9a1c",
  "url": "https://github.com/anggasct/saka",
  "description": "Saka delivery loop (integration github)",
  "public": false,
  "redirect_url": "http://127.0.0.1:41873/callback",
  "hook_attributes": { "url": "https://saka.example.com/hooks/github", "active": true },
  "default_events": ["check_run", "check_suite", "issue_comment", "issues", "pull_request", "status"],
  "default_permissions": { "actions": "write", "checks": "read", "contents": "write", "issues": "write", "metadata": "read", "pull_requests": "write", "statuses": "write" },
  "request_oauth_on_install": false,
  "setup_on_update": false
}"#;

    fn request(webhook_url: Option<&str>) -> ManifestRequest<'_> {
        ManifestRequest {
            name: "saka-3f9a1c",
            integration_id: "github",
            redirect_url: "http://127.0.0.1:41873/callback",
            webhook_url,
        }
    }

    #[test]
    fn the_manifest_with_a_webhook_matches_the_documented_example() {
        let built = manifest(&request(Some("https://saka.example.com/hooks/github")));
        let expected: Value = serde_json::from_str(EXAMPLE).unwrap();
        assert_eq!(built, expected);
    }

    #[test]
    fn the_manifest_serializes_with_sorted_keys() {
        let built = manifest(&request(Some("https://saka.example.com/hooks/github")));
        assert_eq!(
            serde_json::to_string(&built).unwrap(),
            concat!(
                r#"{"default_events":["check_run","check_suite","issue_comment","issues","pull_request","status"],"#,
                r#""default_permissions":{"actions":"write","checks":"read","contents":"write","issues":"write","metadata":"read","pull_requests":"write","statuses":"write"},"#,
                r#""description":"Saka delivery loop (integration github)","#,
                r#""hook_attributes":{"active":true,"url":"https://saka.example.com/hooks/github"},"#,
                r#""name":"saka-3f9a1c","public":false,"redirect_url":"http://127.0.0.1:41873/callback","#,
                r#""request_oauth_on_install":false,"setup_on_update":false,"url":"https://github.com/anggasct/saka"}"#,
            )
        );
    }

    #[test]
    fn the_manifest_without_a_webhook_has_no_hook_or_events() {
        let built = manifest(&request(None));
        let mut expected: Value = serde_json::from_str(EXAMPLE).unwrap();
        let object = expected.as_object_mut().unwrap();
        object.remove("hook_attributes");
        object.remove("default_events");
        assert_eq!(built, expected);
        assert!(built.get("hook_attributes").is_none());
        assert!(built.get("default_events").is_none());
        assert_eq!(built["public"], false);
    }

    #[test]
    fn the_form_action_targets_the_personal_account() {
        assert_eq!(
            form_action("https://github.com", None, "abc_-XYZ").unwrap(),
            "https://github.com/settings/apps/new?state=abc_-XYZ"
        );
    }

    #[test]
    fn the_form_action_targets_an_organization() {
        assert_eq!(
            form_action("https://github.com/", Some("acme"), "s").unwrap(),
            "https://github.com/organizations/acme/settings/apps/new?state=s"
        );
    }

    #[test]
    fn the_form_action_targets_an_enterprise_server_and_encodes_the_state() {
        assert_eq!(
            form_action("https://ghe.example.com", Some("acme"), "a+b/c=&d").unwrap(),
            "https://ghe.example.com/organizations/acme/settings/apps/new?state=a%2Bb%2Fc%3D%26d"
        );
    }

    #[test]
    fn settings_and_apps_urls_follow_the_owner() {
        assert_eq!(
            settings_url("https://github.com", None, "saka-1"),
            "https://github.com/settings/apps/saka-1"
        );
        assert_eq!(
            settings_url("https://github.com/", Some("acme"), "saka-1"),
            "https://github.com/organizations/acme/settings/apps/saka-1"
        );
        assert_eq!(
            apps_url("https://ghe.example.com", Some("acme")),
            "https://ghe.example.com/organizations/acme/settings/apps"
        );
    }
}
