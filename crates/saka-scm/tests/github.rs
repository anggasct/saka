#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use async_trait::async_trait;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use saka_core::gate::outcome;
use saka_core::ids::RepoId;
use saka_core::time::Timestamp;
use saka_core::vocab::{
    ChangeState, CheckConclusion, CheckKind, CheckStatus, GateOutcome, MergeMethod,
};
use saka_ports::{
    Clock, GitAccess, MergeOutcome, MergeRequest, OpenChangeRequest, PortError, PortResult,
    PushOutcome, PushRequest, RepoRef, Repos, Scm,
};
use saka_scm::{GitHub, GitHubApp, GitHubConfig};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::matchers::{
    body_json, body_partial_json, header, method, path, query_param, query_param_is_missing,
};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

const APP_ID: u64 = 123_456;
const NOW_MS: i64 = 1_767_225_600_000;
const HEAD: &str = "1111111111111111111111111111111111111111";
const MERGED: &str = "2222222222222222222222222222222222222222";

struct TestClock(AtomicI64);

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.0.load(Ordering::SeqCst))
    }
}

struct NoRepos;

#[async_trait]
impl Repos for NoRepos {
    async fn fetch(&self, _repo: &RepoRef) -> PortResult<()> {
        Ok(())
    }
    async fn tip(&self, _repo: &RepoRef, _branch: &str) -> PortResult<Option<String>> {
        Ok(None)
    }
    async fn read_file(&self, _r: &RepoRef, _c: &str, _p: &str) -> PortResult<Option<Vec<u8>>> {
        Ok(None)
    }
    async fn push(&self, request: &PushRequest) -> PortResult<PushOutcome> {
        Ok(PushOutcome::Pushed {
            sha: request.local_sha.clone(),
        })
    }
}

struct AppJwt {
    public_pem: Vec<u8>,
    clock: Arc<TestClock>,
}

impl Match for AppJwt {
    fn matches(&self, request: &Request) -> bool {
        let Some(token) = request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
        else {
            return false;
        };
        let Ok(header) = jsonwebtoken::decode_header(token) else {
            return false;
        };
        let Ok(key) = DecodingKey::from_rsa_pem(&self.public_pem) else {
            return false;
        };
        let mut validation = Validation::new(Algorithm::RS256);
        validation.validate_exp = false;
        validation.required_spec_claims.clear();
        let Ok(data) = jsonwebtoken::decode::<Value>(token, &key, &validation) else {
            return false;
        };
        let now = self.clock.now().0 / 1000;
        let c = &data.claims;
        header.alg == Algorithm::RS256
            && c["iss"].as_str() == Some(APP_ID.to_string().as_str())
            && c["iat"] == now - 60
            && c["exp"]
                .as_i64()
                .is_some_and(|exp| exp > now && exp <= now + 600)
    }
}

struct World {
    _dir: tempfile::TempDir,
    server: MockServer,
    clock: Arc<TestClock>,
    public_pem: Vec<u8>,
    app: Arc<GitHubApp>,
    gh: GitHub,
    repo: RepoRef,
}

fn openssl(args: &[&str]) {
    let out = Command::new("openssl")
        .args(args)
        .output()
        .expect("the openssl CLI is needed to make a test key");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rfc3339(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms).unwrap().to_string()
}

impl World {
    async fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("app.pem");
        let public = dir.path().join("app.pub.pem");
        let k = key.to_str().unwrap();
        openssl(&[
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
            k,
        ]);
        openssl(&[
            "pkey",
            "-in",
            k,
            "-pubout",
            "-out",
            public.to_str().unwrap(),
        ]);
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let server = MockServer::start().await;
        let clock = Arc::new(TestClock(AtomicI64::new(NOW_MS)));
        let app = Arc::new(
            GitHubApp::new(
                GitHubConfig {
                    integration_id: "github".into(),
                    api_url: server.uri(),
                    web_url: "https://github.example".into(),
                    app_id: APP_ID,
                    private_key_file: key,
                    user_agent: "saka-test".into(),
                },
                clock.clone(),
            )
            .unwrap(),
        );
        let gh = GitHub::new(app.clone(), Arc::new(NoRepos));
        World {
            public_pem: std::fs::read(&public).unwrap(),
            _dir: dir,
            server,
            clock,
            app,
            gh,
            repo: RepoRef {
                repo_id: RepoId(Uuid::from_u128(1)),
                integration_id: "github".into(),
                full_name: "acme/app".into(),
                url: "https://github.example/acme/app".into(),
            },
        }
    }

    fn jwt(&self) -> AppJwt {
        AppJwt {
            public_pem: self.public_pem.clone(),
            clock: self.clock.clone(),
        }
    }

    async fn mount_tokens(&self) {
        Mock::given(method("GET"))
            .and(path("/repos/acme/app/installation"))
            .and(self.jwt())
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": 42 })))
            .mount(&self.server)
            .await;
        let clock = self.clock.clone();
        let issued = Arc::new(AtomicI64::new(0));
        Mock::given(method("POST"))
            .and(path("/app/installations/42/access_tokens"))
            .and(body_json(json!({ "repositories": ["app"] })))
            .and(self.jwt())
            .respond_with(move |_: &Request| {
                let n = issued.fetch_add(1, Ordering::SeqCst) + 1;
                ResponseTemplate::new(201).set_body_json(json!({
                    "token": format!("ghs_{n}"),
                    "expires_at": rfc3339(clock.now().0 + 3_600_000),
                }))
            })
            .mount(&self.server)
            .await;
    }

    async fn token_requests(&self) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path().ends_with("/access_tokens"))
            .count()
    }

    fn advance_to(&self, ms: i64) {
        self.clock.0.store(ms, Ordering::SeqCst);
    }
}

fn pr(number: u64, state: &str, merged: bool) -> Value {
    json!({
        "number": number,
        "html_url": format!("https://github.example/acme/app/pull/{number}"),
        "state": state,
        "merged": merged,
        "merged_at": if merged { json!("2026-01-01T00:10:00Z") } else { Value::Null },
        "head": { "ref": "saka/two-step/s1", "sha": HEAD },
    })
}

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
    // base64("x-access-token:ghs_1")
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
async fn opening_a_change_that_exists_resolves_to_the_existing_one() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/app/pulls"))
        .and(header("authorization", "Bearer ghs_1"))
        .and(header("x-github-api-version", "2026-03-10"))
        .and(body_partial_json(
            json!({ "head": "saka/two-step/s1", "base": "main", "draft": false }),
        ))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "message": "Validation Failed",
            "errors": [{ "resource": "PullRequest", "code": "custom",
                         "message": "A pull request already exists for acme:saka/two-step/s1." }],
        })))
        .expect(1)
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/pulls"))
        .and(query_param("head", "acme:saka/two-step/s1"))
        .and(query_param("state", "all"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([pr(7, "open", false)])))
        .expect(1)
        .mount(&w.server)
        .await;

    let opened =
        w.gh.open_change_request(&OpenChangeRequest {
            repo: w.repo.clone(),
            head_branch: "saka/two-step/s1".into(),
            base_branch: "main".into(),
            title: "Add the first step".into(),
            body: "body\n\n<!-- saka-task: two-step/s1 -->".into(),
        })
        .await
        .unwrap();
    assert_eq!(opened.number, 7);
    assert_eq!(opened.head_sha, HEAD);
    assert_eq!(opened.url, "https://github.example/acme/app/pull/7");
}

#[tokio::test]
async fn check_runs_over_pages_and_statuses_normalize_to_one_gate_outcome() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr(7, "open", false)))
        .mount(&w.server)
        .await;
    let runs = format!("/repos/acme/app/commits/{HEAD}/check-runs");
    Mock::given(method("GET"))
        .and(path(runs.as_str()))
        .and(query_param_is_missing("page"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "link",
                    format!(
                        "<{}{runs}?per_page=100&page=2>; rel=\"next\"",
                        w.server.uri()
                    )
                    .as_str(),
                )
                .set_body_json(json!({ "total_count": 3, "check_runs": [
                    { "id": 10, "name": "test", "status": "completed", "conclusion": "failure" },
                    { "id": 11, "name": "lint", "status": "completed", "conclusion": "success" },
                ]})),
        )
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path(runs.as_str()))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({ "total_count": 3, "check_runs": [
                { "id": 12, "name": "test", "status": "in_progress", "conclusion": null },
            ]}),
        ))
        .mount(&w.server)
        .await;
    let status = format!("/repos/acme/app/commits/{HEAD}/status");
    Mock::given(method("GET"))
        .and(path(status.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "state": "pending", "statuses": [
            { "context": "deploy/preview", "state": "pending", "target_url": "https://ci.example/1" },
        ]})))
        .mount(&w.server)
        .await;

    let snap = w.gh.change_request(&w.repo, 7).await.unwrap();
    assert_eq!(snap.state, ChangeState::Open);
    assert_eq!(snap.head_sha, HEAD);
    let summary: Vec<(&str, CheckKind, CheckStatus, Option<CheckConclusion>)> = snap
        .checks
        .iter()
        .map(|c| (c.name.as_str(), c.kind, c.status, c.conclusion))
        .collect();
    assert_eq!(
        summary,
        [
            (
                "lint",
                CheckKind::CheckRun,
                CheckStatus::Completed,
                Some(CheckConclusion::Success)
            ),
            ("test", CheckKind::CheckRun, CheckStatus::InProgress, None),
            (
                "deploy/preview",
                CheckKind::CommitStatus,
                CheckStatus::InProgress,
                None
            ),
        ]
    );
    assert_eq!(outcome(&snap.checks), GateOutcome::Pending);
}

#[tokio::test]
async fn a_merged_change_reports_its_merge_commit_and_a_closed_one_reports_closed() {
    let w = World::new().await;
    w.mount_tokens().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr(7, "closed", true)))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/issues/7/events"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "event": "referenced", "commit_id": HEAD },
            { "event": "merged", "commit_id": MERGED },
            { "event": "closed", "commit_id": null },
        ])))
        .mount(&w.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/pulls/8"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr(8, "closed", false)))
        .mount(&w.server)
        .await;

    let merged = w.gh.change_request(&w.repo, 7).await.unwrap();
    assert_eq!(merged.state, ChangeState::Merged);
    assert_eq!(merged.merge_commit_sha.as_deref(), Some(MERGED));
    assert_eq!(merged.merged_at, Some(Timestamp(NOW_MS + 600_000)));
    let closed = w.gh.change_request(&w.repo, 8).await.unwrap();
    assert_eq!(
        (closed.state, closed.merge_commit_sha),
        (ChangeState::Closed, None)
    );
}

#[tokio::test]
async fn merge_sends_the_gate_head_and_maps_refusals() {
    let w = World::new().await;
    w.mount_tokens().await;
    for (number, status) in [(1u64, 200u16), (2, 405), (3, 409), (4, 422)] {
        let body = if status == 200 {
            json!({ "sha": MERGED, "merged": true, "message": "Pull Request successfully merged" })
        } else {
            json!({ "message": "nope" })
        };
        Mock::given(method("PUT"))
            .and(path(
                format!("/repos/acme/app/pulls/{number}/merge").as_str(),
            ))
            .and(body_json(json!({
                "sha": HEAD,
                "merge_method": "squash",
                "commit_title": format!("Add the first step (#{number})"),
            })))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .expect(1)
            .mount(&w.server)
            .await;
    }
    let merge = |number: u64| MergeRequest {
        repo: w.repo.clone(),
        number,
        head_sha: HEAD.into(),
        method: MergeMethod::Squash,
        commit_title: format!("Add the first step (#{number})"),
    };
    assert_eq!(
        w.gh.merge(&merge(1)).await.unwrap(),
        MergeOutcome::Merged {
            merge_commit_sha: MERGED.into(),
            merged_at: None
        }
    );
    let reason = |o: MergeOutcome| match o {
        MergeOutcome::Rejected { reason } => reason,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        reason(w.gh.merge(&merge(2)).await.unwrap()),
        "not_mergeable"
    );
    assert_eq!(reason(w.gh.merge(&merge(3)).await.unwrap()), "head_changed");
    assert_eq!(reason(w.gh.merge(&merge(4)).await.unwrap()), "rejected");
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
