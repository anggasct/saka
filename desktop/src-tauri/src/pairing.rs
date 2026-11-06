use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::address::ServerAddress;
use crate::error::ShellError;
use crate::secrets::Token;

const PAIR_PATH: &str = "/api/v1/auth/pair";
const DEVICE_NAME: &str = "Saka Desktop";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Serialize)]
struct PairRequest<'a> {
    code: &'a str,
    name: &'a str,
}

#[derive(Deserialize)]
struct Paired {
    token: String,
}

#[derive(Deserialize)]
struct Problem {
    code: Option<String>,
}

pub fn client() -> Result<reqwest::Client, ShellError> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| {
            tracing::warn!(error = %e, "http client not built");
            ShellError::ServerUnreachable
        })
}

pub async fn pair(
    http: &reqwest::Client,
    address: &ServerAddress,
    code: &str,
) -> Result<Token, ShellError> {
    let response = http
        .post(address.endpoint(PAIR_PATH))
        .json(&PairRequest {
            code: code.trim(),
            name: DEVICE_NAME,
        })
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e.without_url(), "pairing request failed");
            ShellError::ServerUnreachable
        })?;
    let status = response.status();
    if status.is_success() {
        let paired: Paired = response
            .json()
            .await
            .map_err(|_| ShellError::ServerRefused)?;
        return Ok(Token::new(paired.token));
    }
    let problem: Option<Problem> = response.json().await.ok();
    let code = problem.and_then(|p| p.code);
    Err(match (status.as_u16(), code.as_deref()) {
        (404, Some("auth_disabled")) => ShellError::PairingUnavailable,
        (429, _) => ShellError::RateLimited,
        (400, Some("pairing_code_invalid")) => ShellError::PairingCodeInvalid,
        _ => ShellError::ServerRefused,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    async fn server_answering(status: u16, body: serde_json::Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(PAIR_PATH))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&server)
            .await;
        server
    }

    async fn outcome(status: u16, body: serde_json::Value) -> Result<Token, ShellError> {
        let server = server_answering(status, body).await;
        let address = ServerAddress::parse(&server.uri()).unwrap();
        pair(&client().unwrap(), &address, "WDJB-MJHT").await
    }

    #[tokio::test]
    async fn the_code_and_a_device_name_are_posted_and_the_token_comes_back() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(PAIR_PATH))
            .and(body_json(
                json!({ "code": "WDJB-MJHT", "name": "Saka Desktop" }),
            ))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(json!({ "device_id": "d1", "token": "sdt_abc" })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let address = ServerAddress::parse(&server.uri()).unwrap();
        let token = pair(&client().unwrap(), &address, " WDJB-MJHT ")
            .await
            .unwrap();
        assert_eq!(token.expose(), "sdt_abc");
    }

    #[tokio::test]
    async fn refusals_map_to_their_own_codes() {
        let cases = [
            (
                400,
                json!({ "code": "pairing_code_invalid" }),
                ShellError::PairingCodeInvalid,
            ),
            (
                429,
                json!({ "code": "auth_rate_limited" }),
                ShellError::RateLimited,
            ),
            (
                404,
                json!({ "code": "auth_disabled" }),
                ShellError::PairingUnavailable,
            ),
            (
                500,
                json!({ "code": "internal" }),
                ShellError::ServerRefused,
            ),
            (
                403,
                json!({ "code": "origin_not_allowed" }),
                ShellError::ServerRefused,
            ),
        ];
        for (status, body, expected) in cases {
            assert_eq!(
                outcome(status, body).await.unwrap_err(),
                expected,
                "{status}"
            );
        }
    }

    #[tokio::test]
    async fn a_server_that_is_down_is_unreachable() {
        let address = ServerAddress::parse("http://127.0.0.1:9").unwrap();
        let error = pair(&client().unwrap(), &address, "x").await.unwrap_err();
        assert_eq!(error, ShellError::ServerUnreachable);
    }
}
