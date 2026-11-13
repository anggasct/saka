#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

mod support;

use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use saka_core::time::Timestamp;
use saka_ports::{
    IntakeCapabilities, IntakeSource, IssueRef, IssueSignalKind, IssueState, MAX_ISSUE_BODY_BYTES,
    MAX_ISSUES, RepoRef, Scm, TRUNCATION_MARKER, WebhookTarget,
};
use serde_json::{Value, json};
use sha2::Sha256;
use support::gitlab::GitLabWorld;
use support::{NOW_MS, World, rfc3339};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

const SECRET: &[u8] = b"conformance-secret-0123456789abcdef";
const SINCE: i64 = NOW_MS - 7_200_000;
const T1: i64 = NOW_MS - 3_600_000;
const T2: i64 = NOW_MS - 1_800_000;
const T3: i64 = NOW_MS - 600_000;
const RETRY_AFTER_SECS: i64 = 30;

struct Listed {
    number: u64,
    title: String,
    author: &'static str,
    labels: &'static [&'static str],
    closed: bool,
    confidential: bool,
    updated: i64,
}

impl Listed {
    fn new(number: u64, updated: i64) -> Listed {
        Listed {
            number,
            title: format!("Issue {number}"),
            author: "alice",
            labels: &["bug"],
            closed: false,
            confidential: false,
            updated,
        }
    }
}

enum Activity {
    Comment {
        id: u64,
        actor: &'static str,
        body: &'static str,
        at: i64,
    },
    Label {
        id: u64,
        actor: &'static str,
        label: &'static str,
        at: i64,
    },
    Closed {
        id: u64,
        actor: &'static str,
        at: i64,
    },
    Reopened {
        id: u64,
        actor: &'static str,
        at: i64,
    },
}

enum Access {
    Write,
    ReadOnly,
    Stranger,
}

enum Hook {
    IssueOpened,
    CommentOnIssue,
    CommentOnChange,
}

fn labels() -> Vec<String> {
    vec!["bug".to_string(), "saka:intake".to_string()]
}

#[async_trait]
trait IssueHarness: Sync {
    fn source(&self) -> &dyn IntakeSource;
    fn scm(&self) -> &dyn Scm;
    fn repo(&self) -> RepoRef;
    fn integration(&self) -> &str;
    fn confidential_issues(&self) -> bool;
    async fn listing(&self, issues: &[Listed]);
    async fn issue(&self, listed: &Listed, body: &str);
    async fn missing_issue(&self, number: u64);
    async fn activity(&self, number: u64, activity: &[Activity]);
    async fn write_access(&self, login: &str, access: Access);
    async fn comment_accepted(&self, number: u64, body: &str);
    async fn comment_refused(&self, number: u64, status: u16);
    async fn rate_limited(&self);
    fn webhook(&self, hook: Hook) -> (Vec<(String, String)>, Vec<u8>);
}

fn issue_ref(h: &dyn IssueHarness, number: u64) -> IssueRef {
    IssueRef {
        repo: h.repo(),
        number,
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

    fn item(listed: &Listed) -> Value {
        json!({
            "number": listed.number, "title": listed.title, "state": if listed.closed { "closed" } else { "open" },
            "html_url": format!("https://github.example/acme/app/issues/{}", listed.number),
            "created_at": rfc3339(listed.updated - 1000), "updated_at": rfc3339(listed.updated), "user": { "login": listed.author },
            "labels": listed.labels.iter().map(|l| json!({ "name": l })).collect::<Vec<_>>(),
        })
    }

    async fn serve(&self, tail: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path(format!("/repos/acme/app/{tail}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.w.server)
            .await;
    }
}

#[async_trait]
impl IssueHarness for GitHubHarness {
    fn source(&self) -> &dyn IntakeSource {
        &self.w.gh
    }

    fn scm(&self) -> &dyn Scm {
        &self.w.gh
    }

    fn repo(&self) -> RepoRef {
        self.w.repo.clone()
    }

    fn integration(&self) -> &str {
        "github"
    }

    fn confidential_issues(&self) -> bool {
        false
    }

    async fn listing(&self, issues: &[Listed]) {
        let mut items: Vec<Value> = issues.iter().map(Self::item).collect();
        items.insert(
            0,
            json!({ "number": 999, "title": "A pull request", "state": "open", "updated_at": rfc3339(SINCE),
                    "pull_request": { "url": "x" }, "user": { "login": "mallory" }, "labels": [] }),
        );
        Mock::given(method("GET"))
            .and(path("/repos/acme/app/issues"))
            .and(query_param("state", "all"))
            .and(query_param("sort", "updated"))
            .and(query_param("direction", "asc"))
            .and(query_param("since", Timestamp(SINCE).to_rfc3339()))
            .and(query_param("labels", "bug,saka:intake"))
            .respond_with(ResponseTemplate::new(200).set_body_json(items))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn issue(&self, listed: &Listed, body: &str) {
        let mut item = Self::item(listed);
        item["body"] = json!(body);
        self.serve(&format!("issues/{}", listed.number), item).await;
    }

    async fn missing_issue(&self, number: u64) {
        Mock::given(method("GET"))
            .and(path(format!("/repos/acme/app/issues/{number}").as_str()))
            .respond_with(
                ResponseTemplate::new(404).set_body_json(json!({ "message": "Not Found" })),
            )
            .mount(&self.w.server)
            .await;
    }

    async fn activity(&self, number: u64, activity: &[Activity]) {
        let mut comments = Vec::new();
        let mut events = vec![
            json!({ "id": 99, "event": "unlabeled", "created_at": rfc3339(NOW_MS),
                    "actor": { "login": "zed" }, "label": { "name": "saka:intake" } }),
            json!({ "id": 98, "event": "assigned", "created_at": rfc3339(NOW_MS), "actor": { "login": "zed" } }),
        ];
        for a in activity {
            match a {
                Activity::Comment { id, actor, body, at } => comments.push(json!({
                    "id": id, "body": body, "created_at": rfc3339(*at), "user": { "login": actor } })),
                Activity::Label { id, actor, label, at } => events.push(json!({
                    "id": id, "event": "labeled", "created_at": rfc3339(*at),
                    "actor": { "login": actor }, "label": { "name": label } })),
                Activity::Closed { id, actor, at } => events.push(json!({
                    "id": id, "event": "closed", "created_at": rfc3339(*at), "actor": { "login": actor } })),
                Activity::Reopened { id, actor, at } => events.push(json!({
                    "id": id, "event": "reopened", "created_at": rfc3339(*at), "actor": { "login": actor } })),
            }
        }
        self.serve(&format!("issues/{number}/comments"), json!(comments))
            .await;
        self.serve(&format!("issues/{number}/events"), json!(events))
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

    async fn comment_refused(&self, number: u64, status: u16) {
        Mock::given(method("POST"))
            .and(path(
                format!("/repos/acme/app/issues/{number}/comments").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(status).set_body_json(json!({ "message": "refused" })),
            )
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn rate_limited(&self) {
        Mock::given(method("GET"))
            .and(path("/repos/acme/app/issues"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("retry-after", RETRY_AFTER_SECS.to_string().as_str())
                    .set_body_json(json!({ "message": "secondary rate limit" })),
            )
            .mount(&self.w.server)
            .await;
    }

    fn webhook(&self, hook: Hook) -> (Vec<(String, String)>, Vec<u8>) {
        let repository = json!({ "full_name": "acme/app" });
        let (event, payload) = match hook {
            Hook::IssueOpened => (
                "issues",
                json!({ "action": "opened", "issue": { "number": 6 }, "repository": repository }),
            ),
            Hook::CommentOnIssue => (
                "issue_comment",
                json!({ "action": "created", "issue": { "number": 6 }, "repository": repository }),
            ),
            Hook::CommentOnChange => (
                "issue_comment",
                json!({ "action": "created", "issue": { "number": 6, "pull_request": { "url": "x" } }, "repository": repository }),
            ),
        };
        let body = serde_json::to_vec(&payload).unwrap();
        let headers = vec![
            ("X-GitHub-Event".to_string(), event.to_string()),
            ("X-GitHub-Delivery".to_string(), "9f1c-delivery".to_string()),
            (
                "X-Hub-Signature-256".to_string(),
                format!("sha256={}", hmac_hex(&body)),
            ),
        ];
        (headers, body)
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

    fn item(listed: &Listed) -> Value {
        json!({
            "iid": listed.number, "title": listed.title, "state": if listed.closed { "closed" } else { "opened" },
            "web_url": format!("https://gitlab.example/acme/app/-/issues/{}", listed.number),
            "created_at": rfc3339(listed.updated - 1000), "updated_at": rfc3339(listed.updated), "author": { "username": listed.author },
            "labels": listed.labels, "confidential": listed.confidential,
        })
    }

    async fn serve(&self, tail: &str, body: Value) {
        Mock::given(method("GET"))
            .and(path(format!("/projects/acme%2Fapp/{tail}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.w.server)
            .await;
    }
}

#[async_trait]
impl IssueHarness for GitLabHarness {
    fn source(&self) -> &dyn IntakeSource {
        &self.w.gl
    }

    fn scm(&self) -> &dyn Scm {
        &self.w.gl
    }

    fn repo(&self) -> RepoRef {
        self.w.repo.clone()
    }

    fn integration(&self) -> &str {
        "gitlab"
    }

    fn confidential_issues(&self) -> bool {
        true
    }

    async fn listing(&self, issues: &[Listed]) {
        let items: Vec<Value> = issues.iter().map(Self::item).collect();
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fapp/issues"))
            .and(query_param("scope", "all"))
            .and(query_param("order_by", "updated_at"))
            .and(query_param("sort", "asc"))
            .and(query_param("updated_after", Timestamp(SINCE).to_rfc3339()))
            .and(query_param("labels", "bug,saka:intake"))
            .respond_with(ResponseTemplate::new(200).set_body_json(items))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn issue(&self, listed: &Listed, body: &str) {
        let mut item = Self::item(listed);
        item["description"] = json!(body);
        self.serve(&format!("issues/{}", listed.number), item).await;
    }

    async fn missing_issue(&self, number: u64) {
        Mock::given(method("GET"))
            .and(path(
                format!("/projects/acme%2Fapp/issues/{number}").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(404).set_body_json(json!({ "message": "404 Not found" })),
            )
            .mount(&self.w.server)
            .await;
    }

    async fn activity(&self, number: u64, activity: &[Activity]) {
        let mut notes = vec![json!({ "id": 98, "body": "assigned to zed", "system": true,
            "created_at": rfc3339(NOW_MS), "author": { "username": "zed" } })];
        let mut labels = vec![
            json!({ "id": 99, "action": "remove", "created_at": rfc3339(NOW_MS),
            "user": { "username": "zed" }, "label": { "name": "saka:intake" } }),
        ];
        let mut states = Vec::new();
        for a in activity {
            match a {
                Activity::Comment { id, actor, body, at } => notes.push(json!({
                    "id": id, "body": body, "system": false, "created_at": rfc3339(*at),
                    "author": { "username": actor } })),
                Activity::Label { id, actor, label, at } => labels.push(json!({
                    "id": id, "action": "add", "created_at": rfc3339(*at),
                    "user": { "username": actor }, "label": { "name": label } })),
                Activity::Closed { id, actor, at } => states.push(json!({
                    "id": id, "state": "closed", "created_at": rfc3339(*at), "user": { "username": actor } })),
                Activity::Reopened { id, actor, at } => states.push(json!({
                    "id": id, "state": "reopened", "created_at": rfc3339(*at), "user": { "username": actor } })),
            }
        }
        self.serve(&format!("issues/{number}/notes"), json!(notes))
            .await;
        self.serve(
            &format!("issues/{number}/resource_label_events"),
            json!(labels),
        )
        .await;
        self.serve(
            &format!("issues/{number}/resource_state_events"),
            json!(states),
        )
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
        let level = if matches!(access, Access::Write) {
            30
        } else {
            20
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

    async fn comment_accepted(&self, number: u64, body: &str) {
        Mock::given(method("POST"))
            .and(path(
                format!("/projects/acme%2Fapp/issues/{number}/notes").as_str(),
            ))
            .and(body_json(json!({ "body": body })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 9 })))
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn comment_refused(&self, number: u64, status: u16) {
        Mock::given(method("POST"))
            .and(path(
                format!("/projects/acme%2Fapp/issues/{number}/notes").as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(status).set_body_json(json!({ "message": "refused" })),
            )
            .expect(1)
            .mount(&self.w.server)
            .await;
    }

    async fn rate_limited(&self) {
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fapp/issues"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", RETRY_AFTER_SECS.to_string().as_str())
                    .set_body_json(json!({ "message": "Retry later" })),
            )
            .mount(&self.w.server)
            .await;
    }

    fn webhook(&self, hook: Hook) -> (Vec<(String, String)>, Vec<u8>) {
        let project = json!({ "path_with_namespace": "acme/app" });
        let payload = match hook {
            Hook::IssueOpened => json!({
                "object_kind": "issue", "project": project,
                "object_attributes": { "iid": 6, "action": "open" },
            }),
            Hook::CommentOnIssue => json!({
                "object_kind": "note", "project": project,
                "object_attributes": { "noteable_type": "Issue" },
            }),
            Hook::CommentOnChange => json!({
                "object_kind": "note", "project": project,
                "object_attributes": { "noteable_type": "MergeRequest" },
            }),
        };
        let headers = vec![
            ("X-Gitlab-Event".to_string(), "Hook".to_string()),
            ("Idempotency-Key".to_string(), "9f1c-delivery".to_string()),
            (
                "X-Gitlab-Token".to_string(),
                String::from_utf8(SECRET.to_vec()).unwrap(),
            ),
        ];
        (headers, serde_json::to_vec(&payload).unwrap())
    }
}

async fn issues_are_listed_without_change_requests(h: &dyn IssueHarness) {
    let mut confidential = Listed::new(3, T3);
    confidential.confidential = true;
    let mut closed = Listed::new(2, T2);
    closed.closed = true;
    h.listing(&[Listed::new(1, T1), closed, confidential]).await;
    let listing = h
        .source()
        .list_issues(&h.repo(), Timestamp(SINCE), &labels())
        .await
        .unwrap();
    let seen: Vec<(u64, IssueState, bool)> = listing
        .issues
        .iter()
        .map(|i| (i.number, i.state, i.confidential))
        .collect();
    assert_eq!(
        seen,
        [
            (1, IssueState::Open, false),
            (2, IssueState::Closed, false),
            (3, IssueState::Open, h.confidential_issues()),
        ],
        "oldest first, pull requests dropped, state and confidentiality read"
    );
    assert_eq!(listing.resume_from, None);
    let first = &listing.issues[0];
    assert_eq!(
        (
            first.title.as_str(),
            first.author.as_str(),
            first.labels.as_slice()
        ),
        ("Issue 1", "alice", ["bug".to_string()].as_slice())
    );
    assert_eq!(first.updated_at, Timestamp(T1));
    assert_eq!(first.created_at, Timestamp(T1 - 1000));
    assert!(first.url.ends_with("/issues/1") || first.url.ends_with("/-/issues/1"));
}

async fn a_listing_over_the_cap_says_where_to_resume(h: &dyn IssueHarness) {
    let many: Vec<Listed> = (1..=MAX_ISSUES as u64 + 1)
        .map(|n| Listed::new(n, T1 + n as i64))
        .collect();
    h.listing(&many).await;
    let listing = h
        .source()
        .list_issues(&h.repo(), Timestamp(SINCE), &labels())
        .await
        .unwrap();
    assert_eq!(listing.issues.len(), MAX_ISSUES);
    assert_eq!(listing.resume_from, Some(Timestamp(T1 + MAX_ISSUES as i64)));
}

async fn an_issue_is_read_with_its_body_clipped(h: &dyn IssueHarness) {
    let listed = Listed::new(42, T1);
    let body = format!("{}é", "a".repeat(MAX_ISSUE_BODY_BYTES));
    h.issue(&listed, &body).await;
    let issue = h.source().issue(&issue_ref(h, 42)).await.unwrap();
    assert_eq!(issue.summary.number, 42);
    assert!(issue.body.len() <= MAX_ISSUE_BODY_BYTES);
    assert!(issue.body.ends_with(&format!("\n{TRUNCATION_MARKER}")));
    h.issue(&Listed::new(43, T1), "short").await;
    assert_eq!(
        h.source().issue(&issue_ref(h, 43)).await.unwrap().body,
        "short"
    );
}

async fn a_missing_issue_is_not_found(h: &dyn IssueHarness) {
    h.missing_issue(404).await;
    let err = h.source().issue(&issue_ref(h, 404)).await.unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.retryable),
        ("issue_not_found", false)
    );
    assert_eq!(err.params["number"], 404);
}

async fn signals_merge_comments_labels_and_state_changes_in_time_order(h: &dyn IssueHarness) {
    h.activity(
        7,
        &[
            Activity::Comment {
                id: 1,
                actor: "bob",
                body: "/saka intake",
                at: T2,
            },
            Activity::Label {
                id: 3,
                actor: "dave",
                label: "saka:intake",
                at: T1,
            },
            Activity::Closed {
                id: 4,
                actor: "erin",
                at: T3,
            },
            Activity::Reopened {
                id: 5,
                actor: "erin",
                at: T3 + 1,
            },
        ],
    )
    .await;
    let all = h
        .source()
        .issue_signals(&issue_ref(h, 7), Timestamp(T1))
        .await
        .unwrap();
    let seen: Vec<(&str, i64)> = all.iter().map(|s| (s.actor.as_str(), s.at.0)).collect();
    assert_eq!(
        seen,
        [("dave", T1), ("bob", T2), ("erin", T3), ("erin", T3 + 1)],
        "oldest first; system notes, label removals and other events are no signals"
    );
    assert_eq!(
        all.iter().map(|s| s.kind.clone()).collect::<Vec<_>>(),
        [
            IssueSignalKind::Labeled {
                name: "saka:intake".into()
            },
            IssueSignalKind::Comment {
                body: "/saka intake".into()
            },
            IssueSignalKind::Closed,
            IssueSignalKind::Reopened,
        ]
    );
    let ids: std::collections::BTreeSet<&str> = all.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids.len(), all.len(), "signal ids are unique");
    let later = h
        .source()
        .issue_signals(&issue_ref(h, 7), Timestamp(T3))
        .await
        .unwrap();
    assert_eq!(later.len(), 2);
}

async fn write_access_is_checked(h: &dyn IssueHarness) {
    h.write_access("maintainer", Access::Write).await;
    h.write_access("reporter", Access::ReadOnly).await;
    h.write_access("stranger", Access::Stranger).await;
    let can =
        |login: &'static str| async move { h.source().can_write(&h.repo(), login).await.unwrap() };
    assert!(can("maintainer").await);
    assert!(!can("reporter").await);
    assert!(!can("stranger").await);
}

async fn a_comment_is_posted_on_the_issue(h: &dyn IssueHarness) {
    h.comment_accepted(7, "Saka is working on this.").await;
    h.source()
        .post_issue_comment(&issue_ref(h, 7), "Saka is working on this.")
        .await
        .unwrap();
}

async fn a_refused_comment_is_permanent(h: &dyn IssueHarness) {
    h.comment_refused(7, 403).await;
    let err = h
        .source()
        .post_issue_comment(&issue_ref(h, 7), "Saka is working on this.")
        .await
        .unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.retryable),
        ("comment_refused", false)
    );
}

async fn an_oversized_comment_is_refused_before_it_is_sent(h: &dyn IssueHarness) {
    let err = h
        .source()
        .post_issue_comment(&issue_ref(h, 7), &"x".repeat(100_000))
        .await
        .unwrap_err();
    assert_eq!(err.code, "git_argument_invalid");
}

async fn a_rate_limited_listing_names_when_to_retry(h: &dyn IssueHarness) {
    h.rate_limited().await;
    let err = h
        .source()
        .list_issues(&h.repo(), Timestamp(SINCE), &[])
        .await
        .unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.retryable),
        ("scm_unavailable", true)
    );
    assert_eq!(
        err.retry_at,
        Some(Timestamp(NOW_MS + RETRY_AFTER_SECS * 1000))
    );
}

async fn capabilities_declare_confidential_issues(h: &dyn IssueHarness) {
    assert_eq!(
        h.source().capabilities(h.integration()),
        Some(IntakeCapabilities {
            confidential_issues: h.confidential_issues()
        })
    );
}

async fn issue_webhooks_name_the_issue_intake_target(h: &dyn IssueHarness) {
    let target = |hook: Hook| {
        let (headers, body) = h.webhook(hook);
        h.scm()
            .parse_webhook(h.integration(), &headers, &body)
            .unwrap()
            .targets
    };
    assert_eq!(target(Hook::IssueOpened), [WebhookTarget::IssueIntake]);
    assert_eq!(target(Hook::CommentOnIssue), [WebhookTarget::IssueIntake]);
    assert_eq!(target(Hook::CommentOnChange), [WebhookTarget::Intake]);
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

conformance!(github_issue_conformance, GitHubHarness:
    issues_are_listed_without_change_requests,
    a_listing_over_the_cap_says_where_to_resume,
    an_issue_is_read_with_its_body_clipped,
    a_missing_issue_is_not_found,
    signals_merge_comments_labels_and_state_changes_in_time_order,
    write_access_is_checked,
    a_comment_is_posted_on_the_issue,
    a_refused_comment_is_permanent,
    an_oversized_comment_is_refused_before_it_is_sent,
    a_rate_limited_listing_names_when_to_retry,
    capabilities_declare_confidential_issues,
    issue_webhooks_name_the_issue_intake_target,
    a_change_request_number_is_not_an_issue,
);

conformance!(gitlab_issue_conformance, GitLabHarness:
    issues_are_listed_without_change_requests,
    a_listing_over_the_cap_says_where_to_resume,
    an_issue_is_read_with_its_body_clipped,
    a_missing_issue_is_not_found,
    signals_merge_comments_labels_and_state_changes_in_time_order,
    write_access_is_checked,
    a_comment_is_posted_on_the_issue,
    a_refused_comment_is_permanent,
    an_oversized_comment_is_refused_before_it_is_sent,
    a_rate_limited_listing_names_when_to_retry,
    capabilities_declare_confidential_issues,
    issue_webhooks_name_the_issue_intake_target,
);

async fn a_change_request_number_is_not_an_issue(h: &GitHubHarness) {
    Mock::given(method("GET"))
        .and(path("/repos/acme/app/issues/5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "number": 5, "title": "A pull request", "state": "open",
            "pull_request": { "url": "x" }, "user": { "login": "mallory" }, "labels": [],
        })))
        .mount(&h.w.server)
        .await;
    let err = h.source().issue(&issue_ref(h, 5)).await.unwrap_err();
    assert_eq!(
        (err.code.as_str(), err.retryable),
        ("issue_is_change_request", false)
    );
}
