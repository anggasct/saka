use std::collections::BTreeMap;

use reqwest::{Method, StatusCode, Url};
use saka_core::diagnostics::{AppStatus, HookDelivery, Installation, WebhookStatus};
use saka_ports::{PortError, PortResult, RepoRef};
use serde_json::{Value, json};

use super::app::{As, GitHubApp, Reply, parse_time, split_repo};

const DELIVERY_WINDOW_MS: i64 = 24 * 3_600_000;

fn auth_failed(app: &GitHubApp, status: Option<u16>) -> PortError {
    PortError::permanent(
        "app_auth_failed",
        json!({
            "status": status,
            "app_id": app.cfg.app_id,
            "key_file": app.cfg.private_key_file.to_string_lossy(),
        }),
    )
}

fn unavailable(status: Option<u16>) -> PortError {
    PortError::retryable("scm_unavailable", json!({ "status": status }))
}

fn classify(app: &GitHubApp, e: PortError) -> PortError {
    match e.code.as_str() {
        "scm_app_auth_refused" => auth_failed(app, Some(401)),
        code if code.starts_with("config_key_file") => auth_failed(app, None),
        _ => unavailable(
            e.params
                .get("status")
                .and_then(Value::as_u64)
                .and_then(|s| u16::try_from(s).ok()),
        ),
    }
}

async fn as_app(app: &GitHubApp, url: Url) -> PortResult<Reply> {
    let jwt = app.jwt().map_err(|e| classify(app, e))?;
    app.send(Method::GET, url, As::App, jwt, None)
        .await
        .map_err(|e| classify(app, e))
}

fn rejected(reply: &Reply) -> PortError {
    unavailable(Some(reply.status.as_u16()))
}

pub async fn app_status(app: &GitHubApp) -> PortResult<AppStatus> {
    let url = app.url(&["app"])?;
    let reply = as_app(app, url).await?;
    if !reply.status.is_success() {
        return Err(rejected(&reply));
    }
    let b = &reply.body;
    Ok(AppStatus {
        app_id: b["id"].as_u64().unwrap_or(app.cfg.app_id),
        slug: b["slug"].as_str().unwrap_or_default().to_string(),
        events: b["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.as_str().map(String::from))
            .collect(),
    })
}

pub async fn installation(app: &GitHubApp, repo: &RepoRef) -> PortResult<Installation> {
    let (owner, name) = split_repo(&repo.full_name)?;
    let url = app.url(&["repos", owner, name, "installation"])?;
    let reply = as_app(app, url).await?;
    if reply.status == StatusCode::NOT_FOUND {
        return Ok(Installation {
            installed: false,
            installation_id: None,
            permissions: BTreeMap::new(),
        });
    }
    if !reply.status.is_success() {
        return Err(rejected(&reply));
    }
    let permissions = reply.body["permissions"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
        .collect();
    Ok(Installation {
        installed: true,
        installation_id: reply.body["id"].as_u64(),
        permissions,
    })
}

pub async fn webhook_status(app: &GitHubApp) -> PortResult<WebhookStatus> {
    let config = as_app(app, app.url(&["app", "hook", "config"])?).await?;
    let (url_set, content_type, secret_set) = match config.status {
        StatusCode::NOT_FOUND => (false, None, false),
        s if s.is_success() => {
            let b = &config.body;
            (
                b["url"].as_str().is_some_and(|u| !u.is_empty()),
                b["content_type"].as_str().map(String::from),
                b["secret"].as_str().is_some_and(|s| !s.is_empty()),
            )
        }
        _ => return Err(rejected(&config)),
    };
    let mut url = app.url(&["app", "hook", "deliveries"])?;
    url.query_pairs_mut().append_pair("per_page", "100");
    let listed = as_app(app, url).await?;
    let deliveries = match listed.status {
        StatusCode::NOT_FOUND => Vec::new(),
        s if s.is_success() => {
            let since = app.clock.now().plus_ms(-DELIVERY_WINDOW_MS);
            listed
                .body
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|d| {
                    let delivered_at = parse_time(&d["delivered_at"])?;
                    (delivered_at >= since).then(|| HookDelivery {
                        delivered_at,
                        status_code: d["status_code"]
                            .as_u64()
                            .and_then(|c| u16::try_from(c).ok())
                            .unwrap_or(0),
                    })
                })
                .collect()
        }
        _ => return Err(rejected(&listed)),
    };
    Ok(WebhookStatus {
        url_set,
        content_type,
        secret_set,
        deliveries,
    })
}
