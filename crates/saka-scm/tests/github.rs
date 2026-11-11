#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use saka_core::time::Timestamp;
use saka_ports::{GitAccess, PortError, Scm, UpdateOutcome};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, ResponseTemplate};

mod support;

use support::*;

#[tokio::test]
async fn the_installation_token_is_cached_until_five_minutes_before_it_expires() {
    let w = World::new().await;
    w.mount_tokens().await;

    assert_eq!(w.app.token("acme/app").await.unwrap(), "ghs_1");
    assert_eq!(w.app.token("acme/app").await.unwrap(), "ghs_1");
    assert_eq!(w.token_requests().await, 1);

    let expires = NOW_MS + 3_600_000;
    w.advance_to(expires - 5 * 60_000 - 1);
    assert_eq!(w.app.token("acme/app").await.unwrap(), "ghs_1");
    assert_eq!(w.token_requests().await, 1);

    w.advance_to(expires - 5 * 60_000);
    assert_eq!(w.app.token("acme/app").await.unwrap(), "ghs_2");
    assert_eq!(w.token_requests().await, 2);
}

#[tokio::test]
async fn git_gets_the_token_as_a_basic_auth_header_for_the_web_host() {
    let w = World::new().await;
    w.mount_tokens().await;
    let remote = w.app.remote(&w.repo).await.unwrap();
    assert_eq!(remote.url, "https://github.example/acme/app.git");
    assert_eq!(
        remote.config,
        [(
            "http.https://github.example/.extraheader".to_string(),
            "AUTHORIZATION: basic eC1hY2Nlc3MtdG9rZW46Z2hzXzE=".to_string()
        )]
    );
    assert!(!format!("{remote:?}").contains("eC1hY2Nlc3M"));
    w.app.refused(&w.repo).await;
    w.app.remote(&w.repo).await.unwrap();
    assert_eq!(w.token_requests().await, 2);
}

#[tokio::test]
async fn a_refused_token_is_renewed_once_and_the_request_repeated() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app"))
        .and(header("authorization", "Bearer ghs_1"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({ "message": "Bad credentials" })),
        )
        .expect(1)
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app"))
        .and(header("authorization", "Bearer ghs_2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "full_name": "acme/app", "html_url": "https://github.example/acme/app", "default_branch": "main",
        })))
        .expect(1)
        .mount(&w.server)
        .await;
    let info = w.gh.repository("github", "acme/app").await.unwrap();
    assert_eq!(info.default_branch, "main");
    assert_eq!(w.token_requests().await, 2);
}

#[tokio::test]
async fn rate_limits_server_errors_and_missing_installations_map_to_codes() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/pulls/1"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", "1767229200")
                .set_body_json(json!({ "message": "API rate limit exceeded" })),
        )
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/pulls/2"))
        .respond_with(ResponseTemplate::new(502))
        .mount(&w.server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/repos/acme/app/git/refs/heads/saka/two-step/s1"))
        .respond_with(
            ResponseTemplate::new(422)
                .set_body_json(json!({ "message": "Reference does not exist" })),
        )
        .expect(1)
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/gone/installation"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&w.server)
        .await;

    let limited: PortError = w.gh.change_request(&w.repo, 1).await.unwrap_err();
    assert_eq!(limited.code, "scm_unavailable");
    assert!(limited.retryable);
    assert_eq!(limited.retry_at, Some(Timestamp(1_767_229_200_000)));

    let down = w.gh.change_request(&w.repo, 2).await.unwrap_err();
    assert_eq!(
        (down.code.as_str(), down.retryable, &down.params["status"]),
        ("scm_unavailable", true, &json!(502))
    );

    w.gh.delete_branch(&w.repo, "saka/two-step/s1")
        .await
        .unwrap();

    let gone = w.gh.repository("github", "acme/gone").await.unwrap_err();
    assert_eq!(
        (gone.code.as_str(), gone.retryable),
        ("scm_repo_inaccessible", false)
    );
}

#[tokio::test]
async fn a_private_key_readable_by_others_is_refused() {
    let w = World::new().await;
    let key = w.app.config().private_key_file.clone();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(w.app.jwt().unwrap_err().code, "config_key_file_mode");
    std::fs::set_permissions(Path::new(&key), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(w.app.jwt().is_ok());
}

#[tokio::test]
async fn intake_reads_follow_the_same_error_rules_and_never_leak_the_token() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/pulls"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", "1767229200")
                .set_body_json(json!({ "message": "API rate limit exceeded" })),
        )
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/pulls/9/files"))
        .respond_with(ResponseTemplate::new(502))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/pulls/5"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/collaborators/ghost/permission"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/collaborators/busy/permission"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&w.server)
        .await;

    let limited = w.gh.list_open_changes(&w.repo, 200).await.unwrap_err();
    assert_eq!(limited.code, "scm_unavailable");
    assert!(limited.retryable);
    assert_eq!(limited.retry_at, Some(Timestamp(1_767_229_200_000)));

    let down = w.gh.change_files(&w.repo, 9, 3000).await.unwrap_err();
    assert_eq!(
        (down.code.as_str(), down.retryable),
        ("scm_unavailable", true)
    );

    let missing = w.gh.change_summary(&w.repo, 5).await.unwrap_err();
    assert_eq!(
        (missing.code.as_str(), missing.retryable),
        ("scm_repo_inaccessible", false)
    );

    assert!(!w.gh.can_write(&w.repo, "ghost").await.unwrap());
    let busy = w.gh.can_write(&w.repo, "busy").await.unwrap_err();
    assert!(busy.retryable, "a failed permission read is not a denial");
    for err in [&limited, &down, &missing, &busy] {
        assert!(!format!("{err:?}").contains("ghs_"), "{err:?}");
    }
}

#[tokio::test]
async fn a_changed_file_that_was_renamed_lists_both_paths_once() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/pulls/4/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "filename": "new/name.rs", "previous_filename": "old/name.rs", "status": "renamed" },
            { "filename": "new/name.rs" },
            { "filename": "docs/readme.md" },
        ])))
        .mount(&w.server)
        .await;
    let files = w.gh.change_files(&w.repo, 4, 3000).await.unwrap();
    assert_eq!(files, ["new/name.rs", "old/name.rs", "docs/readme.md"]);
}

#[tokio::test]
async fn update_branch_sends_the_expected_head_and_maps_each_answer() {
    let w = World::new().await;
    w.mount_tokens().await;
    let answers = [
        (
            1,
            202,
            json!({ "message": "Updating pull request branch." }),
        ),
        (
            2,
            422,
            json!({ "message": "merge conflict between base and head" }),
        ),
        (
            3,
            422,
            json!({ "message": "expected head sha didn't match current head ref." }),
        ),
        (4, 409, json!({ "message": "Conflict" })),
    ];
    for (number, status, body) in answers {
        Mock::given(method("PUT"))
            .and(path(format!(
                "/repos/acme/app/pulls/{number}/update-branch"
            )))
            .and(body_json(json!({ "expected_head_sha": HEAD })))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .expect(1)
            .mount(&w.server)
            .await;
    }
    Mock::given(method("PUT"))
        .and(path("/repos/acme/app/pulls/5/update-branch"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&w.server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/repos/acme/app/pulls/6/update-branch"))
        .respond_with(ResponseTemplate::new(502))
        .mount(&w.server)
        .await;

    let outcomes = [
        UpdateOutcome::Accepted,
        UpdateOutcome::Conflict,
        UpdateOutcome::HeadChanged,
        UpdateOutcome::HeadChanged,
    ];
    for (number, outcome) in (1..).zip(outcomes) {
        assert_eq!(
            w.gh.update_branch(&w.repo, number, HEAD).await.unwrap(),
            outcome,
            "pull request {number}"
        );
    }
    let gone = w.gh.update_branch(&w.repo, 5, HEAD).await.unwrap_err();
    assert_eq!(
        (gone.code.as_str(), gone.retryable),
        ("scm_repo_inaccessible", false)
    );
    let down = w.gh.update_branch(&w.repo, 6, HEAD).await.unwrap_err();
    assert_eq!(
        (down.code.as_str(), down.retryable),
        ("scm_unavailable", true)
    );
    let bad = w.gh.update_branch(&w.repo, 1, "abc").await.unwrap_err();
    assert_eq!(
        (bad.code.as_str(), &bad.params["field"]),
        ("git_argument_invalid", &json!("expected_head_sha"))
    );
}

#[tokio::test]
async fn a_comment_is_posted_to_the_issue_thread_and_a_refusal_is_permanent() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/app/issues/3/comments"))
        .and(body_json(json!({ "body": "Please update the branch." })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 9 })))
        .expect(1)
        .mount(&w.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/app/issues/4/comments"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "message": "no" })))
        .mount(&w.server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/app/issues/5/comments"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&w.server)
        .await;

    w.gh.post_comment(&w.repo, 3, "Please update the branch.")
        .await
        .unwrap();
    let refused = w.gh.post_comment(&w.repo, 4, "x").await.unwrap_err();
    assert_eq!(
        (
            refused.code.as_str(),
            refused.retryable,
            &refused.params["status"]
        ),
        ("comment_refused", false, &json!(403))
    );
    let down = w.gh.post_comment(&w.repo, 5, "x").await.unwrap_err();
    assert_eq!(
        (down.code.as_str(), down.retryable),
        ("scm_unavailable", true)
    );
    let long = "a".repeat(saka_ports::MAX_REVIEW_BODY_CHARS + 1);
    let err = w.gh.post_comment(&w.repo, 3, &long).await.unwrap_err();
    assert_eq!(err.code, "git_argument_invalid");
}
