#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

mod support;

use saka_core::diagnostics::{CredentialStatus, RepoGrant, RepoHooks, TokenStatus};
use saka_core::time::Timestamp;
use saka_ports::Scm;
use serde_json::{Value, json};
use support::gitlab::{GitLabWorld, TOKEN};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

async fn serve(w: &GitLabWorld, at: &str, status: u16, body: Value) {
    Mock::given(method("GET"))
        .and(path(at))
        .and(header("private-token", TOKEN))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&w.server)
        .await;
}

async fn token_of(w: &GitLabWorld) -> TokenStatus {
    match w.gl.credential_status("gitlab").await.unwrap() {
        CredentialStatus::Token(token) => token,
        other => panic!("not a token: {other:?}"),
    }
}

async fn level_of(w: &GitLabWorld, project: Value) -> RepoGrant {
    serve(w, "/projects/acme%2Fapp", 200, project).await;
    w.gl.repo_grant(&w.repo).await.unwrap()
}

#[tokio::test]
async fn the_token_is_read_from_its_own_endpoint_with_its_scopes_and_expiry_date() {
    let w = GitLabWorld::new().await;
    serve(
        &w,
        "/personal_access_tokens/self",
        200,
        json!({
            "name": "saka", "active": true, "revoked": false,
            "scopes": ["api", "read_repository"], "expires_at": "2027-03-01",
        }),
    )
    .await;
    assert_eq!(
        token_of(&w).await,
        TokenStatus {
            name: "saka".into(),
            active: true,
            revoked: false,
            scopes: vec!["api".into(), "read_repository".into()],
            expires_at: Some(Timestamp(1_803_859_200_000)),
        }
    );
    let requests = w.server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/personal_access_tokens/self");
}

#[tokio::test]
async fn a_token_without_an_expiry_or_with_a_revoked_flag_is_reported_as_it_is() {
    let w = GitLabWorld::new().await;
    serve(
        &w,
        "/personal_access_tokens/self",
        200,
        json!({
            "name": "saka", "active": false, "revoked": true,
            "scopes": ["read_api"], "expires_at": null,
        }),
    )
    .await;
    let token = token_of(&w).await;
    assert!(token.revoked && !token.active);
    assert_eq!(token.scopes, ["read_api"]);
    assert_eq!(token.expires_at, None);
}

#[tokio::test]
async fn a_refused_token_is_an_auth_refusal_and_a_server_error_is_unavailable() {
    let w = GitLabWorld::new().await;
    serve(
        &w,
        "/personal_access_tokens/self",
        401,
        json!({ "message": "401 Unauthorized" }),
    )
    .await;
    let refused = w.gl.credential_status("gitlab").await.unwrap_err();
    assert_eq!(refused.code, "scm_auth_refused");
    assert_eq!(refused.params["status"], 401);
    assert!(!format!("{refused:?}").contains(TOKEN));

    let w = GitLabWorld::new().await;
    serve(&w, "/personal_access_tokens/self", 500, json!({})).await;
    let down = w.gl.credential_status("gitlab").await.unwrap_err();
    assert_eq!(down.code, "scm_unavailable");
    assert_eq!(down.params["status"], 500);
}

#[tokio::test]
async fn the_access_level_is_the_higher_of_the_project_and_group_levels() {
    let both = |project: Value, group: Value| json!({ "permissions": { "project_access": project, "group_access": group } });
    let cases = [
        (both(json!({ "access_level": 30 }), Value::Null), Some(30)),
        (
            both(json!({ "access_level": 30 }), json!({ "access_level": 40 })),
            Some(40),
        ),
        (both(Value::Null, json!({ "access_level": 20 })), Some(20)),
        (both(Value::Null, Value::Null), None),
        (json!({}), None),
    ];
    for (project, expected) in cases {
        let w = GitLabWorld::new().await;
        assert_eq!(
            level_of(&w, project.clone()).await,
            RepoGrant::Member {
                access_level: expected
            },
            "{project}"
        );
    }
}

#[tokio::test]
async fn a_project_the_token_cannot_see_is_no_access() {
    for status in [403, 404] {
        let w = GitLabWorld::new().await;
        serve(
            &w,
            "/projects/acme%2Fapp",
            status,
            json!({ "message": "404 Project Not Found" }),
        )
        .await;
        assert_eq!(
            w.gl.repo_grant(&w.repo).await.unwrap(),
            RepoGrant::NoAccess,
            "{status}"
        );
    }
}

#[tokio::test]
async fn project_hooks_list_their_enabled_events_ssl_flag_and_alert_state() {
    let w = GitLabWorld::new().await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp/hooks"))
        .and(query_param("per_page", "100"))
        .and(header("private-token", TOKEN))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "id": 7, "url": "https://saka.example/hooks/gitlab",
                "merge_requests_events": true, "note_events": true,
                "pipeline_events": true, "job_events": false, "push_events": false,
                "enable_ssl_verification": false, "alert_status": "temporarily_disabled",
                "disabled_until": "2026-01-01T01:00:00.000Z",
            },
            { "id": 8, "url": "https://other.example/x", "push_events": true },
        ])))
        .mount(&w.server)
        .await;
    let RepoHooks::Listed(hooks) = w.gl.repo_webhooks(&w.repo).await.unwrap() else {
        panic!("hooks were not listed");
    };
    assert_eq!(hooks.len(), 2);
    let mut events = hooks[0].events.clone();
    events.sort();
    assert_eq!(
        events,
        ["merge_requests_events", "note_events", "pipeline_events"]
    );
    assert_eq!(hooks[0].url, "https://saka.example/hooks/gitlab");
    assert!(!hooks[0].ssl_verification);
    assert_eq!(hooks[0].alert_status, "temporarily_disabled");
    assert_eq!(hooks[0].disabled_until, Some(Timestamp(1_767_229_200_000)));
    assert!(hooks[1].ssl_verification);
    assert_eq!(hooks[1].alert_status, "executable");
    assert_eq!(hooks[1].events, ["push_events"]);
}

#[tokio::test]
async fn hooks_a_token_may_not_read_are_unreadable_not_an_error() {
    let w = GitLabWorld::new().await;
    serve(
        &w,
        "/projects/acme%2Fapp/hooks",
        403,
        json!({ "message": "403 Forbidden" }),
    )
    .await;
    assert_eq!(
        w.gl.repo_webhooks(&w.repo).await.unwrap(),
        RepoHooks::Unreadable { status: 403 }
    );
}
