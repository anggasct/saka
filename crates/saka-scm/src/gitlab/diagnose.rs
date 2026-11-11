use reqwest::{Method, StatusCode};
use saka_core::diagnostics::{
    CredentialStatus, RepoGrant, RepoHook, RepoHooks, ScmDiagnostics, TokenRequirements,
    TokenStatus,
};
use saka_core::time::Timestamp;
use saka_ports::{PortError, PortResult, RepoRef};
use serde_json::{Value, json};

use super::client::GitLabClient;
use crate::github::app::{parse_time, unavailable};

const DEVELOPER: u8 = 30;
const MAINTAINER: u8 = 40;

pub const TOKEN: ScmDiagnostics = ScmDiagnostics::Token(TokenRequirements {
    scopes: &["api"],
    required_access: DEVELOPER,
    recommended_access: MAINTAINER,
    hook_events: &[
        "merge_requests_events",
        "note_events",
        "pipeline_events",
        "job_events",
    ],
});

fn date(value: &Value) -> Option<Timestamp> {
    let day: jiff::civil::Date = value.as_str()?.parse().ok()?;
    let start = day.to_zoned(jiff::tz::TimeZone::UTC).ok()?;
    Some(Timestamp(start.timestamp().as_millisecond()))
}

fn refused() -> PortError {
    PortError::permanent("scm_auth_refused", json!({ "status": 401 }))
}

pub async fn credential_status(client: &GitLabClient) -> PortResult<CredentialStatus> {
    let url = client.url(&["personal_access_tokens", "self"])?;
    let reply = client.send(Method::GET, url, None).await?;
    if reply.status == StatusCode::UNAUTHORIZED {
        return Err(refused());
    }
    if !reply.status.is_success() {
        return Err(unavailable(Some(reply.status.as_u16()), "rejected"));
    }
    let b = &reply.body;
    Ok(CredentialStatus::Token(TokenStatus {
        name: b["name"].as_str().unwrap_or_default().to_string(),
        active: b["active"].as_bool().unwrap_or(false),
        revoked: b["revoked"].as_bool().unwrap_or(false),
        scopes: b["scopes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(String::from))
            .collect(),
        expires_at: date(&b["expires_at"]),
    }))
}

fn level(access: &Value) -> Option<u8> {
    access["access_level"]
        .as_u64()
        .and_then(|l| u8::try_from(l).ok())
}

pub async fn repo_grant(client: &GitLabClient, repo: &RepoRef) -> PortResult<RepoGrant> {
    let url = client.project_url(&repo.full_name, &[])?;
    let reply = client.call(&repo.full_name, Method::GET, url, None).await?;
    match reply.status {
        StatusCode::NOT_FOUND | StatusCode::FORBIDDEN => Ok(RepoGrant::NoAccess),
        s if s.is_success() => {
            let permissions = &reply.body["permissions"];
            Ok(RepoGrant::Member {
                access_level: level(&permissions["project_access"])
                    .max(level(&permissions["group_access"])),
            })
        }
        _ => Err(client.unexpected("repo_access", &reply)),
    }
}

fn hook_of(hook: &Value) -> Option<RepoHook> {
    let events = hook
        .as_object()?
        .iter()
        .filter(|(key, value)| key.ends_with("_events") && **value == true)
        .map(|(key, _)| key.clone())
        .collect();
    Some(RepoHook {
        id: hook["id"].as_u64()?,
        url: hook["url"].as_str()?.to_string(),
        events,
        ssl_verification: hook["enable_ssl_verification"].as_bool().unwrap_or(true),
        alert_status: hook["alert_status"]
            .as_str()
            .unwrap_or("executable")
            .to_string(),
        disabled_until: parse_time(&hook["disabled_until"]),
    })
}

pub async fn repo_webhooks(client: &GitLabClient, repo: &RepoRef) -> PortResult<RepoHooks> {
    let mut url = client.project_url(&repo.full_name, &["hooks"])?;
    url.query_pairs_mut().append_pair("per_page", "100");
    let reply = client.call(&repo.full_name, Method::GET, url, None).await?;
    match reply.status {
        StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => Ok(RepoHooks::Unreadable {
            status: reply.status.as_u16(),
        }),
        s if s.is_success() => Ok(RepoHooks::Listed(
            reply
                .body
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(hook_of)
                .collect(),
        )),
        _ => Err(client.unexpected("repo_webhooks", &reply)),
    }
}
