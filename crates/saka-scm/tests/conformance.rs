#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

mod support;

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use saka_core::diagnostics::{CredentialStatus, RepoGrant, RepoHooks, ScmDiagnostics};
use saka_core::gate::outcome;
use saka_core::vocab::{
    ChangeState, CheckConclusion, CheckKind, CheckStatus, GateOutcome, MergeMethod, ReviewEvent,
};
use saka_ports::{
    ChangeCredential, ChangeOpening, EnqueueOutcome, MergeOutcome, MergeRequest, OpenChangeRequest,
    RepoRef, ReviewPost, Scm, ScmCapabilities, SignalKind, UpdateOutcome, WebhookTarget,
};
use serde_json::{Value, json};
use sha2::Sha256;
use support::gitlab::{self, GitLabWorld, TOKEN};
use support::{HEAD, MERGED, NOW_MS, World, pr};
use wiremock::matchers::{
    body_json, body_partial_json, body_string_contains, header, method, path, query_param,
};
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

enum Enqueue {
    Queued,
    HeadChanged,
    Merged,
    Refused,
}

enum UpdateAnswer {
    Accepted,
    Conflict,
    HeadChanged,
}

enum RerunAnswer {
    Accepted,
    Refused(u16),
    NotCalled,
}

enum Signing {
    Valid,
    UpperHex,
    Wrong,
    Missing,
}

struct Listed {
    number: u64,
    title: &'static str,
    author: &'static str,
    labels: &'static [&'static str],
    draft: bool,
    fork: bool,
    branch: &'static str,
    updated: i64,
}

struct Commented {
    id: u64,
    actor: &'static str,
    body: &'static str,
    at: i64,
}

struct Labeled {
    id: u64,
    actor: &'static str,
    label: &'static str,
    at: i64,
}

enum Access {
    Write,
    ReadOnly,
    Stranger,
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
    async fn checks(&self, checks: &[Check], pending_status: &str) {
        self.checks_at(HEAD, checks, pending_status).await;
    }
    async fn checks_at(&self, sha: &str, checks: &[Check], pending_status: &str);
    async fn failing_checks(&self, rerunnable: &[&str], foreign: &[&str]);
    async fn rerun_answers(&self, answer: RerunAnswer);
    async fn merge_answers(&self, number: u64, answer: MergeAnswer);
    async fn queue_state(&self, enabled: bool);
    async fn enqueue_answers(&self, number: u64, answer: Enqueue);
    async fn queue_member(&self, number: u64, member: bool);
    async fn open_changes(&self, changes: &[Listed]);
    async fn change_files(&self, number: u64, files: &[&str]);
    async fn signals(&self, number: u64, comments: &[Commented], labels: &[Labeled]);
    async fn write_access(&self, login: &str, access: Access);
    async fn branch_missing(&self);
    async fn review_accepted(&self, number: u64, event: ReviewEvent, body: &str);
    async fn review_refused(&self, number: u64, status: u16);
    async fn comment_accepted(&self, number: u64, body: &str);
    async fn update_answers(&self, number: u64, answer: UpdateAnswer);
    async fn status_accepted(&self, state: &str, description: &str);
    fn ci_webhook(&self, numbers: &[u64], signing: Signing) -> (Vec<(String, String)>, Vec<u8>);
    async fn diagnosis_answers(&self);
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
            merge_queue: true,
            update_branch: true,
            user_authorship: true,
            change_head_ref: "refs/pull/{number}/head",
            diagnostics: Some(saka_scm::github::requirements::APP),
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

    async fn checks_at(&self, sha: &str, checks: &[Check], pending_status: &str) {
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
                format!("/repos/acme/app/commits/{sha}/check-runs").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "total_count": runs.len(), "check_runs": runs })),
            )
            .mount(&self.w.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/repos/acme/app/commits/{sha}/status").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "state": "pending",
                "statuses": [{ "context": pending_status, "state": "pending", "target_url": "https://ci.example/1" }],
            })))
            .mount(&self.w.server)
            .await;
    }

    async fn failing_checks(&self, rerunnable: &[&str], foreign: &[&str]) {
        let mut runs: Vec<Value> = rerunnable
            .iter()
            .enumerate()
            .map(|(i, name)| {
                json!({
                    "id": 40 + i, "name": name, "status": "completed", "conclusion": "failure",
                    "app": { "slug": "github-actions" },
                    "details_url": format!("https://github.com/acme/app/actions/runs/700/job/{}", 40 + i),
                })
            })
            .collect();
        runs.extend(foreign.iter().enumerate().map(|(i, name)| {
            json!({
                "id": 60 + i, "name": name, "status": "completed", "conclusion": "failure",
                "app": { "slug": "codecov" }, "details_url": "https://codecov.example/run/1",
            })
        }));
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
            .and(path(
                format!("/repos/acme/app/commits/{HEAD}/status").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "state": "failure", "statuses": [] })),
            )
            .mount(&self.w.server)
            .await;
    }

    async fn rerun_answers(&self, answer: RerunAnswer) {
        let (status, times) = match answer {
            RerunAnswer::Accepted => (201, 1),
            RerunAnswer::Refused(status) => (status, 1),
            RerunAnswer::NotCalled => (201, 0),
        };
        Mock::given(method("POST"))
            .and(path("/repos/acme/app/actions/runs/700/rerun-failed-jobs"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({})))
            .expect(times)
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

    async fn queue_state(&self, enabled: bool) {
        let queue = if enabled {
            json!({ "id": "MQ_1" })
        } else {
            Value::Null
        };
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("mergeQueue(branch"))
            .and(body_partial_json(
                json!({ "variables": { "owner": "acme", "name": "app", "branch": "main" } }),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "data": { "repository": { "mergeQueue": queue } } })),
            )
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn enqueue_answers(&self, number: u64, answer: Enqueue) {
        let merged = matches!(answer, Enqueue::Merged);
        let head = if matches!(answer, Enqueue::HeadChanged) {
            "3333333333333333333333333333333333333333"
        } else {
            HEAD
        };
        Mock::given(method("GET"))
            .and(path(format!("/repos/acme/app/pulls/{number}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "node_id": format!("PR_{number}"),
                "state": if merged { "closed" } else { "open" },
                "merged": merged,
                "merged_at": if merged { json!("2026-01-01T00:10:00Z") } else { Value::Null },
                "head": { "sha": head },
            })))
            .mount(&self.w.server)
            .await;
        if merged {
            Mock::given(method("GET"))
                .and(path(
                    format!("/repos/acme/app/issues/{number}/events").as_str(),
                ))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!([{ "event": "merged", "commit_id": MERGED }])),
                )
                .mount(&self.w.server)
                .await;
        }
        let reply = match answer {
            Enqueue::Queued => Some(json!({
                "data": { "enqueuePullRequest": { "mergeQueueEntry": { "id": "E_1" } } }
            })),
            Enqueue::Refused => Some(json!({
                "data": { "enqueuePullRequest": null },
                "errors": [{ "message": "Pull request is not eligible for the merge queue" }],
            })),
            Enqueue::HeadChanged | Enqueue::Merged => None,
        };
        if let Some(reply) = reply {
            Mock::given(method("POST"))
                .and(path("/graphql"))
                .and(body_string_contains("enqueuePullRequest"))
                .and(body_partial_json(json!({
                    "variables": { "id": format!("PR_{number}"), "oid": HEAD }
                })))
                .respond_with(ResponseTemplate::new(200).set_body_json(reply))
                .expect(1)
                .mount(&self.w.server)
                .await;
        }
    }

    async fn queue_member(&self, number: u64, member: bool) {
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("isInMergeQueue"))
            .and(body_partial_json(
                json!({ "variables": { "number": number } }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": { "repository": { "pullRequest": { "isInMergeQueue": member } } }
            })))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn open_changes(&self, changes: &[Listed]) {
        let body: Vec<Value> = changes
            .iter()
            .map(|c| {
                let labels: Vec<Value> = c.labels.iter().map(|l| json!({ "name": l })).collect();
                json!({
                    "number": c.number, "title": c.title, "body": "details",
                    "html_url": format!("https://github.example/acme/app/pull/{}", c.number),
                    "state": "open", "draft": c.draft, "labels": labels,
                    "updated_at": support::rfc3339(c.updated),
                    "user": { "login": c.author },
                    "head": { "ref": c.branch, "sha": HEAD,
                              "repo": { "full_name": if c.fork { "mallory/app" } else { "acme/app" } } },
                    "base": { "ref": "main", "repo": { "full_name": "acme/app" } },
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/repos/acme/app/pulls"))
            .and(query_param("state", "open"))
            .and(query_param("sort", "updated"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.w.server)
            .await;
    }

    async fn change_files(&self, number: u64, files: &[&str]) {
        let body: Vec<Value> = files.iter().map(|f| json!({ "filename": f })).collect();
        Mock::given(method("GET"))
            .and(path(
                format!("/repos/acme/app/pulls/{number}/files").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.w.server)
            .await;
    }

    async fn signals(&self, number: u64, comments: &[Commented], labels: &[Labeled]) {
        let comments: Vec<Value> = comments
            .iter()
            .map(|c| {
                json!({ "id": c.id, "body": c.body, "created_at": support::rfc3339(c.at),
                             "user": { "login": c.actor } })
            })
            .collect();
        let mut events: Vec<Value> = labels
            .iter()
            .map(|l| {
                json!({ "id": l.id, "event": "labeled", "created_at": support::rfc3339(l.at),
                             "actor": { "login": l.actor }, "label": { "name": l.label } })
            })
            .collect();
        events.push(
            json!({ "id": 99, "event": "unlabeled", "created_at": support::rfc3339(NOW_MS),
                            "actor": { "login": "zed" }, "label": { "name": "saka:review" } }),
        );
        events.push(
            json!({ "id": 98, "event": "assigned", "created_at": support::rfc3339(NOW_MS),
                            "actor": { "login": "zed" } }),
        );
        Mock::given(method("GET"))
            .and(path(
                format!("/repos/acme/app/issues/{number}/comments").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(comments))
            .mount(&self.w.server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                format!("/repos/acme/app/issues/{number}/events").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(events))
            .mount(&self.w.server)
            .await;
    }

    async fn write_access(&self, login: &str, access: Access) {
        let answer = match access {
            Access::Write => ResponseTemplate::new(200)
                .set_body_json(json!({ "permission": "write", "role_name": "maintain" })),
            Access::ReadOnly => ResponseTemplate::new(200)
                .set_body_json(json!({ "permission": "read", "role_name": "triage" })),
            Access::Stranger => {
                ResponseTemplate::new(404).set_body_json(json!({ "message": "Not Found" }))
            }
        };
        Mock::given(method("GET"))
            .and(path(
                format!("/repos/acme/app/collaborators/{login}/permission").as_str(),
            ))
            .respond_with(answer)
            .mount(&self.w.server)
            .await;
    }

    async fn review_accepted(&self, number: u64, event: ReviewEvent, body: &str) {
        let wire = match event {
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
            ReviewEvent::Comment => "COMMENT",
        };
        Mock::given(method("POST"))
            .and(path(format!("/repos/acme/app/pulls/{number}/reviews").as_str()))
            .and(body_json(
                json!({ "commit_id": HEAD, "body": body, "event": wire }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "html_url": format!("https://github.example/acme/app/pull/{number}#pullrequestreview-1"),
            })))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn review_refused(&self, number: u64, status: u16) {
        Mock::given(method("POST"))
            .and(path(
                format!("/repos/acme/app/pulls/{number}/reviews").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(status).set_body_json(json!({ "message": "refused" })),
            )
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn comment_accepted(&self, number: u64, body: &str) {
        Mock::given(method("POST"))
            .and(path(
                format!("/repos/acme/app/issues/{number}/comments").as_str(),
            ))
            .and(body_json(json!({ "body": body })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn update_answers(&self, number: u64, answer: UpdateAnswer) {
        let (status, message) = match answer {
            UpdateAnswer::Accepted => (202, "Updating pull request branch."),
            UpdateAnswer::Conflict => (422, "merge conflict between base and head"),
            UpdateAnswer::HeadChanged => (422, "expected head sha didn't match current head ref."),
        };
        Mock::given(method("PUT"))
            .and(path(
                format!("/repos/acme/app/pulls/{number}/update-branch").as_str(),
            ))
            .and(body_json(json!({ "expected_head_sha": HEAD })))
            .respond_with(
                ResponseTemplate::new(status).set_body_json(json!({ "message": message })),
            )
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

    async fn diagnosis_answers(&self) {
        Mock::given(method("GET"))
            .and(path("/app"))
            .and(self.w.jwt())
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    json!({ "id": 9, "slug": "saka-dev", "events": ["pull_request"] }),
                ),
            )
            .mount(&self.w.server)
            .await;
    }
}

struct GitLabHarness {
    w: GitLabWorld,
}

impl GitLabHarness {
    async fn new() -> GitLabHarness {
        let mut w = GitLabWorld::new().await;
        w.gl = saka_scm::GitLab::new(w.client.clone(), std::sync::Arc::new(support::NoRepos))
            .with_webhook_secret(SECRET.to_vec());
        GitLabHarness { w }
    }

    fn state_of(&self, state: Change) -> &'static str {
        match state {
            Change::Open => "opened",
            Change::Merged => "merged",
            Change::Closed => "closed",
        }
    }
}

fn job_status(done: Option<&str>) -> &'static str {
    match done {
        Some("success") => "success",
        Some(_) => "failed",
        None => "running",
    }
}

#[async_trait]
impl Harness for GitLabHarness {
    fn scm(&self) -> &dyn Scm {
        &self.w.gl
    }

    fn repo(&self) -> RepoRef {
        self.w.repo.clone()
    }

    fn integration(&self) -> &str {
        "gitlab"
    }

    fn declared(&self) -> ScmCapabilities {
        ScmCapabilities {
            provider: "gitlab",
            merge_methods: &[MergeMethod::Merge, MergeMethod::Squash],
            webhook_signature: Some(saka_ports::WebhookSignature::SharedToken),
            check_kinds: &[CheckKind::CheckRun, CheckKind::CommitStatus],
            draft_change_requests: true,
            delete_branch_after_merge: true,
            commit_status: true,
            merge_queue: true,
            update_branch: false,
            user_authorship: false,
            change_head_ref: "refs/merge-requests/{number}/head",
            diagnostics: Some(saka_scm::gitlab::GITLAB_DIAGNOSTICS),
        }
    }

    async fn repository(&self, default_branch: &str) {
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fapp"))
            .and(header("private-token", TOKEN))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "path_with_namespace": "acme/app", "web_url": "https://gitlab.example/acme/app",
                "default_branch": default_branch,
            })))
            .mount(&self.w.server)
            .await;
    }

    async fn missing_repository(&self, full_name: &str) {
        let encoded = full_name.replace('/', "%2F");
        Mock::given(method("GET"))
            .and(path(format!("/projects/{encoded}").as_str()))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(json!({ "message": "404 Project Not Found" })),
            )
            .mount(&self.w.server)
            .await;
    }

    async fn change_already_open(&self, number: u64) {
        Mock::given(method("POST"))
            .and(path("/projects/acme%2Fapp/merge_requests"))
            .and(body_partial_json(
                json!({ "source_branch": BRANCH, "target_branch": "main" }),
            ))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({
                "message": [format!("Another open merge request already exists for this source branch: !{number}")],
            })))
            .mount(&self.w.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fapp/merge_requests"))
            .and(query_param("source_branch", BRANCH))
            .and(query_param("state", "opened"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([gitlab::mr(number, "opened")])),
            )
            .mount(&self.w.server)
            .await;
    }

    async fn change(&self, number: u64, state: Change) {
        let body = gitlab::mr(number, self.state_of(state));
        Mock::given(method("GET"))
            .and(path(
                format!("/projects/acme%2Fapp/merge_requests/{number}").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.w.server)
            .await;
    }

    async fn checks_at(&self, sha: &str, checks: &[Check], pending_status: &str) {
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fapp/pipelines"))
            .and(query_param("sha", sha))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "id": 100 }])))
            .mount(&self.w.server)
            .await;
        let jobs: Vec<Value> = checks
            .iter()
            .map(|c| json!({ "id": c.id, "name": c.name, "status": job_status(c.done) }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fapp/pipelines/100/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jobs))
            .mount(&self.w.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/projects/acme%2Fapp/repository/commits/{sha}/statuses").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": 900, "name": pending_status, "status": "running", "target_url": "https://ci.example/1" },
            ])))
            .mount(&self.w.server)
            .await;
    }

    async fn failing_checks(&self, rerunnable: &[&str], foreign: &[&str]) {
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fapp/pipelines"))
            .and(query_param("sha", HEAD))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "id": 100 }])))
            .mount(&self.w.server)
            .await;
        let jobs: Vec<Value> = rerunnable
            .iter()
            .enumerate()
            .map(|(i, name)| json!({ "id": 40 + i, "name": name, "status": "failed" }))
            .collect();
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fapp/pipelines/100/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(jobs))
            .mount(&self.w.server)
            .await;
        let statuses: Vec<Value> = foreign
            .iter()
            .enumerate()
            .map(|(i, name)| json!({ "id": 900 + i, "name": name, "status": "failed" }))
            .collect();
        Mock::given(method("GET"))
            .and(path(
                format!("/projects/acme%2Fapp/repository/commits/{HEAD}/statuses").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(statuses))
            .mount(&self.w.server)
            .await;
    }

    async fn rerun_answers(&self, answer: RerunAnswer) {
        let (status, times) = match answer {
            RerunAnswer::Accepted => (201, 1),
            RerunAnswer::Refused(status) => (status, 1),
            RerunAnswer::NotCalled => (201, 0),
        };
        Mock::given(method("POST"))
            .and(path("/projects/acme%2Fapp/pipelines/100/retry"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({ "id": 100 })))
            .expect(times)
            .mount(&self.w.server)
            .await;
    }

    async fn merge_answers(&self, number: u64, answer: MergeAnswer) {
        let title = format!("Add the first step (#{number})");
        let (status, body) = match answer {
            MergeAnswer::Merged => (200, gitlab::mr(number, "merged")),
            MergeAnswer::HeadChanged => (
                409,
                json!({ "message": "SHA does not match HEAD of source branch" }),
            ),
            MergeAnswer::NotMergeable => {
                self.change(number, Change::Open).await;
                (405, json!({ "message": "405 Method Not Allowed" }))
            }
        };
        Mock::given(method("PUT"))
            .and(path(
                format!("/projects/acme%2Fapp/merge_requests/{number}/merge").as_str(),
            ))
            .and(body_json(json!({
                "sha": HEAD, "squash": true, "should_remove_source_branch": false,
                "merge_commit_message": title, "squash_commit_message": title,
            })))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn queue_state(&self, enabled: bool) {
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fapp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "path_with_namespace": "acme/app", "default_branch": "main",
                "merge_pipelines_enabled": enabled, "merge_trains_enabled": enabled,
            })))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn enqueue_answers(&self, number: u64, answer: Enqueue) {
        let (status, body) = match answer {
            Enqueue::Queued => {
                let mut mr = gitlab::mr(number, "opened");
                mr["merge_when_pipeline_succeeds"] = json!(true);
                (200, mr)
            }
            Enqueue::HeadChanged => (
                409,
                json!({ "message": "SHA does not match HEAD of source branch" }),
            ),
            Enqueue::Merged => (200, gitlab::mr(number, "merged")),
            Enqueue::Refused => (422, json!({ "message": "Branch cannot be merged" })),
        };
        Mock::given(method("PUT"))
            .and(path(
                format!("/projects/acme%2Fapp/merge_requests/{number}/merge").as_str(),
            ))
            .and(body_json(json!({
                "sha": HEAD, "auto_merge": true, "merge_when_pipeline_succeeds": true,
                "should_remove_source_branch": false,
            })))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn queue_member(&self, number: u64, member: bool) {
        let mut mr = gitlab::mr(number, "opened");
        mr["merge_when_pipeline_succeeds"] = json!(member);
        Mock::given(method("GET"))
            .and(path(
                format!("/projects/acme%2Fapp/merge_requests/{number}").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(mr))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn open_changes(&self, changes: &[Listed]) {
        let body: Vec<Value> = changes
            .iter()
            .map(|c| {
                json!({
                    "iid": c.number, "title": c.title, "description": "details",
                    "web_url": format!("https://gitlab.example/acme/app/-/merge_requests/{}", c.number),
                    "state": "opened", "draft": c.draft, "labels": c.labels,
                    "updated_at": support::rfc3339(c.updated),
                    "author": { "username": c.author },
                    "source_branch": c.branch, "target_branch": "main", "sha": HEAD,
                    "source_project_id": if c.fork { 10 } else { 9 }, "target_project_id": 9,
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fapp/merge_requests"))
            .and(query_param("state", "opened"))
            .and(query_param("order_by", "updated_at"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.w.server)
            .await;
    }

    async fn change_files(&self, number: u64, files: &[&str]) {
        let body: Vec<Value> = files
            .iter()
            .map(|f| json!({ "new_path": f, "old_path": f }))
            .collect();
        Mock::given(method("GET"))
            .and(path(
                format!("/projects/acme%2Fapp/merge_requests/{number}/diffs").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.w.server)
            .await;
    }

    async fn signals(&self, number: u64, comments: &[Commented], labels: &[Labeled]) {
        let mut notes: Vec<Value> = comments
            .iter()
            .map(|c| json!({ "id": c.id, "body": c.body, "system": false,
                             "created_at": support::rfc3339(c.at), "author": { "username": c.actor } }))
            .collect();
        notes.push(json!({ "id": 98, "body": "assigned to zed", "system": true,
                           "created_at": support::rfc3339(NOW_MS), "author": { "username": "zed" } }));
        let mut events: Vec<Value> = labels
            .iter()
            .map(|l| {
                json!({ "id": l.id, "action": "add", "created_at": support::rfc3339(l.at),
                             "user": { "username": l.actor }, "label": { "name": l.label } })
            })
            .collect();
        events.push(
            json!({ "id": 99, "action": "remove", "created_at": support::rfc3339(NOW_MS),
                            "user": { "username": "zed" }, "label": { "name": "saka:review" } }),
        );
        Mock::given(method("GET"))
            .and(path(
                format!("/projects/acme%2Fapp/merge_requests/{number}/notes").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(notes))
            .mount(&self.w.server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                format!("/projects/acme%2Fapp/merge_requests/{number}/resource_label_events")
                    .as_str(),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(events))
            .mount(&self.w.server)
            .await;
    }

    async fn write_access(&self, login: &str, access: Access) {
        let id = if matches!(access, Access::Write) {
            5
        } else {
            6
        };
        let users = match access {
            Access::Stranger => json!([]),
            _ => json!([{ "id": id, "username": login }]),
        };
        Mock::given(method("GET"))
            .and(path("/users"))
            .and(query_param("username", login))
            .respond_with(ResponseTemplate::new(200).set_body_json(users))
            .mount(&self.w.server)
            .await;
        let level = match access {
            Access::Write => 30,
            _ => 20,
        };
        Mock::given(method("GET"))
            .and(path(
                format!("/projects/acme%2Fapp/members/all/{id}").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "id": id, "access_level": level })),
            )
            .mount(&self.w.server)
            .await;
    }

    async fn review_accepted(&self, number: u64, event: ReviewEvent, body: &str) {
        let base = format!("/projects/acme%2Fapp/merge_requests/{number}");
        Mock::given(method("POST"))
            .and(path(format!("{base}/notes").as_str()))
            .and(body_json(json!({ "body": body })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 9 })))
            .expect(1)
            .mount(&self.w.server)
            .await;
        match event {
            ReviewEvent::Approve => {
                Mock::given(method("POST"))
                    .and(path(format!("{base}/approve").as_str()))
                    .and(body_json(json!({ "sha": HEAD })))
                    .respond_with(ResponseTemplate::new(201).set_body_json(json!({})))
                    .expect(1)
                    .mount(&self.w.server)
                    .await;
            }
            ReviewEvent::RequestChanges => {
                Mock::given(method("POST"))
                    .and(path(format!("{base}/unapprove").as_str()))
                    .respond_with(ResponseTemplate::new(404).set_body_json(json!({})))
                    .expect(1)
                    .mount(&self.w.server)
                    .await;
            }
            ReviewEvent::Comment => {}
        }
    }

    async fn review_refused(&self, number: u64, status: u16) {
        Mock::given(method("POST"))
            .and(path(
                format!("/projects/acme%2Fapp/merge_requests/{number}/notes").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(status).set_body_json(json!({ "message": "refused" })),
            )
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn comment_accepted(&self, number: u64, body: &str) {
        Mock::given(method("POST"))
            .and(path(
                format!("/projects/acme%2Fapp/merge_requests/{number}/notes").as_str(),
            ))
            .and(body_json(json!({ "body": body })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 9 })))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn update_answers(&self, _number: u64, _answer: UpdateAnswer) {}

    async fn status_accepted(&self, state: &str, description: &str) {
        let state = if state == "failure" { "failed" } else { state };
        Mock::given(method("POST"))
            .and(path(
                format!("/projects/acme%2Fapp/statuses/{HEAD}").as_str(),
            ))
            .and(body_json(json!({
                "state": state, "name": "saka/verify", "description": description,
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn branch_missing(&self) {
        Mock::given(method("DELETE"))
            .and(path(
                "/projects/acme%2Fapp/repository/branches/saka%2Ftwo-step%2Fs1",
            ))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(json!({ "message": "404 Branch Not Found" })),
            )
            .mount(&self.w.server)
            .await;
    }

    fn ci_webhook(&self, numbers: &[u64], signing: Signing) -> (Vec<(String, String)>, Vec<u8>) {
        let body = serde_json::to_vec(&json!({
            "object_kind": "pipeline",
            "object_attributes": { "sha": HEAD, "status": "success" },
            "merge_request": numbers.first().map(|iid| json!({ "iid": iid })),
            "project": { "path_with_namespace": "acme/app" },
        }))
        .unwrap();
        let mut headers = vec![
            ("X-Gitlab-Event".to_string(), "Pipeline Hook".to_string()),
            ("Idempotency-Key".to_string(), "9f1c-delivery".to_string()),
        ];
        let token = match signing {
            Signing::Valid | Signing::UpperHex => Some(String::from_utf8(SECRET.to_vec()).unwrap()),
            Signing::Wrong => Some("another-secret-0123456789abcdef0123".to_string()),
            Signing::Missing => None,
        };
        if let Some(t) = token {
            headers.push(("X-Gitlab-Token".to_string(), t));
        }
        (headers, body)
    }

    async fn diagnosis_answers(&self) {
        let mount = |at: &'static str, body: Value| async move {
            Mock::given(method("GET"))
                .and(path(at))
                .and(header("private-token", TOKEN))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&self.w.server)
                .await;
        };
        mount(
            "/personal_access_tokens/self",
            json!({
                "name": "saka", "active": true, "revoked": false,
                "scopes": ["api"], "expires_at": null,
            }),
        )
        .await;
        mount(
            "/projects/acme%2Fapp",
            json!({ "permissions": { "project_access": { "access_level": 40 }, "group_access": null } }),
        )
        .await;
        mount("/projects/acme%2Fapp/hooks", json!([])).await;
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
            credential: ChangeCredential::App,
        })
        .await
        .unwrap();
    let ChangeOpening::Opened(opened) = opened else {
        panic!("not opened: {opened:?}");
    };
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

async fn checks_are_read_for_a_commit(h: &dyn Harness) {
    h.checks_at(
        MERGED,
        &[
            Check {
                id: 30,
                name: "test",
                done: Some("failure"),
            },
            Check {
                id: 31,
                name: "test",
                done: Some("success"),
            },
            Check {
                id: 32,
                name: "lint",
                done: Some("success"),
            },
        ],
        saka_core::gate::SAKA_STATUS_CONTEXT,
    )
    .await;
    let checks = h.scm().commit_checks(&h.repo(), MERGED).await.unwrap();
    let summary: Vec<(&str, CheckKind, Option<CheckConclusion>)> = checks
        .iter()
        .map(|c| (c.name.as_str(), c.kind, c.conclusion))
        .collect();
    assert_eq!(
        summary,
        [
            ("lint", CheckKind::CheckRun, Some(CheckConclusion::Success)),
            ("test", CheckKind::CheckRun, Some(CheckConclusion::Success)),
            (
                saka_core::gate::SAKA_STATUS_CONTEXT,
                CheckKind::CommitStatus,
                None
            ),
        ]
    );
}

async fn failed_checks_are_rerun_and_the_ones_that_cannot_be_are_named(h: &dyn Harness) {
    h.failing_checks(&["unit", "lint"], &["codecov"]).await;
    h.rerun_answers(RerunAnswer::Accepted).await;
    let outcome = h.scm().rerun_failed_checks(&h.repo(), HEAD).await.unwrap();
    assert_eq!(outcome.rerun, ["lint", "unit"]);
    assert_eq!(outcome.unsupported, ["codecov"]);
}

async fn a_head_with_nothing_the_provider_can_rerun_makes_no_request(h: &dyn Harness) {
    h.failing_checks(&[], &["codecov"]).await;
    h.rerun_answers(RerunAnswer::NotCalled).await;
    let outcome = h.scm().rerun_failed_checks(&h.repo(), HEAD).await.unwrap();
    assert!(outcome.rerun.is_empty());
    assert_eq!(outcome.unsupported, ["codecov"]);
}

async fn a_refused_rerun_is_a_permanent_coded_error(h: &dyn Harness) {
    h.failing_checks(&["unit"], &[]).await;
    h.rerun_answers(RerunAnswer::Refused(403)).await;
    let err = h
        .scm()
        .rerun_failed_checks(&h.repo(), HEAD)
        .await
        .unwrap_err();
    assert_eq!(err.code, "scm_rerun_refused");
    assert!(!err.retryable);
    assert_eq!(err.params["status"], 403);
}

async fn a_rerun_names_a_full_head_sha(h: &dyn Harness) {
    let err = h
        .scm()
        .rerun_failed_checks(&h.repo(), "abc")
        .await
        .unwrap_err();
    assert_eq!(err.code, "git_argument_invalid");
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
    if h.declared().webhook_signature != Some(saka_ports::WebhookSignature::HmacSha256) {
        return;
    }
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

async fn a_review_is_posted_at_the_head(h: &dyn Harness) {
    for (number, event) in [
        (7, ReviewEvent::Approve),
        (8, ReviewEvent::RequestChanges),
        (9, ReviewEvent::Comment),
    ] {
        let body = format!("Saka review @{number}");
        h.review_accepted(number, event, &body).await;
        let post = ReviewPost {
            repo: h.repo(),
            number,
            head_sha: HEAD.into(),
            event,
            body,
        };
        h.scm().post_review(&post).await.unwrap();
    }
    let bad = ReviewPost {
        repo: h.repo(),
        number: 7,
        head_sha: "HEAD".into(),
        event: ReviewEvent::Comment,
        body: "x".into(),
    };
    let refused = h.scm().post_review(&bad).await.unwrap_err();
    assert_eq!(refused.code, "git_argument_invalid");
}

async fn a_comment_is_posted_on_the_change(h: &dyn Harness) {
    h.comment_accepted(7, "Saka needs the branch updated").await;
    h.scm()
        .post_comment(&h.repo(), 7, "Saka needs the branch updated")
        .await
        .unwrap();
}

async fn a_branch_update_follows_the_declared_capability(h: &dyn Harness) {
    if !h.declared().update_branch {
        let err = h.scm().update_branch(&h.repo(), 7, HEAD).await.unwrap_err();
        assert_eq!(err.code, "scm_capability_unsupported");
        assert_eq!(err.params["capability"], "update_branch");
        return;
    }
    for (number, answer, outcome) in [
        (7, UpdateAnswer::Accepted, UpdateOutcome::Accepted),
        (8, UpdateAnswer::Conflict, UpdateOutcome::Conflict),
        (9, UpdateAnswer::HeadChanged, UpdateOutcome::HeadChanged),
    ] {
        h.update_answers(number, answer).await;
        let got = h
            .scm()
            .update_branch(&h.repo(), number, HEAD)
            .await
            .unwrap();
        assert_eq!(got, outcome);
    }
}

async fn a_refused_review_is_permanent(h: &dyn Harness) {
    for (number, status) in [(10, 404), (11, 403), (12, 422)] {
        if status == 422 && h.declared().provider == "gitlab" {
            continue;
        }
        h.review_refused(number, status).await;
        let post = ReviewPost {
            repo: h.repo(),
            number,
            head_sha: HEAD.into(),
            event: ReviewEvent::Approve,
            body: "no".into(),
        };
        let err = h.scm().post_review(&post).await.unwrap_err();
        assert_eq!(err.code, "review_refused", "status {status}");
        assert!(!err.retryable);
        assert_eq!(err.params["status"], status);
    }
}

const T1: i64 = support::NOW_MS - 3_600_000;
const T2: i64 = support::NOW_MS - 1_800_000;
const T3: i64 = support::NOW_MS - 600_000;

async fn changes_are_listed_and_summarized(h: &dyn Harness) {
    h.open_changes(&[
        Listed {
            number: 7,
            title: "Add retries",
            author: "alice",
            labels: &["needs-review", "docs"],
            draft: false,
            fork: false,
            branch: "feature/retries",
            updated: T3,
        },
        Listed {
            number: 8,
            title: "Fix typo",
            author: "mallory",
            labels: &[],
            draft: true,
            fork: true,
            branch: "patch-1",
            updated: T2,
        },
    ])
    .await;
    let listed = h.scm().list_open_changes(&h.repo(), 200).await.unwrap();
    let seen: Vec<_> = listed
        .iter()
        .map(|c| (c.number, c.draft, c.from_fork, c.author.as_str()))
        .collect();
    assert_eq!(
        seen,
        [(7, false, false, "alice"), (8, true, true, "mallory")]
    );
    let first = &listed[0];
    assert_eq!(first.title, "Add retries");
    assert_eq!(first.body, "details");
    assert_eq!(first.labels, ["needs-review", "docs"]);
    assert_eq!(
        (first.head_branch.as_str(), first.base_branch.as_str()),
        ("feature/retries", "main")
    );
    assert_eq!(
        (first.head_sha.as_str(), first.state),
        (HEAD, ChangeState::Open)
    );
    assert_eq!(first.updated_at, saka_core::time::Timestamp(T3));
    assert!(first.url.ends_with("7"), "{}", first.url);

    let capped = h.scm().list_open_changes(&h.repo(), 1).await.unwrap();
    assert_eq!(capped.len(), 1, "the cap is honored");
}

async fn a_change_is_summarized_in_every_state(h: &dyn Harness) {
    for (number, state, expected) in [
        (7, Change::Open, ChangeState::Open),
        (8, Change::Merged, ChangeState::Merged),
        (9, Change::Closed, ChangeState::Closed),
    ] {
        h.change(number, state).await;
        let summary = h.scm().change_summary(&h.repo(), number).await.unwrap();
        assert_eq!(
            (summary.number, summary.head_sha.as_str(), summary.state),
            (number, HEAD, expected)
        );
    }
    h.missing_repository("acme/gone").await;
    let missing = h
        .scm()
        .change_summary(
            &RepoRef {
                full_name: "acme/gone".into(),
                ..h.repo()
            },
            1,
        )
        .await
        .unwrap_err();
    assert!(!missing.retryable);
}

async fn a_change_lists_the_files_it_touches(h: &dyn Harness) {
    h.change_files(7, &["services/api/lib.rs", "README.md"])
        .await;
    let files = h.scm().change_files(&h.repo(), 7, 3000).await.unwrap();
    assert_eq!(files, ["services/api/lib.rs", "README.md"]);
    let capped = h.scm().change_files(&h.repo(), 7, 1).await.unwrap();
    assert_eq!(capped, ["services/api/lib.rs"]);
}

async fn signals_carry_their_actor(h: &dyn Harness) {
    h.signals(
        7,
        &[
            Commented {
                id: 1,
                actor: "bob",
                body: "/saka review",
                at: T1,
            },
            Commented {
                id: 2,
                actor: "carol",
                body: "looks good",
                at: T3,
            },
        ],
        &[Labeled {
            id: 3,
            actor: "dave",
            label: "saka:review",
            at: T2,
        }],
    )
    .await;
    let all = h
        .scm()
        .change_signals(&h.repo(), 7, saka_core::time::Timestamp(T1))
        .await
        .unwrap();
    let seen: Vec<(&str, &str, i64)> = all
        .iter()
        .map(|s| (s.id.as_str(), s.actor.as_str(), s.at.0))
        .collect();
    assert_eq!(
        seen,
        [
            ("comment:1", "bob", T1),
            ("label:3", "dave", T2),
            ("comment:2", "carol", T3)
        ],
        "oldest first; system notes and label removals are no signals"
    );
    assert_eq!(
        all[0].kind,
        SignalKind::Comment {
            body: "/saka review".into()
        }
    );
    assert_eq!(
        all[1].kind,
        SignalKind::Label {
            name: "saka:review".into()
        }
    );
    let later = h
        .scm()
        .change_signals(&h.repo(), 7, saka_core::time::Timestamp(T2))
        .await
        .unwrap();
    assert_eq!(
        later.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["label:3", "comment:2"]
    );
}

async fn write_access_is_checked(h: &dyn Harness) {
    h.write_access("maintainer", Access::Write).await;
    h.write_access("reporter", Access::ReadOnly).await;
    h.write_access("stranger", Access::Stranger).await;
    let can =
        |login: &'static str| async move { h.scm().can_write(&h.repo(), login).await.unwrap() };
    assert!(can("maintainer").await);
    assert!(!can("reporter").await);
    assert!(!can("stranger").await);
}

async fn capabilities_match_the_declared_matrix(h: &dyn Harness) {
    assert_eq!(h.scm().capabilities(h.integration()), Some(h.declared()));
}

async fn diagnosis_returns_the_status_the_declared_capability_names(h: &dyn Harness) {
    h.diagnosis_answers().await;
    let scm = h.scm();
    match h.declared().diagnostics {
        Some(ScmDiagnostics::App(_)) => {
            assert!(matches!(
                scm.credential_status(h.integration()).await.unwrap(),
                CredentialStatus::App(_)
            ));
            assert!(matches!(
                scm.repo_grant(&h.repo()).await.unwrap(),
                RepoGrant::Installation(_)
            ));
            assert_eq!(
                scm.repo_webhooks(&h.repo()).await.unwrap_err().code,
                "scm_capability_unsupported"
            );
        }
        Some(ScmDiagnostics::Token(_)) => {
            assert!(matches!(
                scm.credential_status(h.integration()).await.unwrap(),
                CredentialStatus::Token(_)
            ));
            assert!(matches!(
                scm.repo_grant(&h.repo()).await.unwrap(),
                RepoGrant::Member { .. }
            ));
            assert!(matches!(
                scm.repo_webhooks(&h.repo()).await.unwrap(),
                RepoHooks::Listed(_)
            ));
        }
        None => {
            let credential = scm.credential_status(h.integration()).await.unwrap_err();
            assert_eq!(credential.code, "scm_capability_unsupported");
            let grant = scm.repo_grant(&h.repo()).await.unwrap_err();
            assert_eq!(grant.code, "scm_capability_unsupported");
        }
    }
}

async fn the_merge_queue_is_declared_and_found_only_where_the_branch_has_one(h: &dyn Harness) {
    assert!(h.declared().merge_queue);
    h.queue_state(true).await;
    assert!(h.scm().merge_queue(&h.repo(), "main").await.unwrap());
}

async fn a_branch_without_a_merge_queue_reports_none(h: &dyn Harness) {
    h.queue_state(false).await;
    assert!(!h.scm().merge_queue(&h.repo(), "main").await.unwrap());
}

async fn enqueue_names_the_head_and_reports_its_outcomes(h: &dyn Harness) {
    h.enqueue_answers(1, Enqueue::Queued).await;
    assert_eq!(
        h.scm().enqueue(&merge_request(h, 1)).await.unwrap(),
        EnqueueOutcome::Queued
    );
    h.enqueue_answers(2, Enqueue::HeadChanged).await;
    assert_eq!(
        h.scm().enqueue(&merge_request(h, 2)).await.unwrap(),
        EnqueueOutcome::Rejected {
            reason: "head_changed".into()
        }
    );
    h.enqueue_answers(3, Enqueue::Merged).await;
    assert!(matches!(
        h.scm().enqueue(&merge_request(h, 3)).await.unwrap(),
        EnqueueOutcome::Merged { merge_commit_sha, .. } if merge_commit_sha == MERGED
    ));
    h.enqueue_answers(4, Enqueue::Refused).await;
    assert!(matches!(
        h.scm().enqueue(&merge_request(h, 4)).await.unwrap(),
        EnqueueOutcome::Rejected { .. }
    ));
}

async fn queue_membership_follows_the_provider(h: &dyn Harness) {
    h.queue_member(1, true).await;
    assert!(h.scm().in_merge_queue(&h.repo(), 1).await.unwrap());
    h.queue_member(2, false).await;
    assert!(!h.scm().in_merge_queue(&h.repo(), 2).await.unwrap());
}

macro_rules! conformance {
    ($module:ident, $harness:ident: $($case:ident),* $(,)?) => {
        mod $module {
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

conformance!(github_conformance, GitHubHarness:
    repository_info_reads_the_default_branch,
    open_change_request_is_idempotent_for_the_same_branch,
    change_request_snapshot_normalizes_checks_and_statuses,
    latest_rerun_of_a_check_wins,
    checks_are_read_for_a_commit,
    failed_checks_are_rerun_and_the_ones_that_cannot_be_are_named,
    a_head_with_nothing_the_provider_can_rerun_makes_no_request,
    a_refused_rerun_is_a_permanent_coded_error,
    a_rerun_names_a_full_head_sha,
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
    changes_are_listed_and_summarized,
    a_change_is_summarized_in_every_state,
    a_change_lists_the_files_it_touches,
    signals_carry_their_actor,
    write_access_is_checked,
    a_review_is_posted_at_the_head,
    a_refused_review_is_permanent,
    a_comment_is_posted_on_the_change,
    a_branch_update_follows_the_declared_capability,
    capabilities_match_the_declared_matrix,
    diagnosis_returns_the_status_the_declared_capability_names,
    the_merge_queue_is_declared_and_found_only_where_the_branch_has_one,
    a_branch_without_a_merge_queue_reports_none,
    enqueue_names_the_head_and_reports_its_outcomes,
    queue_membership_follows_the_provider,
);

conformance!(gitlab_conformance, GitLabHarness:
    repository_info_reads_the_default_branch,
    open_change_request_is_idempotent_for_the_same_branch,
    change_request_snapshot_normalizes_checks_and_statuses,
    latest_rerun_of_a_check_wins,
    checks_are_read_for_a_commit,
    failed_checks_are_rerun_and_the_ones_that_cannot_be_are_named,
    a_head_with_nothing_the_provider_can_rerun_makes_no_request,
    a_refused_rerun_is_a_permanent_coded_error,
    a_rerun_names_a_full_head_sha,
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
    changes_are_listed_and_summarized,
    a_change_is_summarized_in_every_state,
    a_change_lists_the_files_it_touches,
    signals_carry_their_actor,
    write_access_is_checked,
    a_review_is_posted_at_the_head,
    a_refused_review_is_permanent,
    a_comment_is_posted_on_the_change,
    a_branch_update_follows_the_declared_capability,
    capabilities_match_the_declared_matrix,
    diagnosis_returns_the_status_the_declared_capability_names,
    the_merge_queue_is_declared_and_found_only_where_the_branch_has_one,
    a_branch_without_a_merge_queue_reports_none,
    enqueue_names_the_head_and_reports_its_outcomes,
    queue_membership_follows_the_provider,
);
