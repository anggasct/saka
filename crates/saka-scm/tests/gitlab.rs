#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

mod support;

use std::os::unix::fs::PermissionsExt;

use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_core::vocab::{ChangeState, MergeMethod};
use saka_ports::{
    ChangeCredential, ChangeOpening, GitAccess, MergeOutcome, MergeRequest, OpenChangeRequest,
    PortError, ReviewPost, Scm, StatusReport, StatusState,
};
use serde_json::{Value, json};
use support::gitlab::{GitLabWorld, TOKEN, mr};
use support::{HEAD, MERGED};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

const BRANCH: &str = "saka/two-step/s1";

fn merge_request(w: &GitLabWorld, number: u64, method: MergeMethod) -> MergeRequest {
    MergeRequest {
        repo: w.repo.clone(),
        number,
        head_sha: HEAD.into(),
        method,
        commit_title: format!("Add the first step (#{number})"),
    }
}

async fn serve(w: &GitLabWorld, verb: &str, at: &str, status: u16, body: Value) {
    Mock::given(method(verb))
        .and(path(at))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&w.server)
        .await;
}

fn code(err: &PortError) -> (&str, bool) {
    (err.code.as_str(), err.retryable)
}

#[tokio::test]
async fn every_request_carries_the_token_header_and_a_rotated_file_applies_at_once() {
    let w = GitLabWorld::new().await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp"))
        .and(header("private-token", TOKEN))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "path_with_namespace": "acme/app", "web_url": "u", "default_branch": "main",
        })))
        .mount(&w.server)
        .await;
    w.gl.repository("gitlab", "acme/app").await.unwrap();
    let seen = w.server.received_requests().await.unwrap();
    assert!(seen[0].headers.get("authorization").is_none());

    std::fs::write(w.token_path(), "glpat-rotated\n").unwrap();
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp"))
        .and(header("private-token", "glpat-rotated"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "path_with_namespace": "acme/app", "web_url": "u", "default_branch": "dev",
        })))
        .with_priority(1)
        .mount(&w.server)
        .await;
    assert_eq!(
        w.gl.repository("gitlab", "acme/app")
            .await
            .unwrap()
            .default_branch,
        "dev"
    );
}

#[tokio::test]
async fn a_token_file_that_is_loose_missing_or_empty_is_refused_without_a_request() {
    let w = GitLabWorld::new().await;
    let path = w.token_path();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let err = w.gl.repository("gitlab", "acme/app").await.unwrap_err();
    assert_eq!(code(&err), ("config_key_file_mode", false));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&path, "  \n").unwrap();
    let err = w.gl.repository("gitlab", "acme/app").await.unwrap_err();
    assert_eq!(code(&err), ("config_key_file_unreadable", false));
    assert_eq!(err.params["reason"], "empty");
    std::fs::remove_file(&path).unwrap();
    let err = w.gl.repository("gitlab", "acme/app").await.unwrap_err();
    assert_eq!(err.params["reason"], "missing");
    assert!(w.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn nested_groups_are_one_encoded_segment_and_odd_names_are_refused() {
    let w = GitLabWorld::new().await;
    serve(
        &w,
        "GET",
        "/projects/group%2Fsub%2Fapp",
        200,
        json!({ "path_with_namespace": "group/sub/app", "web_url": "u", "default_branch": "main" }),
    )
    .await;
    let info = w.gl.repository("gitlab", "group/sub/app").await.unwrap();
    assert_eq!(info.full_name, "group/sub/app");
    for bad in ["acme", "acme/../x", "acme/a b", "acme/", "acme/a%2Fb"] {
        let err = w.gl.repository("gitlab", bad).await.unwrap_err();
        assert_eq!(err.code, "scm_repo_name_invalid", "{bad}");
    }
}

#[tokio::test]
async fn http_failures_are_classified_and_never_echo_the_token() {
    let w = GitLabWorld::new().await;
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp",
        401,
        json!({ "message": "401 Unauthorized" }),
    )
    .await;
    let err = w.gl.repository("gitlab", "acme/app").await.unwrap_err();
    assert_eq!(code(&err), ("scm_auth_refused", false));

    let w = GitLabWorld::new().await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .mount(&w.server)
        .await;
    let err = w.gl.repository("gitlab", "acme/app").await.unwrap_err();
    assert_eq!(code(&err), ("scm_unavailable", true));
    assert_eq!(err.retry_at, Some(Timestamp(support::NOW_MS + 30_000)));

    let w = GitLabWorld::new().await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp"))
        .respond_with(ResponseTemplate::new(429).insert_header("ratelimit-reset", "1767230000"))
        .mount(&w.server)
        .await;
    let err = w.gl.repository("gitlab", "acme/app").await.unwrap_err();
    assert_eq!(err.retry_at, Some(Timestamp(1_767_230_000_000)));

    for status in [500, 502, 503] {
        let w = GitLabWorld::new().await;
        serve(&w, "GET", "/projects/acme%2Fapp", status, json!({})).await;
        let err = w.gl.repository("gitlab", "acme/app").await.unwrap_err();
        assert_eq!(code(&err), ("scm_unavailable", true), "{status}");
    }

    let w = GitLabWorld::new().await;
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp",
        403,
        json!({ "message": "403 Forbidden" }),
    )
    .await;
    let err = w.gl.repository("gitlab", "acme/app").await.unwrap_err();
    assert_eq!(code(&err), ("scm_rejected", false));
    assert!(!format!("{err:?}{}", err.to_json()).contains(TOKEN));
}

#[tokio::test]
async fn pages_follow_the_next_page_header_and_stop_at_twenty() {
    let w = GitLabWorld::new().await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp/pipelines/5/jobs"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 2, "name": "test", "status": "success" }
        ])))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp/pipelines/5/jobs"))
        .and(query_param("per_page", "100"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-next-page", "2")
                .set_body_json(json!([{ "id": 1, "name": "lint", "status": "success" }])),
        )
        .with_priority(5)
        .mount(&w.server)
        .await;
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp/pipelines",
        200,
        json!([{ "id": 5 }]),
    )
    .await;
    serve(
        &w,
        "GET",
        &format!("/projects/acme%2Fapp/repository/commits/{HEAD}/statuses"),
        200,
        json!([]),
    )
    .await;
    let checks = w.gl.commit_checks(&w.repo, HEAD).await.unwrap();
    let names: Vec<&str> = checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["lint", "test"]);

    let w = GitLabWorld::new().await;
    serve(&w, "GET", "/projects/acme%2Fapp/pipelines", 200, json!([])).await;
    Mock::given(method("GET"))
        .and(path(
            format!("/projects/acme%2Fapp/repository/commits/{HEAD}/statuses").as_str(),
        ))
        .respond_with(|request: &wiremock::Request| {
            let page: u32 = request
                .url
                .query_pairs()
                .find(|(k, _)| k == "page")
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(1);
            ResponseTemplate::new(200)
                .insert_header("x-next-page", (page + 1).to_string().as_str())
                .set_body_json(
                    json!([{ "id": page, "name": format!("ci-{page}"), "status": "success" }]),
                )
        })
        .mount(&w.server)
        .await;
    let checks = w.gl.commit_checks(&w.repo, HEAD).await.unwrap();
    assert_eq!(checks.len(), 20);
}

#[tokio::test]
async fn the_newest_pipeline_is_asked_for_and_a_commit_without_one_has_only_statuses() {
    let w = GitLabWorld::new().await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp/pipelines"))
        .and(query_param("sha", HEAD))
        .and(query_param("order_by", "id"))
        .and(query_param("sort", "desc"))
        .and(query_param("per_page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&w.server)
        .await;
    serve(
        &w,
        "GET",
        &format!("/projects/acme%2Fapp/repository/commits/{HEAD}/statuses"),
        200,
        json!([{ "id": 3, "name": "deploy", "status": "success" }]),
    )
    .await;
    let checks = w.gl.commit_checks(&w.repo, HEAD).await.unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].name, "deploy");
}

#[tokio::test]
async fn opening_sends_the_branch_pair_and_a_user_credential_is_unsupported() {
    let w = GitLabWorld::new().await;
    Mock::given(method("POST"))
        .and(path("/projects/acme%2Fapp/merge_requests"))
        .and(body_json(json!({
            "source_branch": BRANCH, "target_branch": "main", "title": "Add the first step",
            "description": "body", "remove_source_branch": false,
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(mr(9, "opened")))
        .expect(1)
        .mount(&w.server)
        .await;
    let mut request = OpenChangeRequest {
        repo: w.repo.clone(),
        head_branch: BRANCH.into(),
        base_branch: "main".into(),
        title: "Add the first step".into(),
        body: "body".into(),
        credential: ChangeCredential::App,
    };
    let ChangeOpening::Opened(opened) = w.gl.open_change_request(&request).await.unwrap() else {
        panic!("not opened");
    };
    assert_eq!(
        (opened.number, opened.head_sha.as_str(), opened.url.as_str()),
        (
            9,
            HEAD,
            "https://gitlab.example/acme/app/-/merge_requests/9"
        )
    );
    request.credential = ChangeCredential::User(SecretText::new("glpat-user".into()));
    let err = w.gl.open_change_request(&request).await.unwrap_err();
    assert_eq!(code(&err), ("scm_capability_unsupported", false));
    assert_eq!(err.params["capability"], "user_authorship");
    assert_eq!(w.server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_422_that_is_not_a_duplicate_stays_an_error() {
    let w = GitLabWorld::new().await;
    serve(
        &w,
        "POST",
        "/projects/acme%2Fapp/merge_requests",
        422,
        json!({ "message": { "title": ["can't be blank"] } }),
    )
    .await;
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp/merge_requests",
        200,
        json!([]),
    )
    .await;
    let request = OpenChangeRequest {
        repo: w.repo.clone(),
        head_branch: BRANCH.into(),
        base_branch: "main".into(),
        title: String::new(),
        body: String::new(),
        credential: ChangeCredential::App,
    };
    let err = w.gl.open_change_request(&request).await.unwrap_err();
    assert_eq!(code(&err), ("scm_rejected", false));
    assert!(
        err.params["message"]
            .as_str()
            .unwrap()
            .contains("can't be blank")
    );
}

#[tokio::test]
async fn snapshots_read_state_head_and_merge_commit() {
    let w = GitLabWorld::new().await;
    let mut locked = mr(1, "locked");
    locked["sha"] = Value::Null;
    locked["diff_refs"] = json!({ "head_sha": MERGED });
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp/merge_requests/1",
        200,
        locked,
    )
    .await;
    serve(&w, "GET", "/projects/acme%2Fapp/pipelines", 200, json!([])).await;
    serve(
        &w,
        "GET",
        &format!("/projects/acme%2Fapp/repository/commits/{MERGED}/statuses"),
        200,
        json!([]),
    )
    .await;
    let snap = w.gl.change_request(&w.repo, 1).await.unwrap();
    assert_eq!(
        (snap.state, snap.head_sha.as_str()),
        (ChangeState::Open, MERGED)
    );

    let cases = [
        (
            json!({ "merge_commit_sha": "a", "squash_commit_sha": "b" }),
            "a",
        ),
        (
            json!({ "merge_commit_sha": null, "squash_commit_sha": "b" }),
            "b",
        ),
        (
            json!({ "merge_commit_sha": "", "squash_commit_sha": null }),
            HEAD,
        ),
    ];
    for (n, (extra, expected)) in cases.into_iter().enumerate() {
        let number = n as u64 + 2;
        let mut merged = mr(number, "merged");
        merged["merge_commit_sha"] = extra["merge_commit_sha"].clone();
        merged["squash_commit_sha"] = extra["squash_commit_sha"].clone();
        serve(
            &w,
            "GET",
            &format!("/projects/acme%2Fapp/merge_requests/{number}"),
            200,
            merged,
        )
        .await;
        let snap = w.gl.change_request(&w.repo, number).await.unwrap();
        assert_eq!(snap.merge_commit_sha.as_deref(), Some(expected));
    }
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp/merge_requests/9",
        404,
        json!({}),
    )
    .await;
    let err = w.gl.change_request(&w.repo, 9).await.unwrap_err();
    assert_eq!(code(&err), ("scm_repo_inaccessible", false));
}

#[tokio::test]
async fn branches_and_files_are_encoded_and_bounded() {
    let w = GitLabWorld::new().await;
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp/repository/branches/saka%2Ftwo-step%2Fs1",
        200,
        json!({ "commit": { "id": HEAD } }),
    )
    .await;
    assert_eq!(
        w.gl.branch_head("gitlab", "acme/app", BRANCH)
            .await
            .unwrap(),
        Some(HEAD.to_string())
    );
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp/repository/branches/gone",
        404,
        json!({}),
    )
    .await;
    assert_eq!(
        w.gl.branch_head("gitlab", "acme/app", "gone")
            .await
            .unwrap(),
        None
    );

    Mock::given(method("GET"))
        .and(path(
            "/projects/acme%2Fapp/repository/files/docs%2Fa.md/raw",
        ))
        .and(query_param("ref", HEAD))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"# hello\n".to_vec()))
        .mount(&w.server)
        .await;
    assert_eq!(
        w.gl.file_at("gitlab", "acme/app", HEAD, "docs/a.md", 100)
            .await
            .unwrap(),
        Some(b"# hello\n".to_vec())
    );
    let err =
        w.gl.file_at("gitlab", "acme/app", HEAD, "docs/a.md", 3)
            .await
            .unwrap_err();
    assert_eq!(err.code, "file_too_large");
    let err =
        w.gl.file_at("gitlab", "acme/app", HEAD, "docs/../a.md", 100)
            .await
            .unwrap_err();
    assert_eq!(err.code, "git_argument_invalid");
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp/repository/files/none/raw",
        404,
        json!({}),
    )
    .await;
    assert_eq!(
        w.gl.file_at("gitlab", "acme/app", HEAD, "none", 100)
            .await
            .unwrap(),
        None
    );

    Mock::given(method("DELETE"))
        .and(path(
            "/projects/acme%2Fapp/repository/branches/saka%2Ftwo-step%2Fs1",
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&w.server)
        .await;
    w.gl.delete_branch(&w.repo, BRANCH).await.unwrap();
    for bad in ["", "a//b", "a/../b"] {
        assert_eq!(
            w.gl.delete_branch(&w.repo, bad).await.unwrap_err().code,
            "git_argument_invalid"
        );
    }
}

#[tokio::test]
async fn merge_sends_the_method_and_maps_each_refusal() {
    let w = GitLabWorld::new().await;
    Mock::given(method("PUT"))
        .and(path("/projects/acme%2Fapp/merge_requests/1/merge"))
        .and(body_json(json!({
            "sha": HEAD, "squash": false, "should_remove_source_branch": false,
            "merge_commit_message": "Add the first step (#1)",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(mr(1, "merged")))
        .expect(1)
        .mount(&w.server)
        .await;
    let mut request = merge_request(&w, 1, MergeMethod::Merge);
    assert!(matches!(
        w.gl.merge(&request).await.unwrap(),
        MergeOutcome::Merged { merge_commit_sha, merged_at: Some(_) } if merge_commit_sha == MERGED
    ));

    request.method = MergeMethod::Rebase;
    let err = w.gl.merge(&request).await.unwrap_err();
    assert_eq!(code(&err), ("scm_capability_unsupported", false));
    assert_eq!(err.params["capability"], "merge_method");
    assert_eq!(w.server.received_requests().await.unwrap().len(), 1);

    request.method = MergeMethod::Squash;
    for (number, status, expected) in [
        (2, 422, "rejected"),
        (3, 405, "not_mergeable"),
        (4, 406, "not_mergeable"),
    ] {
        request.number = number;
        serve(
            &w,
            "PUT",
            &format!("/projects/acme%2Fapp/merge_requests/{number}/merge"),
            status,
            json!({}),
        )
        .await;
        let mut open = mr(number, "opened");
        open["detailed_merge_status"] = json!("conflict");
        serve(
            &w,
            "GET",
            &format!("/projects/acme%2Fapp/merge_requests/{number}"),
            200,
            open,
        )
        .await;
        assert_eq!(
            w.gl.merge(&request).await.unwrap(),
            MergeOutcome::Rejected {
                reason: expected.into()
            },
            "{status}"
        );
    }
    request.number = 5;
    serve(
        &w,
        "PUT",
        "/projects/acme%2Fapp/merge_requests/5/merge",
        403,
        json!({}),
    )
    .await;
    assert_eq!(
        w.gl.merge(&request).await.unwrap(),
        MergeOutcome::Rejected {
            reason: "rejected".into()
        }
    );
}

#[tokio::test]
async fn a_merge_while_gitlab_still_checks_mergeability_is_retried_not_refused() {
    let w = GitLabWorld::new().await;
    let mut request = merge_request(&w, 1, MergeMethod::Squash);
    for (status, detailed) in [(405, "checking"), (422, "unchecked"), (405, "preparing")] {
        let w = GitLabWorld::new().await;
        serve(
            &w,
            "PUT",
            "/projects/acme%2Fapp/merge_requests/1/merge",
            status,
            json!({}),
        )
        .await;
        let mut open = mr(1, "opened");
        open["detailed_merge_status"] = json!(detailed);
        serve(
            &w,
            "GET",
            "/projects/acme%2Fapp/merge_requests/1",
            200,
            open,
        )
        .await;
        request.repo = w.repo.clone();
        let err = w.gl.merge(&request).await.unwrap_err();
        assert_eq!(code(&err), ("scm_unavailable", true), "{detailed}");
        assert_eq!(err.params["detail"], "mergeability_pending");
    }
}

#[tokio::test]
async fn a_200_that_is_not_yet_merged_is_read_once_more_then_retried() {
    let w = GitLabWorld::new().await;
    let request = merge_request(&w, 1, MergeMethod::Squash);
    serve(
        &w,
        "PUT",
        "/projects/acme%2Fapp/merge_requests/1/merge",
        200,
        mr(1, "locked"),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp/merge_requests/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mr(1, "locked")))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&w.server)
        .await;
    let err = w.gl.merge(&request).await.unwrap_err();
    assert_eq!(code(&err), ("scm_unavailable", true));
    assert_eq!(err.params["detail"], "merge_pending");

    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fapp/merge_requests/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mr(1, "merged")))
        .mount(&w.server)
        .await;
    assert!(matches!(
        w.gl.merge(&request).await.unwrap(),
        MergeOutcome::Merged { .. }
    ));
}

#[tokio::test]
async fn a_401_on_merge_is_a_refusal_unless_the_token_itself_is_bad() {
    let w = GitLabWorld::new().await;
    let mut request = merge_request(&w, 1, MergeMethod::Squash);
    serve(
        &w,
        "PUT",
        "/projects/acme%2Fapp/merge_requests/1/merge",
        401,
        json!({}),
    )
    .await;
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp",
        200,
        json!({ "path_with_namespace": "acme/app", "web_url": "u", "default_branch": "main" }),
    )
    .await;
    assert_eq!(
        w.gl.merge(&request).await.unwrap(),
        MergeOutcome::Rejected {
            reason: "rejected".into()
        }
    );

    let w = GitLabWorld::new().await;
    request.repo = w.repo.clone();
    serve(
        &w,
        "PUT",
        "/projects/acme%2Fapp/merge_requests/1/merge",
        401,
        json!({}),
    )
    .await;
    serve(&w, "GET", "/projects/acme%2Fapp", 401, json!({})).await;
    assert_eq!(
        code(&w.gl.merge(&request).await.unwrap_err()),
        ("scm_auth_refused", false)
    );
}

#[tokio::test]
async fn statuses_map_the_state_and_repeating_one_is_accepted() {
    let w = GitLabWorld::new().await;
    let report = |state| StatusReport {
        context: "saka/verify".into(),
        state,
        description: "2 commands passed".into(),
    };
    Mock::given(method("POST"))
        .and(path(
            format!("/projects/acme%2Fapp/statuses/{HEAD}").as_str(),
        ))
        .and(body_json(json!({
            "state": "failed", "name": "saka/verify", "description": "2 commands passed",
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
        .expect(1)
        .mount(&w.server)
        .await;
    w.gl.report_status(&w.repo, HEAD, &report(StatusState::Failure))
        .await
        .unwrap();

    serve(
        &w,
        "POST",
        &format!("/projects/acme%2Fapp/statuses/{HEAD}"),
        400,
        json!({ "message": "Cannot transition status via :run from :running (Reason(s): Status cannot transition via \"run\")" }),
    )
    .await;
    w.gl.report_status(&w.repo, HEAD, &report(StatusState::Pending))
        .await
        .unwrap();

    for status in [403, 404] {
        let w = GitLabWorld::new().await;
        serve(
            &w,
            "POST",
            &format!("/projects/acme%2Fapp/statuses/{HEAD}"),
            status,
            json!({}),
        )
        .await;
        let err =
            w.gl.report_status(&w.repo, HEAD, &report(StatusState::Success))
                .await
                .unwrap_err();
        assert_eq!(code(&err), ("scm_status_refused", false));
    }
    let w = GitLabWorld::new().await;
    serve(
        &w,
        "POST",
        &format!("/projects/acme%2Fapp/statuses/{HEAD}"),
        400,
        json!({ "message": "other" }),
    )
    .await;
    let err =
        w.gl.report_status(&w.repo, HEAD, &report(StatusState::Success))
            .await
            .unwrap_err();
    assert_eq!(err.code, "scm_rejected");
}

#[tokio::test]
async fn push_credentials_are_a_basic_header_on_the_instance_host_only() {
    let w = GitLabWorld::new().await;
    let remote = w.client.remote(&w.repo).await.unwrap();
    assert_eq!(remote.url, "https://gitlab.example/acme/app.git");
    let basic = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(format!("oauth2:{TOKEN}"))
    };
    assert_eq!(
        remote.config,
        [(
            "http.https://gitlab.example/.extraheader".to_string(),
            format!("AUTHORIZATION: basic {basic}")
        )]
    );
    assert!(!format!("{remote:?}").contains(TOKEN));
    assert!(!format!("{remote:?}").contains(&basic));
    let mut bad = w.repo.clone();
    bad.full_name = "acme".into();
    assert_eq!(
        w.client.remote(&bad).await.unwrap_err().code,
        "scm_repo_name_invalid"
    );
}

#[tokio::test]
async fn user_accounts_and_the_app_webhook_status_are_declared_unsupported() {
    let w = GitLabWorld::new().await;
    assert!(!w.gl.device_flow_configured("gitlab").unwrap());
    assert_eq!(
        w.gl.start_device_flow("gitlab").await.unwrap_err().code,
        "scm_capability_unsupported"
    );
    assert_eq!(
        w.gl.webhook_status("gitlab").await.unwrap_err().code,
        "scm_capability_unsupported"
    );
    assert!(!w.gl.webhook_configured("gitlab").unwrap());
    let err = w.gl.parse_webhook("gitlab", &[], b"{}").unwrap_err();
    assert_eq!(err.code, "webhook_not_configured");
}

#[tokio::test]
async fn intake_reads_map_errors_like_the_rest_and_a_failed_lookup_is_not_a_denial() {
    let w = GitLabWorld::new().await;
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp/merge_requests",
        502,
        json!({}),
    )
    .await;
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp/merge_requests/5",
        404,
        json!({ "message": "404 Not found" }),
    )
    .await;
    serve(&w, "GET", "/users", 500, json!({})).await;

    let down = w.gl.list_open_changes(&w.repo, 200).await.unwrap_err();
    assert_eq!(code(&down), ("scm_unavailable", true));
    let missing = w.gl.change_summary(&w.repo, 5).await.unwrap_err();
    assert_eq!(code(&missing), ("scm_repo_inaccessible", false));
    let busy = w.gl.can_write(&w.repo, "alice").await.unwrap_err();
    assert!(busy.retryable);
    for err in [&down, &missing, &busy] {
        assert!(!format!("{err:?}").contains(TOKEN), "{err:?}");
    }
}

#[tokio::test]
async fn a_member_below_developer_cannot_write_and_a_missing_membership_cannot_either() {
    let w = GitLabWorld::new().await;
    serve(
        &w,
        "GET",
        "/users",
        200,
        json!([{ "id": 7, "username": "alice" }]),
    )
    .await;
    serve(
        &w,
        "GET",
        "/projects/acme%2Fapp/members/all/7",
        404,
        json!({ "message": "404 Not found" }),
    )
    .await;
    assert!(!w.gl.can_write(&w.repo, "alice").await.unwrap());
}

#[tokio::test]
async fn an_approval_the_host_refuses_after_the_note_is_a_permanent_refusal() {
    let w = GitLabWorld::new().await;
    let base = "/projects/acme%2Fapp/merge_requests/3";
    serve(
        &w,
        "POST",
        &format!("{base}/notes"),
        201,
        json!({ "id": 1 }),
    )
    .await;
    serve(
        &w,
        "POST",
        &format!("{base}/approve"),
        409,
        json!({ "message": "SHA does not match HEAD of source branch" }),
    )
    .await;
    let post = ReviewPost {
        repo: w.repo.clone(),
        number: 3,
        head_sha: HEAD.into(),
        event: saka_core::vocab::ReviewEvent::Approve,
        body: "Looks right".into(),
    };
    let err = w.gl.post_review(&post).await.unwrap_err();
    assert_eq!(err.code, "review_refused");
    assert!(!err.retryable);
    assert_eq!(err.params["status"], 409);
    assert!(!err.to_string().contains(TOKEN));
}

#[tokio::test]
async fn a_comment_is_posted_as_a_merge_request_note_and_a_refusal_is_permanent() {
    let w = GitLabWorld::new().await;
    Mock::given(method("POST"))
        .and(path("/projects/acme%2Fapp/merge_requests/3/notes"))
        .and(header("private-token", TOKEN))
        .and(body_json(json!({ "body": "Please update the branch." })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 9 })))
        .expect(1)
        .mount(&w.server)
        .await;
    serve(
        &w,
        "POST",
        "/projects/acme%2Fapp/merge_requests/4/notes",
        403,
        json!({ "message": "403 Forbidden" }),
    )
    .await;
    serve(
        &w,
        "POST",
        "/projects/acme%2Fapp/merge_requests/5/notes",
        502,
        json!({}),
    )
    .await;

    w.gl.post_comment(&w.repo, 3, "Please update the branch.")
        .await
        .unwrap();
    let refused = w.gl.post_comment(&w.repo, 4, "x").await.unwrap_err();
    assert_eq!(code(&refused), ("comment_refused", false));
    let down = w.gl.post_comment(&w.repo, 5, "x").await.unwrap_err();
    assert_eq!(code(&down), ("scm_unavailable", true));
}

#[tokio::test]
async fn updating_a_branch_is_unsupported_and_sends_nothing() {
    let w = GitLabWorld::new().await;
    let err = w.gl.update_branch(&w.repo, 1, HEAD).await.unwrap_err();
    assert_eq!(code(&err), ("scm_capability_unsupported", false));
    assert_eq!(err.params["capability"], "update_branch");
    assert!(w.server.received_requests().await.unwrap().is_empty());
}
