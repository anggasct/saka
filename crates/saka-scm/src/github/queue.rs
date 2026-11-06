use reqwest::{Method, StatusCode};
use saka_ports::{EnqueueOutcome, MergeRequest, PortError, PortResult, RepoRef};
use serde_json::{Value, json};

use super::GitHub;
use super::app::{inaccessible, parse_time, split_repo};

const QUEUE_QUERY: &str = "query($owner: String!, $name: String!, $branch: String!) { repository(owner: $owner, name: $name) { mergeQueue(branch: $branch) { id } } }";
const MEMBER_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { isInMergeQueue } } }";
const ENQUEUE_MUTATION: &str = "mutation($id: ID!, $oid: GitObjectID!) { enqueuePullRequest(input: {pullRequestId: $id, expectedHeadOid: $oid}) { mergeQueueEntry { id } } }";
const UNREADABLE: [&str; 3] = ["NOT_FOUND", "FORBIDDEN", "INSUFFICIENT_SCOPES"];

fn errors(body: &Value) -> &[Value] {
    body["errors"].as_array().map_or(&[], Vec::as_slice)
}

impl GitHub {
    async fn graphql(&self, full_name: &str, query: &str, variables: Value) -> PortResult<Value> {
        let url = self.app.graphql_url()?;
        let body = json!({ "query": query, "variables": variables });
        let reply = self
            .app
            .call(full_name, Method::POST, url, Some(&body))
            .await?;
        if reply.status == StatusCode::NOT_FOUND {
            return Err(inaccessible(full_name, 404));
        }
        if !reply.status.is_success() {
            return Err(self.app.unexpected("graphql", &reply));
        }
        Ok(reply.body)
    }

    pub(super) async fn queue_enabled(
        &self,
        repo: &RepoRef,
        base_branch: &str,
    ) -> PortResult<bool> {
        let (owner, name) = split_repo(&repo.full_name)?;
        let body = self
            .graphql(
                &repo.full_name,
                QUEUE_QUERY,
                json!({ "owner": owner, "name": name, "branch": base_branch }),
            )
            .await?;
        let found = errors(&body);
        if found.is_empty() {
            return Ok(body["data"]["repository"]["mergeQueue"].is_object());
        }
        if found
            .iter()
            .all(|e| e["type"].as_str().is_some_and(|t| UNREADABLE.contains(&t)))
        {
            return Ok(false);
        }
        Err(PortError::retryable(
            "scm_unavailable",
            json!({ "detail": "graphql_error" }),
        ))
    }

    pub(super) async fn enqueue_change(
        &self,
        request: &MergeRequest,
    ) -> PortResult<EnqueueOutcome> {
        let full_name = &request.repo.full_name;
        let n = request.number.to_string();
        let found = self
            .repo_call(full_name, Method::GET, &["pulls", &n], None)
            .await?;
        if found.status == StatusCode::NOT_FOUND {
            return Err(inaccessible(full_name, 404));
        }
        if !found.status.is_success() {
            return Err(self.app.unexpected("enqueue", &found));
        }
        let pr = &found.body;
        let rejected = |reason: &str| {
            Ok(EnqueueOutcome::Rejected {
                reason: reason.into(),
            })
        };
        if pr["merged"] == true || pr["merged_at"].is_string() {
            let merge_commit_sha = self.merge_commit(full_name, request.number).await?;
            return Ok(EnqueueOutcome::Merged {
                merge_commit_sha,
                merged_at: parse_time(&pr["merged_at"]),
            });
        }
        if pr["state"] != "open" {
            return rejected("not_mergeable");
        }
        if pr["head"]["sha"].as_str() != Some(request.head_sha.as_str()) {
            return rejected("head_changed");
        }
        let Some(id) = pr["node_id"].as_str() else {
            return Err(PortError::retryable(
                "scm_unavailable",
                json!({ "detail": "pull_request_incomplete" }),
            ));
        };
        let body = self
            .graphql(
                full_name,
                ENQUEUE_MUTATION,
                json!({ "id": id, "oid": request.head_sha }),
            )
            .await?;
        if let Some(first) = errors(&body).first() {
            let message = first["message"].as_str().unwrap_or_default().to_lowercase();
            return if message.contains("head") || message.contains("expected") {
                rejected("head_changed")
            } else {
                rejected("rejected")
            };
        }
        if body["data"]["enqueuePullRequest"]["mergeQueueEntry"].is_object() {
            Ok(EnqueueOutcome::Queued)
        } else {
            Err(PortError::retryable(
                "scm_unavailable",
                json!({ "detail": "enqueue_pending", "number": request.number }),
            ))
        }
    }

    pub(super) async fn queue_member(&self, repo: &RepoRef, number: u64) -> PortResult<bool> {
        let (owner, name) = split_repo(&repo.full_name)?;
        let body = self
            .graphql(
                &repo.full_name,
                MEMBER_QUERY,
                json!({ "owner": owner, "name": name, "number": number }),
            )
            .await?;
        if !errors(&body).is_empty() {
            return Err(PortError::retryable(
                "scm_unavailable",
                json!({ "detail": "graphql_error" }),
            ));
        }
        Ok(body["data"]["repository"]["pullRequest"]["isInMergeQueue"] == true)
    }
}
