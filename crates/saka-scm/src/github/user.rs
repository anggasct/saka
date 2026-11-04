use reqwest::header::{ACCEPT, HeaderMap, RETRY_AFTER};
use reqwest::{Method, StatusCode, Url};
use saka_core::authorship::Identity;
use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_core::vocab::AuthorizationFailure;
use saka_ports::{
    DeviceCode, DevicePoll, IdentityLookup, PortError, PortResult, RepoAccess, TokenRefresh,
    UserTokens,
};
use serde_json::{Value, json};

use super::app::{As, GitHubApp, rate_limit_reset, unavailable};

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const TOKEN_EXPIRATION: &str = "github-authentication-token-expiration";

struct OAuthReply {
    status: StatusCode,
    body: Value,
}

impl OAuthReply {
    fn error(&self) -> Option<&str> {
        self.body["error"].as_str()
    }
}

fn rejected(op: &str, reply: &OAuthReply) -> PortError {
    PortError::permanent(
        "scm_rejected",
        json!({ "op": op, "status": reply.status.as_u16(), "error": reply.error() }),
    )
}

fn secs(value: &Value) -> Option<u64> {
    value.as_u64()
}

fn tokens(body: &Value) -> Option<UserTokens> {
    let access = body["access_token"].as_str().filter(|t| !t.is_empty())?;
    Some(UserTokens {
        access_token: SecretText::new(access.to_string()),
        expires_in_secs: secs(&body["expires_in"]),
        refresh_token: body["refresh_token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(|t| SecretText::new(t.to_string())),
        refresh_expires_in_secs: secs(&body["refresh_token_expires_in"]),
    })
}

fn device_failure(error: &str) -> Option<AuthorizationFailure> {
    use AuthorizationFailure as F;
    Some(match error {
        "device_flow_disabled" => F::DeviceFlowDisabled,
        "incorrect_client_credentials" => F::IncorrectClientCredentials,
        "incorrect_device_code" => F::IncorrectDeviceCode,
        "unsupported_grant_type" => F::UnsupportedGrantType,
        "bad_verification_code" => F::BadVerificationCode,
        _ => return None,
    })
}

// GitHub writes `2027-01-31 00:00:00 UTC` or `2027-01-31 00:00:00 +0000`.
pub fn token_expiration(raw: &str) -> Option<Timestamp> {
    let raw = raw.trim();
    let zoned = match raw.strip_suffix(" UTC") {
        Some(local) => format!("{local} +0000"),
        None => raw.to_string(),
    };
    let at = jiff::fmt::strtime::parse("%Y-%m-%d %H:%M:%S %z", &zoned)
        .ok()?
        .to_timestamp()
        .ok()?;
    Some(Timestamp(at.as_millisecond()))
}

// A header that cannot be read leaves the expiry unknown: the provider still refuses an expired
// token when it is used, and that refusal marks it.
fn expiration_of(headers: &HeaderMap) -> Option<Timestamp> {
    let raw = headers.get(TOKEN_EXPIRATION)?;
    let parsed = raw.to_str().ok().and_then(token_expiration);
    if parsed.is_none() {
        tracing::warn!(
            header = TOKEN_EXPIRATION,
            "github token expiration unreadable"
        );
    }
    parsed
}

pub fn noreply_email(web_url: &str, provider_user_id: u64, login: &str) -> String {
    let host = Url::parse(web_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "github.com".into());
    format!("{provider_user_id}+{login}@users.noreply.{host}")
}

impl GitHubApp {
    fn client_id(&self) -> PortResult<&str> {
        self.cfg.client_id.as_deref().ok_or_else(|| {
            PortError::permanent(
                "device_flow_unconfigured",
                json!({ "integration_id": self.cfg.integration_id }),
            )
        })
    }

    fn web(&self, path: &str) -> PortResult<Url> {
        Url::parse(&format!("{}{path}", self.cfg.web_url.trim_end_matches('/')))
            .map_err(|_| PortError::permanent("scm_config_invalid", json!({ "field": "web_url" })))
    }

    // The form carries the device code or refresh token, so neither the body sent nor the body
    // received is ever logged; only the path, the status and GitHub's error code are.
    async fn oauth(&self, path: &str, form: &[(&str, &str)]) -> PortResult<OAuthReply> {
        let url = self.web(path)?;
        let response = self
            .http
            .post(url)
            .header(ACCEPT, "application/json")
            .form(form)
            .send()
            .await
            .map_err(|e| {
                let detail = if e.is_timeout() {
                    "timeout"
                } else {
                    "transport"
                };
                tracing::warn!(path, detail, "github oauth request failed");
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
            path,
            status = status.as_u16(),
            error = body["error"].as_str(),
            "github oauth request"
        );
        if status == StatusCode::TOO_MANY_REQUESTS
            || (status == StatusCode::FORBIDDEN && headers.contains_key(RETRY_AFTER))
        {
            let mut e = unavailable(Some(status.as_u16()), "rate_limited");
            e.retry_at = rate_limit_reset(&headers, self.clock.now());
            return Err(e);
        }
        if status.is_server_error() {
            return Err(unavailable(Some(status.as_u16()), "server_error"));
        }
        Ok(OAuthReply { status, body })
    }

    pub async fn device_code(&self) -> PortResult<DeviceCode> {
        let client_id = self.client_id()?;
        let reply = self
            .oauth(
                "/login/device/code",
                &[("client_id", client_id), ("scope", "offline_access")],
            )
            .await?;
        if let Some(error @ ("device_flow_disabled" | "incorrect_client_credentials")) =
            reply.error()
        {
            return Err(PortError::permanent(
                "device_flow_unconfigured",
                json!({ "integration_id": self.cfg.integration_id, "reason": error }),
            ));
        }
        let b = &reply.body;
        match (
            b["device_code"].as_str().filter(|c| !c.is_empty()),
            b["user_code"].as_str(),
            b["verification_uri"].as_str(),
            secs(&b["expires_in"]),
        ) {
            (Some(device), Some(user), Some(uri), Some(expires)) if reply.status.is_success() => {
                Ok(DeviceCode {
                    device_code: SecretText::new(device.to_string()),
                    user_code: user.to_string(),
                    verification_uri: uri.to_string(),
                    expires_in_secs: expires,
                    interval_secs: secs(&b["interval"]).unwrap_or(5),
                })
            }
            _ => Err(rejected("device_code", &reply)),
        }
    }

    pub async fn poll_device(&self, device_code: &SecretText) -> PortResult<DevicePoll> {
        let client_id = self.client_id()?;
        let reply = self
            .oauth(
                "/login/oauth/access_token",
                &[
                    ("client_id", client_id),
                    ("device_code", device_code.expose()),
                    ("grant_type", DEVICE_GRANT),
                ],
            )
            .await?;
        if let Some(error) = reply.error() {
            return match error {
                "authorization_pending" => Ok(DevicePoll::Pending),
                "slow_down" => Ok(DevicePoll::SlowDown {
                    interval_secs: secs(&reply.body["interval"]),
                }),
                "expired_token" => Ok(DevicePoll::Expired),
                "access_denied" => Ok(DevicePoll::Denied),
                other => match device_failure(other) {
                    Some(code) => Ok(DevicePoll::Failed { code }),
                    None => Err(rejected("device_poll", &reply)),
                },
            };
        }
        match tokens(&reply.body) {
            Some(t) if reply.status.is_success() => Ok(DevicePoll::Token(t)),
            _ => Err(rejected("device_poll", &reply)),
        }
    }

    pub async fn refresh_user(&self, refresh_token: &SecretText) -> PortResult<TokenRefresh> {
        let client_id = self.client_id()?;
        let reply = self
            .oauth(
                "/login/oauth/access_token",
                &[
                    ("client_id", client_id),
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token.expose()),
                ],
            )
            .await?;
        if reply.status == StatusCode::UNAUTHORIZED || reply.error() == Some("bad_refresh_token") {
            return Ok(TokenRefresh::Rejected);
        }
        if reply.error().is_some() {
            return Err(rejected("refresh", &reply));
        }
        match tokens(&reply.body) {
            Some(t) if reply.status.is_success() => Ok(TokenRefresh::Refreshed(t)),
            _ => Err(rejected("refresh", &reply)),
        }
    }

    pub async fn identity(&self, token: &SecretText) -> PortResult<IdentityLookup> {
        let url = self.url(&["user"])?;
        let reply = self
            .send(Method::GET, url, As::User, token.expose().to_string(), None)
            .await?;
        if reply.status == StatusCode::UNAUTHORIZED {
            return Ok(IdentityLookup::Rejected { status: 401 });
        }
        if !reply.status.is_success() {
            return Err(self.unexpected("user", &reply));
        }
        let b = &reply.body;
        let (Some(id), Some(login)) = (b["id"].as_u64(), b["login"].as_str()) else {
            return Err(unavailable(Some(reply.status.as_u16()), "user_incomplete"));
        };
        let name = b["name"]
            .as_str()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(String::from);
        Ok(IdentityLookup::Found {
            identity: Identity {
                provider_user_id: id,
                login: login.to_string(),
                display_name: name,
                email: noreply_email(&self.cfg.web_url, id, login),
            },
            token_expires_at: expiration_of(&reply.headers),
        })
    }

    pub async fn user_repository(
        &self,
        full_name: &str,
        token: &SecretText,
    ) -> PortResult<RepoAccess> {
        let (owner, name) = super::app::split_repo(full_name)?;
        let url = self.url(&["repos", owner, name])?;
        let reply = self
            .send(Method::GET, url, As::User, token.expose().to_string(), None)
            .await?;
        Ok(if reply.status == StatusCode::OK {
            RepoAccess::Readable
        } else {
            RepoAccess::Inaccessible {
                status: reply.status.as_u16(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_noreply_address_uses_the_host_of_the_web_url() {
        assert_eq!(
            noreply_email("https://github.com", 1, "octo"),
            "1+octo@users.noreply.github.com"
        );
        assert_eq!(
            noreply_email("https://ghe.acme.test/", 7, "ann"),
            "7+ann@users.noreply.ghe.acme.test"
        );
    }

    #[test]
    fn the_token_expiration_reads_both_github_forms_and_nothing_else() {
        let utc = token_expiration("2027-01-31 00:00:00 UTC").unwrap();
        assert_eq!(utc, Timestamp(1_801_353_600_000));
        assert_eq!(token_expiration("2027-01-31 00:00:00 +0000"), Some(utc));
        assert_eq!(
            token_expiration("2027-01-30 16:00:00 -0800"),
            Some(utc),
            "an offset moves the instant"
        );
        for garbage in [
            "",
            "never",
            "2027-01-31",
            "2027-13-40 00:00:00 UTC",
            "1801267200",
        ] {
            assert_eq!(token_expiration(garbage), None, "{garbage}");
        }
    }
}
