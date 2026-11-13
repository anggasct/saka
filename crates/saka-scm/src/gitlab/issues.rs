use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use saka_core::time::Timestamp;
use saka_ports::{
    IntakeCapabilities, IntakeSource, Issue, IssueListing, IssueRef, IssueSignal, IssueSignalKind,
    IssueState, IssueSummary, MAX_ISSUES, MAX_SIGNALS, PortError, PortResult, RepoRef,
    check_comment_body, clip_issue_body, clip_text,
};
use serde_json::{Value, json};

use super::GitLab;
use super::text;
use crate::github::app::parse_time;

const MAX_STATE_EVENTS: usize = 2000;
const MAX_LABEL_EVENTS: usize = 2000;

pub(super) const INTAKE_CAPABILITIES: IntakeCapabilities = IntakeCapabilities {
    confidential_issues: true,
};

fn incomplete() -> PortError {
    PortError::retryable("scm_unavailable", json!({ "detail": "issue_incomplete" }))
}

fn summary(item: &Value) -> PortResult<IssueSummary> {
    let Some(number) = item["iid"].as_u64().filter(|n| *n >= 1) else {
        return Err(incomplete());
    };
    Ok(IssueSummary {
        number,
        title: text(&item["title"]).unwrap_or_default(),
        url: text(&item["web_url"]).unwrap_or_default(),
        author: text(&item["author"]["username"]).unwrap_or_default(),
        state: if item["state"] == "closed" {
            IssueState::Closed
        } else {
            IssueState::Open
        },
        labels: item["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l.as_str().map(String::from))
            .collect(),
        confidential: item["confidential"] == true,
        created_at: parse_time(&item["created_at"]).unwrap_or_default(),
        updated_at: parse_time(&item["updated_at"]).unwrap_or_default(),
    })
}

fn note(item: &Value) -> Option<IssueSignal> {
    (item["system"] != true).then_some(())?;
    Some(IssueSignal {
        id: format!("comment:{}", item["id"].as_u64()?),
        at: parse_time(&item["created_at"])?,
        actor: item["author"]["username"].as_str()?.to_string(),
        kind: IssueSignalKind::Comment {
            body: clip_text(item["body"].as_str().unwrap_or_default()),
        },
    })
}

fn label_event(item: &Value) -> Option<IssueSignal> {
    (item["action"] == "add").then_some(())?;
    Some(IssueSignal {
        id: format!("label:{}", item["id"].as_u64()?),
        at: parse_time(&item["created_at"])?,
        actor: item["user"]["username"].as_str()?.to_string(),
        kind: IssueSignalKind::Labeled {
            name: item["label"]["name"].as_str()?.to_string(),
        },
    })
}

fn state_event(item: &Value) -> Option<IssueSignal> {
    let kind = match item["state"].as_str()? {
        "closed" => IssueSignalKind::Closed,
        "reopened" => IssueSignalKind::Reopened,
        _ => return None,
    };
    Some(IssueSignal {
        id: format!("state:{}", item["id"].as_u64()?),
        at: parse_time(&item["created_at"])?,
        actor: item["user"]["username"].as_str()?.to_string(),
        kind,
    })
}

#[async_trait]
impl IntakeSource for GitLab {
    fn capabilities(&self, _integration_id: &str) -> Option<IntakeCapabilities> {
        Some(INTAKE_CAPABILITIES)
    }

    async fn list_issues(
        &self,
        repo: &RepoRef,
        updated_since: Timestamp,
        labels: &[String],
    ) -> PortResult<IssueListing> {
        let mut url = self.client.project_url(&repo.full_name, &["issues"])?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("scope", "all")
                .append_pair("order_by", "updated_at")
                .append_pair("sort", "asc")
                .append_pair("updated_after", &updated_since.to_rfc3339());
            if !labels.is_empty() {
                query.append_pair("labels", &labels.join(","));
            }
        }
        let raw = self
            .client
            .pages_up_to(&repo.full_name, url, MAX_ISSUES + 1)
            .await?;
        let more = raw.len() > MAX_ISSUES;
        let issues = raw
            .iter()
            .take(MAX_ISSUES)
            .map(summary)
            .collect::<PortResult<Vec<_>>>()?;
        let resume_from = issues.last().map(|i| i.updated_at).filter(|_| more);
        Ok(IssueListing {
            issues,
            resume_from,
        })
    }

    async fn issue(&self, issue: &IssueRef) -> PortResult<Issue> {
        let n = issue.number.to_string();
        let reply = self
            .project_call(&issue.repo.full_name, Method::GET, &["issues", &n], None)
            .await?;
        if reply.status == StatusCode::NOT_FOUND {
            return Err(PortError::permanent(
                "issue_not_found",
                json!({ "number": issue.number }),
            ));
        }
        if !reply.status.is_success() {
            return Err(self.client.unexpected("issue", &reply));
        }
        Ok(Issue {
            summary: summary(&reply.body)?,
            body: clip_issue_body(reply.body["description"].as_str().unwrap_or_default()),
        })
    }

    async fn issue_signals(
        &self,
        issue: &IssueRef,
        since: Timestamp,
    ) -> PortResult<Vec<IssueSignal>> {
        let name = &issue.repo.full_name;
        let n = issue.number.to_string();
        let mut notes = self.client.project_url(name, &["issues", &n, "notes"])?;
        notes
            .query_pairs_mut()
            .append_pair("order_by", "created_at")
            .append_pair("sort", "desc");
        let labels = self
            .client
            .project_url(name, &["issues", &n, "resource_label_events"])?;
        let states = self
            .client
            .project_url(name, &["issues", &n, "resource_state_events"])?;
        let notes = self.client.pages_up_to(name, notes, MAX_SIGNALS).await?;
        let labels = self
            .client
            .pages_up_to(name, labels, MAX_LABEL_EVENTS)
            .await?;
        let states = self
            .client
            .pages_up_to(name, states, MAX_STATE_EVENTS)
            .await?;
        let mut signals: Vec<IssueSignal> = notes
            .iter()
            .filter_map(note)
            .chain(labels.iter().filter_map(label_event))
            .chain(states.iter().filter_map(state_event))
            .filter(|s| s.at >= since)
            .collect();
        signals.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        let excess = signals.len().saturating_sub(MAX_SIGNALS);
        signals.drain(..excess);
        Ok(signals)
    }

    async fn can_write(&self, repo: &RepoRef, login: &str) -> PortResult<bool> {
        self.writer(repo, login).await
    }

    async fn post_issue_comment(&self, issue: &IssueRef, body: &str) -> PortResult<()> {
        check_comment_body(body)?;
        let n = issue.number.to_string();
        let reply = self
            .project_call(
                &issue.repo.full_name,
                Method::POST,
                &["issues", &n, "notes"],
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
            _ => Err(self.client.unexpected("post_issue_comment", &reply)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> Value {
        json!({
            "iid": 7, "title": "Crash on start", "web_url": "https://gitlab.test/acme/app/-/issues/7",
            "state": "opened", "created_at": "2026-11-03T10:00:00Z", "updated_at": "2026-11-04T10:00:00Z", "author": { "username": "alice" },
            "labels": ["bug", "saka:intake"], "confidential": true,
        })
    }

    #[test]
    fn an_issue_becomes_the_same_summary_as_on_the_other_provider() {
        let s = summary(&item()).unwrap();
        assert_eq!((s.number, s.author.as_str()), (7, "alice"));
        assert_eq!(s.labels, ["bug", "saka:intake"]);
        assert_eq!(s.state, IssueState::Open);
        assert!(s.confidential);
        assert_eq!(s.created_at, Timestamp(1_793_700_000_000));
        let mut closed = item();
        closed["state"] = json!("closed");
        assert_eq!(summary(&closed).unwrap().state, IssueState::Closed);
    }

    #[test]
    fn system_notes_and_label_removals_are_no_signals() {
        let human = json!({ "id": 1, "system": false, "created_at": "2026-11-04T10:00:00Z",
            "author": { "username": "bob" }, "body": "/saka intake" });
        assert_eq!(note(&human).unwrap().actor, "bob");
        let system = json!({ "id": 2, "system": true, "created_at": "2026-11-04T10:00:00Z",
            "author": { "username": "bob" }, "body": "added ~label" });
        assert!(note(&system).is_none());
        let removed = json!({ "id": 4, "action": "remove", "created_at": "2026-11-04T10:00:00Z",
            "user": { "username": "carol" }, "label": { "name": "saka:intake" } });
        assert!(label_event(&removed).is_none());
    }

    #[test]
    fn state_events_become_closed_and_reopened_signals() {
        for (state, kind) in [
            ("closed", IssueSignalKind::Closed),
            ("reopened", IssueSignalKind::Reopened),
        ] {
            let found = state_event(&json!({ "id": 5, "state": state,
                "created_at": "2026-11-04T10:00:00Z", "user": { "username": "carol" } }))
            .unwrap();
            assert_eq!(found.kind, kind, "{state}");
        }
        assert!(
            state_event(&json!({ "id": 6, "state": "merged",
                "created_at": "2026-11-04T10:00:00Z", "user": { "username": "carol" } }))
            .is_none()
        );
    }
}
