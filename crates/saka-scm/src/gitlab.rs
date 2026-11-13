mod ci;
mod client;
mod diagnose;
mod intake;
mod issues;
mod webhook;

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use saka_core::diagnostics::{CredentialStatus, RepoGrant, RepoHooks, WebhookStatus};
use saka_core::gate::Check;
use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_core::vocab::{ChangeState, CheckKind, MergeMethod, ReviewEvent};
use saka_ports::{
    ChangeCredential, ChangeOpening, ChangeSignal, ChangeSnapshot, ChangeSummary, DeviceCode,
    DevicePoll, EnqueueOutcome, IdentityLookup, MergeOutcome, MergeRequest, OpenChangeRequest,
    OpenedChange, PortError, PortResult, PostedReview, PushOutcome, PushRequest, RepoAccess,
    RepoInfo, RepoRef, Repos, ReviewPost, Scm, ScmCapabilities, StatusReport, StatusState,
    TokenRefresh, UpdateOutcome, WebhookDelivery, WebhookSignature, check_comment_body,
};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::github::app::{inaccessible, parse_time};
use client::Reply;
pub use client::{GitLabClient, GitLabConfig};

pub use diagnose::{TOKEN as GITLAB_DIAGNOSTICS, TOKEN_REQUIREMENTS, token_status};

pub const CAPABILITIES: ScmCapabilities = ScmCapabilities {
    provider: "gitlab",
    merge_methods: &[MergeMethod::Merge, MergeMethod::Squash],
    webhook_signature: Some(WebhookSignature::SharedToken),
    check_kinds: &[CheckKind::CheckRun, CheckKind::CommitStatus],
    draft_change_requests: true,
    delete_branch_after_merge: true,
    commit_status: true,
    merge_queue: true,
    update_branch: false,
    user_authorship: false,
    change_head_ref: "refs/merge-requests/{number}/head",
    diagnostics: Some(GITLAB_DIAGNOSTICS),
};

fn queued_in(mr: &Value) -> bool {
    mr["merge_when_pipeline_succeeds"] == true || mr["auto_merge"] == true
}

const MERGEABILITY_PENDING: [&str; 3] = ["checking", "unchecked", "preparing"];

pub struct GitLab {
    client: Arc<GitLabClient>,
    repos: Arc<dyn Repos>,
    webhook_secret: Option<Zeroizing<Vec<u8>>>,
}

fn text(value: &Value) -> Option<String> {
    value.as_str().filter(|s| !s.is_empty()).map(String::from)
}

fn head_of(mr: &Value) -> Option<String> {
    text(&mr["sha"]).or_else(|| text(&mr["diff_refs"]["head_sha"]))
}

fn merge_commit_of(mr: &Value) -> Option<String> {
    text(&mr["merge_commit_sha"])
        .or_else(|| text(&mr["squash_commit_sha"]))
        .or_else(|| text(&mr["sha"]))
}

fn opened(mr: &Value) -> PortResult<OpenedChange> {
    match (mr["iid"].as_u64().filter(|n| *n >= 1), head_of(mr)) {
        (Some(number), Some(head_sha)) => Ok(OpenedChange {
            number,
            url: mr["web_url"].as_str().unwrap_or_default().to_string(),
            head_sha,
        }),
        _ => Err(PortError::retryable(
            "scm_unavailable",
            json!({ "detail": "merge_request_incomplete" }),
        )),
    }
}

fn plain_segments(branch: &str) -> bool {
    !branch.is_empty()
        && !branch
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
}

impl GitLab {
    pub fn new(client: Arc<GitLabClient>, repos: Arc<dyn Repos>) -> GitLab {
        GitLab {
            client,
            repos,
            webhook_secret: None,
        }
    }

    #[must_use]
    pub fn with_webhook_secret(mut self, secret: Vec<u8>) -> GitLab {
        self.webhook_secret = Some(Zeroizing::new(secret));
        self
    }

    pub fn client(&self) -> &Arc<GitLabClient> {
        &self.client
    }

    async fn project_call(
        &self,
        full_name: &str,
        method: Method,
        tail: &[&str],
        body: Option<&Value>,
    ) -> PortResult<Reply> {
        let url = self.client.project_url(full_name, tail)?;
        self.client.call(full_name, method, url, body).await
    }

    async fn merge_request(&self, full_name: &str, number: u64) -> PortResult<Reply> {
        let n = number.to_string();
        let reply = self
            .project_call(full_name, Method::GET, &["merge_requests", &n], None)
            .await?;
        match reply.status {
            s if s.is_success() => Ok(reply),
            StatusCode::NOT_FOUND => Err(inaccessible(full_name, 404)),
            _ => Err(self.client.unexpected("change_request", &reply)),
        }
    }

    async fn checks(&self, full_name: &str, sha: &str) -> PortResult<Vec<Check>> {
        let mut url = self.client.project_url(full_name, &["pipelines"])?;
        url.query_pairs_mut()
            .append_pair("sha", sha)
            .append_pair("order_by", "id")
            .append_pair("sort", "desc")
            .append_pair("per_page", "1");
        let reply = self.client.call(full_name, Method::GET, url, None).await?;
        if !reply.status.is_success() {
            return Err(self.client.unexpected("pipelines", &reply));
        }
        let pipeline = reply.body.as_array().and_then(|p| p.first());
        let jobs = match pipeline.and_then(|p| p["id"].as_u64()) {
            Some(id) => {
                let id = id.to_string();
                let url = self
                    .client
                    .project_url(full_name, &["pipelines", &id, "jobs"])?;
                ci::from_jobs(&self.client.pages(full_name, url).await?)
            }
            None => Vec::new(),
        };
        let url = self
            .client
            .project_url(full_name, &["repository", "commits", sha, "statuses"])?;
        let statuses = self.client.pages(full_name, url).await?;
        let mut checks = ci::from_statuses(&statuses, &jobs);
        checks.splice(0..0, jobs);
        Ok(checks)
    }

    async fn existing_merge_request(
        &self,
        request: &OpenChangeRequest,
    ) -> PortResult<Option<OpenedChange>> {
        let full_name = &request.repo.full_name;
        let mut url = self.client.project_url(full_name, &["merge_requests"])?;
        url.query_pairs_mut()
            .append_pair("source_branch", &request.head_branch)
            .append_pair("target_branch", &request.base_branch)
            .append_pair("state", "opened");
        let found = self.client.pages(full_name, url).await?;
        found
            .iter()
            .filter(|mr| mr["source_branch"] == request.head_branch.as_str())
            .max_by_key(|mr| mr["iid"].as_u64().unwrap_or(0))
            .map(opened)
            .transpose()
    }

    async fn merged(&self, repo: &RepoRef, number: u64) -> PortResult<Option<MergeOutcome>> {
        let mr = self.merge_request(&repo.full_name, number).await?.body;
        if mr["state"] != "merged" {
            return Ok(None);
        }
        Ok(
            merge_commit_of(&mr).map(|merge_commit_sha| MergeOutcome::Merged {
                merge_commit_sha,
                merged_at: parse_time(&mr["merged_at"]),
            }),
        )
    }

    async fn refused_merge(
        &self,
        request: &MergeRequest,
        status: StatusCode,
    ) -> PortResult<MergeOutcome> {
        let mr = self
            .merge_request(&request.repo.full_name, request.number)
            .await?
            .body;
        let still_checking = mr["state"] == "opened"
            && mr["detailed_merge_status"]
                .as_str()
                .is_some_and(|s| MERGEABILITY_PENDING.contains(&s));
        if still_checking {
            return Err(PortError::retryable(
                "scm_unavailable",
                json!({ "detail": "mergeability_pending", "number": request.number }),
            ));
        }
        let reason = if status == StatusCode::UNPROCESSABLE_ENTITY {
            "rejected"
        } else {
            "not_mergeable"
        };
        Ok(MergeOutcome::Rejected {
            reason: reason.into(),
        })
    }
}

#[async_trait]
impl Scm for GitLab {
    async fn repository(&self, _integration_id: &str, full_name: &str) -> PortResult<RepoInfo> {
        let reply = self.project_call(full_name, Method::GET, &[], None).await?;
        if reply.status == StatusCode::NOT_FOUND {
            return Err(inaccessible(full_name, 404));
        }
        if !reply.status.is_success() {
            return Err(self.client.unexpected("repository", &reply));
        }
        let field = |key: &str| reply.body[key].as_str().unwrap_or_default().to_string();
        Ok(RepoInfo {
            full_name: field("path_with_namespace"),
            url: field("web_url"),
            default_branch: field("default_branch"),
        })
    }

    async fn branch_head(
        &self,
        _integration_id: &str,
        full_name: &str,
        branch: &str,
    ) -> PortResult<Option<String>> {
        let reply = self
            .project_call(
                full_name,
                Method::GET,
                &["repository", "branches", branch],
                None,
            )
            .await?;
        if reply.status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !reply.status.is_success() {
            return Err(self.client.unexpected("branch_head", &reply));
        }
        text(&reply.body["commit"]["id"]).map(Some).ok_or_else(|| {
            PortError::retryable("scm_unavailable", json!({ "detail": "branch_incomplete" }))
        })
    }

    async fn file_at(
        &self,
        _integration_id: &str,
        full_name: &str,
        commit: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<Option<Vec<u8>>> {
        if !plain_segments(path) {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "path" }),
            ));
        }
        let mut url = self
            .client
            .project_url(full_name, &["repository", "files", path, "raw"])?;
        url.query_pairs_mut().append_pair("ref", commit);
        let reply = self.client.call(full_name, Method::GET, url, None).await?;
        if reply.status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !reply.status.is_success() {
            return Err(self.client.unexpected("file_at", &reply));
        }
        if reply.raw.len() as u64 > max_bytes {
            return Err(PortError::permanent(
                "file_too_large",
                json!({ "path": path, "bytes": reply.raw.len(), "max_bytes": max_bytes }),
            ));
        }
        Ok(Some(reply.raw))
    }

    async fn push_branch(&self, request: &PushRequest) -> PortResult<PushOutcome> {
        self.repos.push(request).await
    }

    async fn open_change_request(&self, request: &OpenChangeRequest) -> PortResult<ChangeOpening> {
        if matches!(request.credential, ChangeCredential::User(_)) {
            return Err(PortError::unsupported("user_authorship"));
        }
        let full_name = &request.repo.full_name;
        let body = json!({
            "source_branch": request.head_branch,
            "target_branch": request.base_branch,
            "title": request.title,
            "description": request.body,
            "remove_source_branch": false,
        });
        let reply = self
            .project_call(full_name, Method::POST, &["merge_requests"], Some(&body))
            .await?;
        if reply.status.is_success() {
            return opened(&reply.body).map(ChangeOpening::Opened);
        }
        match reply.status {
            StatusCode::NOT_FOUND => Err(inaccessible(full_name, 404)),
            StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY => {
                match self.existing_merge_request(request).await? {
                    Some(found) => Ok(ChangeOpening::Opened(found)),
                    None => Err(self.client.unexpected("open_change_request", &reply)),
                }
            }
            _ => Err(self.client.unexpected("open_change_request", &reply)),
        }
    }

    async fn change_request(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSnapshot> {
        let mr = self.merge_request(&repo.full_name, number).await?.body;
        let head_sha = head_of(&mr).unwrap_or_default();
        let state = match mr["state"].as_str() {
            Some("merged") => ChangeState::Merged,
            Some("closed") => ChangeState::Closed,
            _ => ChangeState::Open,
        };
        let (merge_commit_sha, checks) = match state {
            ChangeState::Merged => (
                Some(merge_commit_of(&mr).ok_or_else(|| {
                    PortError::retryable(
                        "scm_unavailable",
                        json!({ "detail": "merge_commit_not_yet_listed", "number": number }),
                    )
                })?),
                Vec::new(),
            ),
            ChangeState::Open => (None, self.checks(&repo.full_name, &head_sha).await?),
            ChangeState::Closed => (None, Vec::new()),
        };
        Ok(ChangeSnapshot {
            state,
            head_sha,
            merge_commit_sha,
            merged_at: parse_time(&mr["merged_at"]),
            checks,
        })
    }

    async fn commit_checks(&self, repo: &RepoRef, sha: &str) -> PortResult<Vec<Check>> {
        self.checks(&repo.full_name, sha).await
    }

    async fn list_open_changes(
        &self,
        repo: &RepoRef,
        max: usize,
    ) -> PortResult<Vec<ChangeSummary>> {
        self.open_changes(repo, max).await
    }

    async fn change_summary(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSummary> {
        self.summary_of(repo, number).await
    }

    async fn change_files(
        &self,
        repo: &RepoRef,
        number: u64,
        max: usize,
    ) -> PortResult<Vec<String>> {
        self.files_of(repo, number, max).await
    }

    async fn change_signals(
        &self,
        repo: &RepoRef,
        number: u64,
        since: Timestamp,
    ) -> PortResult<Vec<ChangeSignal>> {
        self.signals_of(repo, number, since).await
    }

    async fn can_write(&self, repo: &RepoRef, login: &str) -> PortResult<bool> {
        self.writer(repo, login).await
    }

    async fn post_comment(&self, repo: &RepoRef, number: u64, body: &str) -> PortResult<()> {
        check_comment_body(body)?;
        let n = number.to_string();
        let reply = self
            .project_call(
                &repo.full_name,
                Method::POST,
                &["merge_requests", &n, "notes"],
                Some(&json!({ "body": body })),
            )
            .await?;
        match reply.status {
            s if s.is_success() => Ok(()),
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED => {
                Err(PortError::permanent(
                    "comment_refused",
                    json!({ "status": reply.status.as_u16() }),
                ))
            }
            _ => Err(self.client.unexpected("post_comment", &reply)),
        }
    }

    async fn update_branch(
        &self,
        _repo: &RepoRef,
        _number: u64,
        _expected_head_sha: &str,
    ) -> PortResult<UpdateOutcome> {
        Err(PortError::unsupported("update_branch"))
    }

    async fn merge(&self, request: &MergeRequest) -> PortResult<MergeOutcome> {
        let squash = match request.method {
            MergeMethod::Merge => false,
            MergeMethod::Squash => true,
            MergeMethod::Rebase => return Err(PortError::unsupported("merge_method")),
        };
        let mut body = json!({
            "sha": request.head_sha,
            "squash": squash,
            "should_remove_source_branch": false,
            "merge_commit_message": request.commit_title,
        });
        if squash {
            body["squash_commit_message"] = json!(request.commit_title);
        }
        let full_name = &request.repo.full_name;
        let n = request.number.to_string();
        let url = self
            .client
            .project_url(full_name, &["merge_requests", &n, "merge"])?;
        let reply = self.client.send(Method::PUT, url, Some(&body)).await?;
        let rejected = || {
            Ok(MergeOutcome::Rejected {
                reason: "rejected".into(),
            })
        };
        match reply.status {
            s if s.is_success() => {
                if reply.body["state"] == "merged"
                    && let Some(merge_commit_sha) = merge_commit_of(&reply.body)
                {
                    return Ok(MergeOutcome::Merged {
                        merge_commit_sha,
                        merged_at: parse_time(&reply.body["merged_at"]),
                    });
                }
                self.merged(&request.repo, request.number)
                    .await?
                    .ok_or_else(|| {
                        PortError::retryable(
                            "scm_unavailable",
                            json!({ "detail": "merge_pending", "number": request.number }),
                        )
                    })
            }
            StatusCode::CONFLICT => Ok(MergeOutcome::Rejected {
                reason: "head_changed".into(),
            }),
            s @ (StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::NOT_ACCEPTABLE
            | StatusCode::UNPROCESSABLE_ENTITY) => self.refused_merge(request, s).await,
            StatusCode::UNAUTHORIZED => {
                self.repository("", full_name).await?;
                rejected()
            }
            StatusCode::FORBIDDEN => rejected(),
            StatusCode::NOT_FOUND => Err(inaccessible(full_name, 404)),
            _ => Err(self.client.unexpected("merge", &reply)),
        }
    }

    async fn merge_queue(&self, repo: &RepoRef, _base_branch: &str) -> PortResult<bool> {
        let reply = self
            .project_call(&repo.full_name, Method::GET, &[], None)
            .await?;
        if reply.status == StatusCode::NOT_FOUND {
            return Err(inaccessible(&repo.full_name, 404));
        }
        if !reply.status.is_success() {
            return Err(self.client.unexpected("merge_queue", &reply));
        }
        Ok(reply.body["merge_pipelines_enabled"] == true
            && reply.body["merge_trains_enabled"] == true)
    }

    async fn enqueue(&self, request: &MergeRequest) -> PortResult<EnqueueOutcome> {
        let full_name = &request.repo.full_name;
        let n = request.number.to_string();
        let body = json!({
            "sha": request.head_sha,
            "auto_merge": true,
            "merge_when_pipeline_succeeds": true,
            "should_remove_source_branch": false,
        });
        let url = self
            .client
            .project_url(full_name, &["merge_requests", &n, "merge"])?;
        let reply = self.client.send(Method::PUT, url, Some(&body)).await?;
        let rejected = |reason: &str| {
            Ok(EnqueueOutcome::Rejected {
                reason: reason.into(),
            })
        };
        match reply.status {
            s if s.is_success() => {
                if reply.body["state"] == "merged"
                    && let Some(merge_commit_sha) = merge_commit_of(&reply.body)
                {
                    return Ok(EnqueueOutcome::Merged {
                        merge_commit_sha,
                        merged_at: parse_time(&reply.body["merged_at"]),
                    });
                }
                if queued_in(&reply.body) {
                    Ok(EnqueueOutcome::Queued)
                } else {
                    Err(PortError::retryable(
                        "scm_unavailable",
                        json!({ "detail": "enqueue_pending", "number": request.number }),
                    ))
                }
            }
            StatusCode::CONFLICT => rejected("head_changed"),
            StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_ACCEPTABLE => {
                rejected("not_mergeable")
            }
            StatusCode::UNPROCESSABLE_ENTITY | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                rejected("rejected")
            }
            StatusCode::NOT_FOUND => Err(inaccessible(full_name, 404)),
            _ => Err(self.client.unexpected("enqueue", &reply)),
        }
    }

    async fn in_merge_queue(&self, repo: &RepoRef, number: u64) -> PortResult<bool> {
        let mr = self.merge_request(&repo.full_name, number).await?.body;
        Ok(mr["state"] == "opened" && queued_in(&mr))
    }

    async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> PortResult<()> {
        if !plain_segments(branch) {
            return Err(PortError::permanent(
                "git_argument_invalid",
                json!({ "field": "branch" }),
            ));
        }
        let reply = self
            .project_call(
                &repo.full_name,
                Method::DELETE,
                &["repository", "branches", branch],
                None,
            )
            .await?;
        match reply.status {
            s if s.is_success() => Ok(()),
            StatusCode::NOT_FOUND => Ok(()),
            _ => Err(self.client.unexpected("delete_branch", &reply)),
        }
    }

    async fn post_review(&self, post: &ReviewPost) -> PortResult<PostedReview> {
        post.check()?;
        let full_name = &post.repo.full_name;
        let number = post.number.to_string();
        let refused = |reply: &Reply| {
            PortError::permanent("review_refused", json!({ "status": reply.status.as_u16() }))
        };
        let note = self
            .project_call(
                full_name,
                Method::POST,
                &["merge_requests", &number, "notes"],
                Some(&json!({ "body": post.body })),
            )
            .await?;
        match note.status {
            s if s.is_success() => {}
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED => {
                return Err(refused(&note));
            }
            _ => return Err(self.client.unexpected("post_review", &note)),
        }
        match post.event {
            ReviewEvent::Comment => {}
            ReviewEvent::Approve => {
                let approval = self
                    .project_call(
                        full_name,
                        Method::POST,
                        &["merge_requests", &number, "approve"],
                        Some(&json!({ "sha": post.head_sha })),
                    )
                    .await?;
                match approval.status {
                    s if s.is_success() => {}
                    StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED | StatusCode::CONFLICT => {
                        return Err(refused(&approval));
                    }
                    _ => return Err(self.client.unexpected("post_review", &approval)),
                }
            }
            ReviewEvent::RequestChanges => {
                let withdrawn = self
                    .project_call(
                        full_name,
                        Method::POST,
                        &["merge_requests", &number, "unapprove"],
                        None,
                    )
                    .await?;
                if withdrawn.status.is_server_error() {
                    return Err(self.client.unexpected("post_review", &withdrawn));
                }
            }
        }
        Ok(PostedReview { url: None })
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
        let state = match report.state {
            StatusState::Pending => "pending",
            StatusState::Success => "success",
            StatusState::Failure => "failed",
        };
        let body = json!({
            "state": state,
            "name": report.context,
            "description": report.description,
        });
        let reply = self
            .project_call(
                &repo.full_name,
                Method::POST,
                &["statuses", sha],
                Some(&body),
            )
            .await?;
        match reply.status {
            s if s.is_success() => Ok(()),
            StatusCode::BAD_REQUEST if reply.message().contains("Cannot transition") => Ok(()),
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => Err(PortError::permanent(
                "scm_status_refused",
                json!({ "status": reply.status.as_u16() }),
            )),
            _ => Err(self.client.unexpected("report_status", &reply)),
        }
    }

    fn capabilities(&self, _integration_id: &str) -> Option<ScmCapabilities> {
        Some(CAPABILITIES)
    }

    async fn credential_status(&self, _integration_id: &str) -> PortResult<CredentialStatus> {
        diagnose::credential_status(&self.client).await
    }

    async fn repo_grant(&self, repo: &RepoRef) -> PortResult<RepoGrant> {
        diagnose::repo_grant(&self.client, repo).await
    }

    async fn repo_webhooks(&self, repo: &RepoRef) -> PortResult<RepoHooks> {
        diagnose::repo_webhooks(&self.client, repo).await
    }

    async fn webhook_status(&self, _integration_id: &str) -> PortResult<WebhookStatus> {
        Err(PortError::unsupported("diagnostics"))
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

    fn device_flow_configured(&self, _integration_id: &str) -> PortResult<bool> {
        Ok(false)
    }

    async fn start_device_flow(&self, _integration_id: &str) -> PortResult<DeviceCode> {
        Err(PortError::unsupported("user_authorship"))
    }

    async fn poll_device_flow(
        &self,
        _integration_id: &str,
        _device_code: &SecretText,
    ) -> PortResult<DevicePoll> {
        Err(PortError::unsupported("user_authorship"))
    }

    async fn refresh_user_token(
        &self,
        _integration_id: &str,
        _refresh_token: &SecretText,
    ) -> PortResult<TokenRefresh> {
        Err(PortError::unsupported("user_authorship"))
    }

    async fn user_identity(
        &self,
        _integration_id: &str,
        _token: &SecretText,
    ) -> PortResult<IdentityLookup> {
        Err(PortError::unsupported("user_authorship"))
    }

    async fn user_repository_access(
        &self,
        _integration_id: &str,
        _full_name: &str,
        _token: &SecretText,
    ) -> PortResult<RepoAccess> {
        Err(PortError::unsupported("user_authorship"))
    }
}
