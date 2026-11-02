pub mod app;
pub mod ci;
mod webhook;

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use saka_core::vocab::{ChangeState, CheckKind, MergeMethod};
use saka_ports::{
    ChangeSnapshot, MergeOutcome, MergeRequest, OpenChangeRequest, OpenedChange, PortError,
    PortResult, PushOutcome, PushRequest, RepoInfo, RepoRef, Repos, Scm, ScmCapabilities,
    StatusReport, WebhookDelivery, WebhookSignature,
};
use serde_json::{Value, json};
use zeroize::Zeroizing;

pub use app::{API_VERSION, GitHubApp, GitHubConfig};
use app::{Reply, inaccessible, parse_time, split_repo};

pub const CAPABILITIES: ScmCapabilities = ScmCapabilities {
    provider: "github",
    merge_methods: &[MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase],
    webhook_signature: Some(WebhookSignature::HmacSha256),
    check_kinds: &[CheckKind::CheckRun, CheckKind::CommitStatus],
    draft_change_requests: true,
    delete_branch_after_merge: true,
    commit_status: true,
};

pub struct GitHub {
    app: Arc<GitHubApp>,
    repos: Arc<dyn Repos>,
    webhook_secret: Option<Zeroizing<Vec<u8>>>,
}

fn opened(pr: &Value) -> PortResult<OpenedChange> {
    let number = pr["number"].as_u64().filter(|n| *n >= 1);
    let head = pr["head"]["sha"].as_str();
    match (number, head) {
        (Some(number), Some(head)) => Ok(OpenedChange {
            number,
            url: pr["html_url"].as_str().unwrap_or_default().to_string(),
            head_sha: head.to_string(),
        }),
        _ => Err(PortError::retryable(
            "scm_unavailable",
            json!({ "detail": "pull_request_incomplete" }),
        )),
    }
}

impl GitHub {
    pub fn new(app: Arc<GitHubApp>, repos: Arc<dyn Repos>) -> GitHub {
        GitHub {
            app,
            repos,
            webhook_secret: None,
        }
    }

    #[must_use]
    pub fn with_webhook_secret(mut self, secret: Vec<u8>) -> GitHub {
        self.webhook_secret = Some(Zeroizing::new(secret));
        self
    }

    pub fn app(&self) -> &Arc<GitHubApp> {
        &self.app
    }

    async fn repo_call(
        &self,
        full_name: &str,
        method: Method,
        tail: &[&str],
        body: Option<&Value>,
    ) -> PortResult<Reply> {
        let (owner, name) = split_repo(full_name)?;
        let mut segments = vec!["repos", owner, name];
        segments.extend_from_slice(tail);
        let url = self.app.url(&segments)?;
        self.app.call(full_name, method, url, body).await
    }

    async fn merge_commit(&self, full_name: &str, number: u64) -> PortResult<String> {
        let (owner, name) = split_repo(full_name)?;
        let n = number.to_string();
        let mut url = self
            .app
            .url(&["repos", owner, name, "issues", &n, "events"])?;
        url.query_pairs_mut().append_pair("per_page", "100");
        let events = self.app.pages(full_name, url, None).await?;
        events
            .iter()
            .rev()
            .find(|e| e["event"] == "merged")
            .and_then(|e| e["commit_id"].as_str())
            .map(String::from)
            .ok_or_else(|| {
                PortError::retryable(
                    "scm_unavailable",
                    json!({ "detail": "merge_commit_not_yet_listed", "number": number }),
                )
            })
    }

    async fn checks(&self, full_name: &str, head: &str) -> PortResult<Vec<saka_core::gate::Check>> {
        let (owner, name) = split_repo(full_name)?;
        let mut runs_url = self
            .app
            .url(&["repos", owner, name, "commits", head, "check-runs"])?;
        runs_url.query_pairs_mut().append_pair("per_page", "100");
        let runs = self
            .app
            .pages(full_name, runs_url, Some("check_runs"))
            .await?;
        let mut status_url = self
            .app
            .url(&["repos", owner, name, "commits", head, "status"])?;
        status_url.query_pairs_mut().append_pair("per_page", "100");
        let statuses = self
            .app
            .pages(full_name, status_url, Some("statuses"))
            .await?;
        let mut checks = ci::from_check_runs(&runs);
        checks.extend(ci::from_statuses(&statuses));
        Ok(checks)
    }
}

#[async_trait]
impl Scm for GitHub {
    async fn repository(&self, _integration_id: &str, full_name: &str) -> PortResult<RepoInfo> {
        let reply = self.repo_call(full_name, Method::GET, &[], None).await?;
        if reply.status == StatusCode::NOT_FOUND {
            return Err(inaccessible(full_name, 404));
        }
        if !reply.status.is_success() {
            return Err(self.app.unexpected("repository", &reply));
        }
        let text = |key: &str| reply.body[key].as_str().unwrap_or_default().to_string();
        Ok(RepoInfo {
            full_name: text("full_name"),
            url: text("html_url"),
            default_branch: text("default_branch"),
        })
    }

    async fn push_branch(&self, request: &PushRequest) -> PortResult<PushOutcome> {
        self.repos.push(request).await
    }

    async fn open_change_request(&self, request: &OpenChangeRequest) -> PortResult<OpenedChange> {
        let full_name = &request.repo.full_name;
        let body = json!({
            "title": request.title,
            "head": request.head_branch,
            "base": request.base_branch,
            "body": request.body,
            "draft": false,
        });
        let reply = self
            .repo_call(full_name, Method::POST, &["pulls"], Some(&body))
            .await?;
        if reply.status.is_success() {
            return opened(&reply.body);
        }
        if reply.status == StatusCode::NOT_FOUND {
            return Err(inaccessible(full_name, 404));
        }
        if reply.status != StatusCode::UNPROCESSABLE_ENTITY {
            return Err(self.app.unexpected("open_change_request", &reply));
        }
        // Usually "a pull request already exists": a repeated job finds the one it opened.
        let (owner, name) = split_repo(full_name)?;
        let mut url = self.app.url(&["repos", owner, name, "pulls"])?;
        url.query_pairs_mut()
            .append_pair("head", &format!("{owner}:{}", request.head_branch))
            .append_pair("base", &request.base_branch)
            .append_pair("state", "all")
            .append_pair("per_page", "100");
        let found = self.app.pages(full_name, url, None).await?;
        match found
            .iter()
            .filter(|pr| pr["head"]["ref"] == request.head_branch.as_str())
            .max_by_key(|pr| pr["number"].as_u64().unwrap_or(0))
        {
            Some(pr) => opened(pr),
            None => Err(self.app.unexpected("open_change_request", &reply)),
        }
    }

    async fn change_request(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSnapshot> {
        let full_name = &repo.full_name;
        let n = number.to_string();
        let reply = self
            .repo_call(full_name, Method::GET, &["pulls", &n], None)
            .await?;
        if reply.status == StatusCode::NOT_FOUND {
            return Err(inaccessible(full_name, 404));
        }
        if !reply.status.is_success() {
            return Err(self.app.unexpected("change_request", &reply));
        }
        let pr = &reply.body;
        let head_sha = pr["head"]["sha"].as_str().unwrap_or_default().to_string();
        let merged = pr["merged"].as_bool().unwrap_or(false) || !pr["merged_at"].is_null();
        let state = match (pr["state"].as_str(), merged) {
            (_, true) => ChangeState::Merged,
            (Some("closed"), false) => ChangeState::Closed,
            _ => ChangeState::Open,
        };
        let (merge_commit_sha, checks) = match state {
            ChangeState::Merged => (
                Some(self.merge_commit(full_name, number).await?),
                Vec::new(),
            ),
            ChangeState::Open => (None, self.checks(full_name, &head_sha).await?),
            ChangeState::Closed => (None, Vec::new()),
        };
        Ok(ChangeSnapshot {
            state,
            head_sha,
            merge_commit_sha,
            merged_at: parse_time(&pr["merged_at"]),
            checks,
        })
    }

    async fn merge(&self, request: &MergeRequest) -> PortResult<MergeOutcome> {
        let n = request.number.to_string();
        let body = json!({
            "sha": request.head_sha,
            "merge_method": request.method.as_str(),
            "commit_title": request.commit_title,
        });
        let reply = self
            .repo_call(
                &request.repo.full_name,
                Method::PUT,
                &["pulls", &n, "merge"],
                Some(&body),
            )
            .await?;
        let rejected = |reason: &str| {
            Ok(MergeOutcome::Rejected {
                reason: reason.into(),
            })
        };
        match reply.status {
            s if s.is_success() => match reply.body["sha"].as_str() {
                Some(sha) => Ok(MergeOutcome::Merged {
                    merge_commit_sha: sha.to_string(),
                    merged_at: None,
                }),
                None => Err(PortError::retryable(
                    "scm_unavailable",
                    json!({ "detail": "merge_sha_missing" }),
                )),
            },
            StatusCode::METHOD_NOT_ALLOWED => rejected("not_mergeable"),
            StatusCode::CONFLICT => rejected("head_changed"),
            StatusCode::UNPROCESSABLE_ENTITY => rejected("rejected"),
            StatusCode::NOT_FOUND => Err(inaccessible(&request.repo.full_name, 404)),
            _ => Err(self.app.unexpected("merge", &reply)),
        }
    }

    async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> PortResult<()> {
        if branch.is_empty()
            || branch
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..")
        {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "branch" }),
            ));
        }
        let mut tail = vec!["git", "refs", "heads"];
        tail.extend(branch.split('/'));
        let reply = self
            .repo_call(&repo.full_name, Method::DELETE, &tail, None)
            .await?;
        match reply.status {
            s if s.is_success() => Ok(()),
            // Already gone.
            StatusCode::NOT_FOUND | StatusCode::UNPROCESSABLE_ENTITY => Ok(()),
            _ => Err(self.app.unexpected("delete_branch", &reply)),
        }
    }

    async fn report_status(
        &self,
        repo: &RepoRef,
        sha: &str,
        report: &StatusReport,
    ) -> PortResult<()> {
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "sha" }),
            ));
        }
        let body = json!({
            "state": report.state.as_str(),
            "context": report.context,
            "description": report.description,
        });
        let reply = self
            .repo_call(
                &repo.full_name,
                Method::POST,
                &["statuses", sha],
                Some(&body),
            )
            .await?;
        match reply.status {
            s if s.is_success() => Ok(()),
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => Err(PortError::permanent(
                "scm_status_refused",
                json!({ "status": reply.status.as_u16() }),
            )),
            _ => Err(self.app.unexpected("report_status", &reply)),
        }
    }

    fn capabilities(&self, _integration_id: &str) -> Option<ScmCapabilities> {
        Some(CAPABILITIES)
    }

    fn webhook_configured(&self, _integration_id: &str) -> PortResult<bool> {
        Ok(self.webhook_secret.is_some())
    }

    fn parse_webhook(
        &self,
        _integration_id: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> PortResult<WebhookDelivery> {
        let secret = self
            .webhook_secret
            .as_ref()
            .ok_or_else(|| PortError::permanent("webhook_not_configured", json!({})))?;
        webhook::parse(secret, headers, body)
    }
}
