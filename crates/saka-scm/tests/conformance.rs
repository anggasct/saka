#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

mod support;

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use saka_core::gate::outcome;
use saka_core::vocab::{
    ChangeState, CheckConclusion, CheckKind, CheckStatus, GateOutcome, MergeMethod,
};
use saka_ports::{
    MergeOutcome, MergeRequest, OpenChangeRequest, RepoRef, Scm, ScmCapabilities, WebhookTarget,
};
use serde_json::{Value, json};
use sha2::Sha256;
use support::{HEAD, MERGED, World, pr};
use wiremock::matchers::{body_json, body_partial_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

const SECRET: &[u8] = b"conformance-secret-0123456789abcdef";
const BRANCH: &str = "saka/two-step/s1";

enum Change {
    Open,
    Merged,
    Closed,
}

enum MergeAnswer {
    Merged,
    HeadChanged,
    NotMergeable,
}

enum Signing {
    Valid,
    UpperHex,
    Wrong,
    Missing,
}

struct Check {
    id: u64,
    name: &'static str,
    done: Option<&'static str>,
}

#[async_trait]
trait Harness: Sync {
    fn scm(&self) -> &dyn Scm;
    fn repo(&self) -> RepoRef;
    fn integration(&self) -> &str;
    fn declared(&self) -> ScmCapabilities;
    async fn repository(&self, default_branch: &str);
    async fn missing_repository(&self, full_name: &str);
    async fn change_already_open(&self, number: u64);
    async fn change(&self, number: u64, state: Change);
    async fn checks(&self, checks: &[Check], pending_status: &str);
    async fn merge_answers(&self, number: u64, answer: MergeAnswer);
    async fn branch_missing(&self);
    async fn status_accepted(&self, state: &str, description: &str);
    fn ci_webhook(&self, numbers: &[u64], signing: Signing) -> (Vec<(String, String)>, Vec<u8>);
}

struct GitHubHarness {
    w: World,
}

impl GitHubHarness {
    async fn new() -> GitHubHarness {
        let mut w = World::new().await;
        w.gh = saka_scm::GitHub::new(w.app.clone(), std::sync::Arc::new(support::NoRepos))
            .with_webhook_secret(SECRET.to_vec());
        w.mount_tokens().await;
        GitHubHarness { w }
    }
}

fn hmac_hex(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
    mac.update(body);
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[async_trait]
impl Harness for GitHubHarness {
    fn scm(&self) -> &dyn Scm {
        &self.w.gh
    }

    fn repo(&self) -> RepoRef {
        self.w.repo.clone()
    }

    fn integration(&self) -> &str {
        "github"
    }

    fn declared(&self) -> ScmCapabilities {
        ScmCapabilities {
            provider: "github",
            merge_methods: &[MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase],
            webhook_signature: Some(saka_ports::WebhookSignature::HmacSha256),
            check_kinds: &[CheckKind::CheckRun, CheckKind::CommitStatus],
            draft_change_requests: true,
            delete_branch_after_merge: true,
            commit_status: true,
        }
    }

    async fn repository(&self, default_branch: &str) {
        Mock::given(method("GET"))
            .and(path("/repos/acme/app"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "full_name": "acme/app", "html_url": "https://github.example/acme/app",
                "default_branch": default_branch,
            })))
            .mount(&self.w.server)
            .await;
    }

    async fn missing_repository(&self, full_name: &str) {
        Mock::given(method("GET"))
            .and(path(format!("/repos/{full_name}/installation").as_str()))
            .respond_with(ResponseTemplate::new(404))
            .mount(&self.w.server)
            .await;
    }

    async fn change_already_open(&self, number: u64) {
        Mock::given(method("POST"))
            .and(path("/repos/acme/app/pulls"))
            .and(body_partial_json(
                json!({ "head": BRANCH, "base": "main", "draft": false }),
            ))
            .respond_with(ResponseTemplate::new(422).set_body_json(json!({
                "message": "Validation Failed",
                "errors": [{ "resource": "PullRequest", "code": "custom",
                             "message": format!("A pull request already exists for acme:{BRANCH}.") }],
            })))
            .mount(&self.w.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/app/pulls"))
            .and(query_param("head", format!("acme:{BRANCH}").as_str()))
            .and(query_param("state", "all"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([pr(number, "open", false)])),
            )
            .mount(&self.w.server)
            .await;
    }

    async fn change(&self, number: u64, state: Change) {
        let body = match state {
            Change::Open => pr(number, "open", false),
            Change::Merged => pr(number, "closed", true),
            Change::Closed => pr(number, "closed", false),
        };
        Mock::given(method("GET"))
            .and(path(format!("/repos/acme/app/pulls/{number}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.w.server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                format!("/repos/acme/app/issues/{number}/events").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "event": "referenced", "commit_id": HEAD },
                { "event": "merged", "commit_id": MERGED },
                { "event": "closed", "commit_id": null },
            ])))
            .mount(&self.w.server)
            .await;
    }

    async fn checks(&self, checks: &[Check], pending_status: &str) {
        let runs: Vec<Value> = checks
            .iter()
            .map(|c| {
                json!({
                    "id": c.id, "name": c.name,
                    "status": if c.done.is_some() { "completed" } else { "in_progress" },
                    "conclusion": c.done,
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path(
                format!("/repos/acme/app/commits/{HEAD}/check-runs").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "total_count": runs.len(), "check_runs": runs })),
            )
            .mount(&self.w.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/repos/acme/app/commits/{HEAD}/status").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "state": "pending",
                "statuses": [{ "context": pending_status, "state": "pending", "target_url": "https://ci.example/1" }],
            })))
            .mount(&self.w.server)
            .await;
    }

    async fn merge_answers(&self, number: u64, answer: MergeAnswer) {
        let (status, body) = match answer {
            MergeAnswer::Merged => (200, json!({ "sha": MERGED, "merged": true })),
            MergeAnswer::HeadChanged => (409, json!({ "message": "Head branch was modified" })),
            MergeAnswer::NotMergeable => {
                (405, json!({ "message": "Pull Request is not mergeable" }))
            }
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
            .mount(&self.w.server)
            .await;
    }

    async fn status_accepted(&self, state: &str, description: &str) {
        Mock::given(method("POST"))
            .and(path(format!("/repos/acme/app/statuses/{HEAD}").as_str()))
            .and(body_json(json!({
                "state": state, "context": "saka/verify", "description": description,
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn branch_missing(&self) {
        Mock::given(method("DELETE"))
            .and(path(
                format!("/repos/acme/app/git/refs/heads/{BRANCH}").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(422)
                    .set_body_json(json!({ "message": "Reference does not exist" })),
            )
            .mount(&self.w.server)
            .await;
    }

    fn ci_webhook(&self, numbers: &[u64], signing: Signing) -> (Vec<(String, String)>, Vec<u8>) {
        let prs: Vec<Value> = numbers.iter().map(|n| json!({ "number": n })).collect();
        let body = serde_json::to_vec(&json!({
            "action": "completed",
            "check_run": { "head_sha": HEAD, "pull_requests": prs },
            "repository": { "full_name": "acme/app" },
        }))
        .unwrap();
        let mut headers = vec![
            ("X-GitHub-Event".to_string(), "check_run".to_string()),
            ("X-GitHub-Delivery".to_string(), "9f1c-delivery".to_string()),
        ];
        let signature = match signing {
            Signing::Valid => Some(format!("sha256={}", hmac_hex(&body))),
            Signing::UpperHex => Some(format!("sha256={}", hmac_hex(&body).to_uppercase())),
            Signing::Wrong => Some(format!("sha256={}", hmac_hex(b"another body"))),
            Signing::Missing => None,
        };
        if let Some(s) = signature {
            headers.push(("X-Hub-Signature-256".to_string(), s));
        }
        (headers, body)
    }
}

fn merge_request(h: &dyn Harness, number: u64) -> MergeRequest {
    MergeRequest {
        repo: h.repo(),
        number,
        head_sha: HEAD.into(),
        method: MergeMethod::Squash,
        commit_title: format!("Add the first step (#{number})"),
    }
}

async fn repository_info_reads_the_default_branch(h: &dyn Harness) {
    h.repository("trunk").await;
    let info = h
        .scm()
        .repository(h.integration(), "acme/app")
        .await
        .unwrap();
    assert_eq!(info.default_branch, "trunk");
    assert_eq!(info.full_name, "acme/app");
}

async fn open_change_request_is_idempotent_for_the_same_branch(h: &dyn Harness) {
    h.change_already_open(7).await;
    let opened = h
        .scm()
        .open_change_request(&OpenChangeRequest {
            repo: h.repo(),
            head_branch: BRANCH.into(),
            base_branch: "main".into(),
            title: "Add the first step".into(),
            body: "body\n\n<!-- saka-task: two-step/s1 -->".to_string(),
        })
        .await
        .unwrap();
    assert_eq!((opened.number, opened.head_sha.as_str()), (7, HEAD));
}

async fn change_request_snapshot_normalizes_checks_and_statuses(h: &dyn Harness) {
    h.change(7, Change::Open).await;
    h.checks(
        &[
            Check {
                id: 11,
                name: "lint",
                done: Some("success"),
            },
            Check {
                id: 12,
                name: "test",
                done: None,
            },
        ],
        "deploy/preview",
    )
    .await;
    let snap = h.scm().change_request(&h.repo(), 7).await.unwrap();
    assert_eq!(
        (snap.state, snap.head_sha.as_str()),
        (ChangeState::Open, HEAD)
    );
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

async fn latest_rerun_of_a_check_wins(h: &dyn Harness) {
    h.change(7, Change::Open).await;
    h.checks(
        &[
            Check {
                id: 20,
                name: "test",
                done: Some("failure"),
            },
            Check {
                id: 21,
                name: "test",
                done: Some("success"),
            },
        ],
        "deploy/preview",
    )
    .await;
    let snap = h.scm().change_request(&h.repo(), 7).await.unwrap();
    let tests: Vec<_> = snap.checks.iter().filter(|c| c.name == "test").collect();
    assert_eq!(tests.len(), 1);
    assert_eq!(tests[0].conclusion, Some(CheckConclusion::Success));
}

async fn merged_change_reports_its_merge_commit(h: &dyn Harness) {
    h.change(7, Change::Merged).await;
    h.change(8, Change::Closed).await;
    let merged = h.scm().change_request(&h.repo(), 7).await.unwrap();
    assert_eq!(merged.state, ChangeState::Merged);
    assert_eq!(merged.merge_commit_sha.as_deref(), Some(MERGED));
    assert!(merged.merged_at.is_some());
    let closed = h.scm().change_request(&h.repo(), 8).await.unwrap();
    assert_eq!(
        (closed.state, closed.merge_commit_sha),
        (ChangeState::Closed, None)
    );
}

async fn merge_reports_its_outcomes(h: &dyn Harness) {
    h.merge_answers(1, MergeAnswer::Merged).await;
    assert!(matches!(
        h.scm().merge(&merge_request(h, 1)).await.unwrap(),
        MergeOutcome::Merged { merge_commit_sha, .. } if merge_commit_sha == MERGED
    ));
}

async fn merge_at_a_stale_head_is_head_changed(h: &dyn Harness) {
    h.merge_answers(3, MergeAnswer::HeadChanged).await;
    assert_eq!(
        h.scm().merge(&merge_request(h, 3)).await.unwrap(),
        MergeOutcome::Rejected {
            reason: "head_changed".into()
        }
    );
}

async fn merge_of_an_unmergeable_change_is_not_mergeable(h: &dyn Harness) {
    h.merge_answers(2, MergeAnswer::NotMergeable).await;
    assert_eq!(
        h.scm().merge(&merge_request(h, 2)).await.unwrap(),
        MergeOutcome::Rejected {
            reason: "not_mergeable".into()
        }
    );
}

async fn delete_of_a_missing_branch_succeeds(h: &dyn Harness) {
    h.branch_missing().await;
    h.scm().delete_branch(&h.repo(), BRANCH).await.unwrap();
}

async fn unknown_repository_is_inaccessible(h: &dyn Harness) {
    h.missing_repository("acme/gone").await;
    let err = h
        .scm()
        .repository(h.integration(), "acme/gone")
        .await
        .unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.retryable),
        ("scm_repo_inaccessible", false)
    );
}

async fn webhook_with_a_valid_signature_parses_its_targets(h: &dyn Harness) {
    assert!(h.scm().webhook_configured(h.integration()).unwrap());
    let (headers, body) = h.ci_webhook(&[7], Signing::Valid);
    let delivery = h
        .scm()
        .parse_webhook(h.integration(), &headers, &body)
        .unwrap();
    assert_eq!(delivery.repo_full_name.as_deref(), Some("acme/app"));
    assert!(!delivery.delivery_id.is_empty());
    assert!(
        delivery
            .targets
            .contains(&WebhookTarget::Change { number: 7 })
    );
    assert!(
        delivery
            .targets
            .contains(&WebhookTarget::Head { sha: HEAD.into() })
    );
}

async fn webhook_with_a_bad_or_missing_signature_is_refused(h: &dyn Harness) {
    for signing in [Signing::Wrong, Signing::Missing] {
        let (headers, body) = h.ci_webhook(&[7], signing);
        let err = h
            .scm()
            .parse_webhook(h.integration(), &headers, &body)
            .unwrap_err();
        assert_eq!(
            (err.code.as_str(), err.retryable),
            ("webhook_signature_invalid", false)
        );
    }
}

async fn webhook_signature_compare_ignores_hex_case(h: &dyn Harness) {
    let (headers, body) = h.ci_webhook(&[7], Signing::UpperHex);
    assert!(
        h.scm()
            .parse_webhook(h.integration(), &headers, &body)
            .is_ok()
    );
}

async fn commit_status_is_reported_for_a_head(h: &dyn Harness) {
    if !h.declared().commit_status {
        return;
    }
    h.status_accepted("success", "2 commands passed").await;
    let report = saka_ports::StatusReport {
        context: "saka/verify".into(),
        state: saka_ports::StatusState::Success,
        description: "2 commands passed".into(),
    };
    h.scm()
        .report_status(&h.repo(), HEAD, &report)
        .await
        .unwrap();
    let bad = h
        .scm()
        .report_status(&h.repo(), "HEAD", &report)
        .await
        .unwrap_err();
    assert_eq!(bad.code, "git_argument_invalid");
}

async fn capabilities_match_the_declared_matrix(h: &dyn Harness) {
    assert_eq!(h.scm().capabilities(h.integration()), Some(h.declared()));
}

macro_rules! conformance {
    ($harness:ident: $($case:ident),* $(,)?) => {
        mod github_conformance {
            use super::*;
            $(
                #[tokio::test]
                async fn $case() {
                    let h = $harness::new().await;
                    super::$case(&h).await;
                }
            )*
        }
    };
}

conformance!(GitHubHarness:
    repository_info_reads_the_default_branch,
    open_change_request_is_idempotent_for_the_same_branch,
    change_request_snapshot_normalizes_checks_and_statuses,
    latest_rerun_of_a_check_wins,
    merged_change_reports_its_merge_commit,
    merge_reports_its_outcomes,
    merge_at_a_stale_head_is_head_changed,
    merge_of_an_unmergeable_change_is_not_mergeable,
    delete_of_a_missing_branch_succeeds,
    unknown_repository_is_inaccessible,
    webhook_with_a_valid_signature_parses_its_targets,
    webhook_with_a_bad_or_missing_signature_is_refused,
    webhook_signature_compare_ignores_hex_case,
    commit_status_is_reported_for_a_head,
    capabilities_match_the_declared_matrix,
);
