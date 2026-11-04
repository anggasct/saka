#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use saka_core::secret::SecretText;
use saka_core::vocab::MergeMethod;
use saka_ports::{
    ChangeCredential, ChangeOpening, GitAccess, MergeOutcome, MergeRequest, OpenChangeRequest, Scm,
    StatusReport, StatusState,
};
use serde_json::json;
use wiremock::matchers::{method, path, path_regex, query_param};
use wiremock::{Mock, ResponseTemplate};

mod support;

use support::{HEAD, MERGED, World, pr};

const USER_TOKEN: &str = "ghu_userlinkaccess0001";
const BRANCH: &str = "saka/two-step/s1";

fn request(w: &World, credential: ChangeCredential) -> OpenChangeRequest {
    OpenChangeRequest {
        repo: w.repo.clone(),
        head_branch: BRANCH.into(),
        base_branch: "main".into(),
        title: "Add the first step".into(),
        body: "body".into(),
        credential,
    }
}

fn user() -> ChangeCredential {
    ChangeCredential::User(SecretText::new(USER_TOKEN.into()))
}

async fn calls(w: &World) -> Vec<(String, String, String)> {
    w.server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| {
            !r.url.path().ends_with("/installation") && !r.url.path().ends_with("/access_tokens")
        })
        .map(|r| {
            let auth = r
                .headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            (r.method.to_string(), r.url.path().to_string(), auth)
        })
        .collect()
}

#[tokio::test]
async fn only_opening_the_pull_request_carries_the_user_token() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/app/pulls"))
        .respond_with(ResponseTemplate::new(201).set_body_json(pr(7, "open", false)))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr(7, "open", false)))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/acme/app/commits/{HEAD}/check-runs")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "check_runs": [] })))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/acme/app/commits/{HEAD}/status")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "statuses": [] })))
        .mount(&w.server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/acme/app/statuses/{HEAD}")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
        .mount(&w.server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/repos/acme/app/pulls/7/merge"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "merged": true, "sha": MERGED })),
        )
        .mount(&w.server)
        .await;
    Mock::given(method("DELETE"))
        .and(path_regex(
            "^/repos/acme/app/git/refs/heads/saka/two-step/s1$",
        ))
        .respond_with(ResponseTemplate::new(204))
        .mount(&w.server)
        .await;

    let opened =
        w.gh.open_change_request(&request(&w, user()))
            .await
            .unwrap();
    let ChangeOpening::Opened(opened) = opened else {
        panic!("not opened: {opened:?}");
    };
    assert_eq!(opened.number, 7);
    w.gh.change_request(&w.repo, 7).await.unwrap();
    w.gh.report_status(
        &w.repo,
        HEAD,
        &StatusReport {
            context: "saka/verify".into(),
            state: StatusState::Success,
            description: "passed".into(),
        },
    )
    .await
    .unwrap();
    let merged =
        w.gh.merge(&MergeRequest {
            repo: w.repo.clone(),
            number: 7,
            head_sha: HEAD.into(),
            method: MergeMethod::Squash,
            commit_title: "Add the first step (#7)".into(),
        })
        .await
        .unwrap();
    assert!(matches!(merged, MergeOutcome::Merged { .. }));
    w.gh.delete_branch(&w.repo, BRANCH).await.unwrap();
    let remote = w.app.remote(&w.repo).await.unwrap();

    let calls = calls(&w).await;
    let ops: Vec<(&str, &str)> = calls
        .iter()
        .map(|(m, p, _)| (m.as_str(), p.as_str()))
        .collect();
    assert_eq!(
        ops,
        [
            ("POST", "/repos/acme/app/pulls"),
            ("GET", "/repos/acme/app/pulls/7"),
            (
                "GET",
                &*format!("/repos/acme/app/commits/{HEAD}/check-runs")
            ),
            ("GET", &*format!("/repos/acme/app/commits/{HEAD}/status")),
            ("POST", &*format!("/repos/acme/app/statuses/{HEAD}")),
            ("PUT", "/repos/acme/app/pulls/7/merge"),
            ("DELETE", "/repos/acme/app/git/refs/heads/saka/two-step/s1"),
        ]
    );
    for (m, p, auth) in &calls {
        let expected = if m == "POST" && p == "/repos/acme/app/pulls" {
            format!("Bearer {USER_TOKEN}")
        } else {
            "Bearer ghs_1".to_string()
        };
        assert_eq!(auth, &expected, "{m} {p}");
    }
    // The push credential: base64("x-access-token:ghs_1"), the installation token.
    assert_eq!(
        remote.config[0].1,
        "AUTHORIZATION: basic eC1hY2Nlc3MtdG9rZW46Z2hzXzE="
    );
}

#[tokio::test]
async fn an_existing_pull_request_is_looked_up_with_the_installation_token() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/app/pulls"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "message": "Validation Failed",
            "errors": [{ "message": "A pull request already exists for acme:saka/two-step/s1." }],
        })))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/pulls"))
        .and(query_param("head", "acme:saka/two-step/s1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([pr(9, "open", false)])))
        .mount(&w.server)
        .await;

    let opened =
        w.gh.open_change_request(&request(&w, user()))
            .await
            .unwrap();
    assert!(
        matches!(opened, ChangeOpening::Opened(ref c) if c.number == 9),
        "{opened:?}"
    );
    let calls = calls(&w).await;
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].2, format!("Bearer {USER_TOKEN}"));
    assert_eq!(
        (calls[1].0.as_str(), calls[1].2.as_str()),
        ("GET", "Bearer ghs_1")
    );
}

#[tokio::test]
async fn a_refused_user_token_and_missing_access_are_outcomes_not_errors() {
    for (status, want) in [
        (401, ChangeOpening::CredentialRejected),
        (403, ChangeOpening::AccessDenied { status: 403 }),
        (404, ChangeOpening::AccessDenied { status: 404 }),
    ] {
        let w = World::new().await;
        w.mount_tokens().await;
        Mock::given(method("POST"))
            .and(path("/repos/acme/app/pulls"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({ "message": "nope" })))
            .expect(1)
            .mount(&w.server)
            .await;
        let opened =
            w.gh.open_change_request(&request(&w, user()))
                .await
                .unwrap();
        assert_eq!(opened, want, "{status}");
        assert_eq!(
            w.token_requests().await,
            0,
            "no installation token for {status}"
        );
    }
}

#[tokio::test]
async fn the_app_credential_keeps_the_installation_token_and_its_errors() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/app/pulls"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "Not Found" })))
        .mount(&w.server)
        .await;
    let e =
        w.gh.open_change_request(&request(&w, ChangeCredential::App))
            .await
            .unwrap_err();
    assert_eq!(e.code, "scm_repo_inaccessible");
    let calls = calls(&w).await;
    assert_eq!(calls[0].2, "Bearer ghs_1");
}
