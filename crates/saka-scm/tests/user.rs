#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::sync::Arc;
use std::sync::atomic::AtomicI64;

use saka_core::secret::SecretText;
use saka_core::vocab::AuthorizationFailure;
use saka_ports::{DevicePoll, IdentityLookup, PortError, TokenRefresh};
use saka_scm::{GitHubApp, GitHubConfig};
use serde_json::{Value, json};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

mod support;

use support::{NOW_MS, TestClock};

const CLIENT_ID: &str = "Iv23liAbCdEfGh123456";
const DEVICE_CODE: &str = "3584d83530557fdd1f46af8289938c8ef79f9dc5";

async fn github(client_id: Option<&str>) -> (MockServer, GitHubApp) {
    let server = MockServer::start().await;
    let app = GitHubApp::new(
        GitHubConfig {
            integration_id: "github".into(),
            api_url: server.uri(),
            web_url: server.uri(),
            app_id: 1,
            private_key_file: "/nonexistent/app.pem".into(),
            user_agent: "saka-test".into(),
            client_id: client_id.map(String::from),
        },
        Arc::new(TestClock(AtomicI64::new(NOW_MS))),
    )
    .unwrap();
    (server, app)
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

fn form(request: &Request) -> Vec<(String, String)> {
    std::str::from_utf8(&request.body)
        .unwrap()
        .split('&')
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap();
            (decode(k), decode(v))
        })
        .collect()
}

async fn answer(server: &MockServer, status: u16, body: Value) {
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .and(header("accept", "application/json"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn the_device_code_request_sends_the_client_id_and_offline_access_as_a_form() {
    let (server, app) = github(Some(CLIENT_ID)).await;
    Mock::given(method("POST"))
        .and(path("/login/device/code"))
        .and(header("accept", "application/json"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": DEVICE_CODE,
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 900,
            "interval": 5,
        })))
        .expect(1)
        .mount(&server)
        .await;
    let code = app.device_code().await.unwrap();
    assert_eq!(code.device_code.expose(), DEVICE_CODE);
    assert_eq!(code.user_code, "WDJB-MJHT");
    assert_eq!(code.verification_uri, "https://github.com/login/device");
    assert_eq!((code.expires_in_secs, code.interval_secs), (900, 5));
    assert!(!format!("{code:?}").contains(DEVICE_CODE));
    let sent = &server.received_requests().await.unwrap()[0];
    assert_eq!(
        form(sent),
        [
            ("client_id".to_string(), CLIENT_ID.to_string()),
            ("scope".to_string(), "offline_access".to_string()),
        ]
    );
    assert!(sent.headers.get("authorization").is_none());
}

#[tokio::test]
async fn a_device_flow_github_refuses_and_a_missing_client_id_are_unconfigured() {
    let (server, app) = github(Some(CLIENT_ID)).await;
    Mock::given(method("POST"))
        .and(path("/login/device/code"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "error": "device_flow_disabled" })),
        )
        .mount(&server)
        .await;
    let e = app.device_code().await.unwrap_err();
    assert_eq!(
        (e.code.as_str(), e.retryable),
        ("device_flow_unconfigured", false)
    );
    assert_eq!(e.params["reason"], "device_flow_disabled");
    let (_server, bare) = github(None).await;
    assert_eq!(
        bare.device_code().await.unwrap_err().code,
        "device_flow_unconfigured"
    );
}

#[tokio::test]
async fn every_poll_answer_maps_to_its_device_poll() {
    let (server, app) = github(Some(CLIENT_ID)).await;
    let code = SecretText::new(DEVICE_CODE.into());
    let cases: Vec<(Value, DevicePoll)> = vec![
        (
            json!({ "error": "authorization_pending" }),
            DevicePoll::Pending,
        ),
        (
            json!({ "error": "slow_down", "interval": 10 }),
            DevicePoll::SlowDown {
                interval_secs: Some(10),
            },
        ),
        (
            json!({ "error": "slow_down" }),
            DevicePoll::SlowDown {
                interval_secs: None,
            },
        ),
        (json!({ "error": "expired_token" }), DevicePoll::Expired),
        (json!({ "error": "access_denied" }), DevicePoll::Denied),
        (
            json!({ "error": "device_flow_disabled" }),
            DevicePoll::Failed {
                code: AuthorizationFailure::DeviceFlowDisabled,
            },
        ),
        (
            json!({ "error": "incorrect_device_code" }),
            DevicePoll::Failed {
                code: AuthorizationFailure::IncorrectDeviceCode,
            },
        ),
        (
            json!({ "error": "incorrect_client_credentials" }),
            DevicePoll::Failed {
                code: AuthorizationFailure::IncorrectClientCredentials,
            },
        ),
        (
            json!({ "error": "unsupported_grant_type" }),
            DevicePoll::Failed {
                code: AuthorizationFailure::UnsupportedGrantType,
            },
        ),
        (
            json!({ "error": "bad_verification_code" }),
            DevicePoll::Failed {
                code: AuthorizationFailure::BadVerificationCode,
            },
        ),
    ];
    for (body, expected) in cases {
        answer(&server, 200, body.clone()).await;
        assert_eq!(app.poll_device(&code).await.unwrap(), expected, "{body}");
    }

    answer(
        &server,
        200,
        json!({
            "access_token": "ghu_16C7e42F292c6912E7710c838347Ae178B4a",
            "expires_in": 28800,
            "refresh_token": "ghr_1B4a2e77838347a7E420ce178F2E7c6912E169246c34E1ccbF66C46812d16D5B1A9Dc86A1498",
            "refresh_token_expires_in": 15897600,
            "scope": "",
            "token_type": "bearer",
        }),
    )
    .await;
    let DevicePoll::Token(tokens) = app.poll_device(&code).await.unwrap() else {
        panic!("a token answer is a token");
    };
    assert!(tokens.access_token.expose().starts_with("ghu_"));
    assert!(
        tokens
            .refresh_token
            .as_ref()
            .unwrap()
            .expose()
            .starts_with("ghr_")
    );
    assert_eq!(
        (tokens.expires_in_secs, tokens.refresh_expires_in_secs),
        (Some(28_800), Some(15_897_600))
    );
    let shown = format!("{tokens:?}");
    assert!(
        !shown.contains("ghu_") && !shown.contains("ghr_"),
        "{shown}"
    );
    let sent = server.received_requests().await.unwrap();
    assert_eq!(
        form(&sent[0]),
        [
            ("client_id".to_string(), CLIENT_ID.to_string()),
            ("device_code".to_string(), DEVICE_CODE.to_string()),
            (
                "grant_type".to_string(),
                "urn:ietf:params:oauth:grant-type:device_code".to_string()
            ),
        ]
    );

    answer(
        &server,
        200,
        json!({ "access_token": "ghu_x", "token_type": "bearer" }),
    )
    .await;
    let DevicePoll::Token(plain) = app.poll_device(&code).await.unwrap() else {
        panic!("a non-expiring token is still a token");
    };
    assert_eq!(plain.refresh_token, None);

    answer(&server, 502, json!({})).await;
    let e: PortError = app.poll_device(&code).await.unwrap_err();
    assert!(e.retryable, "{e:?}");
    answer(&server, 200, json!({ "error": "something_new" })).await;
    let e = app.poll_device(&code).await.unwrap_err();
    assert_eq!((e.code.as_str(), e.retryable), ("scm_rejected", false));
    assert!(!e.params.to_string().contains(DEVICE_CODE));
}

#[tokio::test]
async fn a_refresh_sends_the_refresh_grant_without_a_client_secret() {
    let (server, app) = github(Some(CLIENT_ID)).await;
    let refresh = SecretText::new("ghr_old_refresh_token_0123456789".into());
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "ghu_new_access",
            "expires_in": 28800,
            "refresh_token": "ghr_new_refresh",
            "refresh_token_expires_in": 15897600,
        })))
        .mount(&server)
        .await;
    let TokenRefresh::Refreshed(tokens) = app.refresh_user(&refresh).await.unwrap() else {
        panic!("a token answer refreshes");
    };
    assert_eq!(tokens.access_token.expose(), "ghu_new_access");
    assert_eq!(
        tokens.refresh_token.as_ref().map(SecretText::expose),
        Some("ghr_new_refresh")
    );
    let sent = &server.received_requests().await.unwrap()[0];
    let fields = form(sent);
    assert_eq!(
        fields,
        [
            ("client_id".to_string(), CLIENT_ID.to_string()),
            ("grant_type".to_string(), "refresh_token".to_string()),
            (
                "refresh_token".to_string(),
                "ghr_old_refresh_token_0123456789".to_string()
            ),
        ]
    );
    assert!(fields.iter().all(|(k, _)| k != "client_secret"));

    answer(&server, 200, json!({ "error": "bad_refresh_token" })).await;
    assert_eq!(
        app.refresh_user(&refresh).await.unwrap(),
        TokenRefresh::Rejected
    );
    answer(&server, 401, json!({ "message": "Bad credentials" })).await;
    assert_eq!(
        app.refresh_user(&refresh).await.unwrap(),
        TokenRefresh::Rejected
    );
    answer(&server, 503, json!({})).await;
    assert!(app.refresh_user(&refresh).await.unwrap_err().retryable);
}

#[tokio::test]
async fn the_user_endpoint_gives_the_identity_with_the_noreply_address() {
    let (server, app) = github(Some(CLIENT_ID)).await;
    let token = SecretText::new("ghu_identity_token".into());
    Mock::given(method("GET"))
        .and(path("/user"))
        .and(header("authorization", "Bearer ghu_identity_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "login": "octo", "name": "Octo Cat",
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let host = reqwest::Url::parse(&server.uri())
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();
    let IdentityLookup::Found(found) = app.identity(&token).await.unwrap() else {
        panic!("a 200 is an identity");
    };
    assert_eq!(found.provider_user_id, 1);
    assert_eq!(found.login, "octo");
    assert_eq!(found.display_name.as_deref(), Some("Octo Cat"));
    assert_eq!(found.email, format!("1+octo@users.noreply.{host}"));

    for name in [json!(""), json!(null)] {
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 583231, "login": "monalisa", "name": name,
            })))
            .mount(&server)
            .await;
        let IdentityLookup::Found(found) = app.identity(&token).await.unwrap() else {
            panic!("a 200 is an identity");
        };
        assert_eq!(found.display_name, None, "{name}");
        assert_eq!(found.email, format!("583231+monalisa@users.noreply.{host}"));
    }

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({ "message": "Bad credentials" })),
        )
        .mount(&server)
        .await;
    assert_eq!(
        app.identity(&token).await.unwrap(),
        IdentityLookup::Rejected { status: 401 }
    );
}
