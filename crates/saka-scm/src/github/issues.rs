use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use saka_core::time::Timestamp;
use saka_ports::{
    IntakeCapabilities, IntakeSource, Issue, IssueListing, IssueRef, IssueSignal, IssueSignalKind,
    IssueState, IssueSummary, MAX_ISSUES, MAX_SIGNALS, PortError, PortResult, RepoRef, Scm,
    clip_issue_body, clip_text,
};
use serde_json::{Value, json};

use super::GitHub;
use super::app::{parse_time, split_repo};
use super::intake::{incomplete, text};

const MAX_RAW_ISSUES: usize = MAX_ISSUES * 5;

pub(super) const INTAKE_CAPABILITIES: IntakeCapabilities = IntakeCapabilities {
    confidential_issues: false,
};

fn summary(item: &Value) -> PortResult<IssueSummary> {
    let Some(number) = item["number"].as_u64().filter(|n| *n >= 1) else {
        return Err(incomplete("issue_incomplete"));
    };
    Ok(IssueSummary {
        number,
        title: text(&item["title"]),
        url: text(&item["html_url"]),
        author: text(&item["user"]["login"]),
        state: if item["state"] == "closed" {
            IssueState::Closed
        } else {
            IssueState::Open
        },
        labels: item["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str().map(String::from))
            .collect(),
        confidential: false,
        updated_at: parse_time(&item["updated_at"]).unwrap_or_default(),
    })
}

fn comment(item: &Value) -> Option<IssueSignal> {
    Some(IssueSignal {
        id: format!("comment:{}", item["id"].as_u64()?),
        at: parse_time(&item["created_at"])?,
        actor: item["user"]["login"].as_str()?.to_string(),
        kind: IssueSignalKind::Comment {
            body: clip_text(item["body"].as_str().unwrap_or_default()),
        },
    })
}

fn event(item: &Value) -> Option<IssueSignal> {
    let kind = match item["event"].as_str()? {
        "labeled" => IssueSignalKind::Labeled {
            name: item["label"]["name"].as_str()?.to_string(),
        },
        "closed" => IssueSignalKind::Closed,
        "reopened" => IssueSignalKind::Reopened,
        _ => return None,
    };
    Some(IssueSignal {
        id: format!("event:{}", item["id"].as_u64()?),
        at: parse_time(&item["created_at"])?,
        actor: item["actor"]["login"].as_str()?.to_string(),
        kind,
    })
}

fn not_found(number: u64) -> PortError {
    PortError::permanent("issue_not_found", json!({ "number": number }))
}

#[async_trait]
impl IntakeSource for GitHub {
    fn capabilities(&self, _integration_id: &str) -> Option<IntakeCapabilities> {
        Some(INTAKE_CAPABILITIES)
    }

    async fn list_issues(
        &self,
        repo: &RepoRef,
        updated_since: Timestamp,
        labels: &[String],
    ) -> PortResult<IssueListing> {
        let (owner, name) = split_repo(&repo.full_name)?;
        let mut url = self.app.url(&["repos", owner, name, "issues"])?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("state", "all")
                .append_pair("sort", "updated")
                .append_pair("direction", "asc")
                .append_pair("per_page", "100")
                .append_pair("since", &updated_since.to_rfc3339());
            if !labels.is_empty() {
                query.append_pair("labels", &labels.join(","));
            }
        }
        let raw = self
            .app
            .pages_up_to(&repo.full_name, url, None, MAX_RAW_ISSUES)
            .await?;
        let mut issues = Vec::new();
        let mut examined = None;
        let mut stopped_early = false;
        for item in &raw {
            if issues.len() == MAX_ISSUES {
                stopped_early = true;
                break;
            }
            examined = parse_time(&item["updated_at"]);
            if item["pull_request"].is_null() {
                issues.push(summary(item)?);
            }
        }
        let capped = raw.len() >= MAX_RAW_ISSUES;
        Ok(IssueListing {
            issues,
            resume_from: examined.filter(|_| stopped_early || capped),
        })
    }

    async fn issue(&self, issue: &IssueRef) -> PortResult<Issue> {
        let n = issue.number.to_string();
        let reply = self
            .repo_call(&issue.repo.full_name, Method::GET, &["issues", &n], None)
            .await?;
        if matches!(reply.status, StatusCode::NOT_FOUND | StatusCode::GONE) {
            return Err(not_found(issue.number));
        }
        if !reply.status.is_success() {
            return Err(self.app.unexpected("issue", &reply));
        }
        if !reply.body["pull_request"].is_null() {
            return Err(PortError::permanent(
                "issue_is_change_request",
                json!({ "number": issue.number }),
            ));
        }
        Ok(Issue {
            summary: summary(&reply.body)?,
            body: clip_issue_body(reply.body["body"].as_str().unwrap_or_default()),
        })
    }

    async fn issue_signals(
        &self,
        issue: &IssueRef,
        since: Timestamp,
    ) -> PortResult<Vec<IssueSignal>> {
        let (owner, name) = split_repo(&issue.repo.full_name)?;
        let n = issue.number.to_string();
        let mut comments = self
            .app
            .url(&["repos", owner, name, "issues", &n, "comments"])?;
        comments
            .query_pairs_mut()
            .append_pair("per_page", "100")
            .append_pair("since", &since.to_rfc3339());
        let mut events = self
            .app
            .url(&["repos", owner, name, "issues", &n, "events"])?;
        events.query_pairs_mut().append_pair("per_page", "100");
        let comments = self
            .app
            .pages(&issue.repo.full_name, comments, None)
            .await?;
        let events = self.app.pages(&issue.repo.full_name, events, None).await?;
        let mut signals: Vec<IssueSignal> = comments
            .iter()
            .filter_map(comment)
            .chain(events.iter().filter_map(event))
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
        Scm::post_comment(self, &issue.repo, issue.number, body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> Value {
        json!({
            "number": 42, "title": "Crash on start", "html_url": "https://github.com/acme/app/issues/42",
            "state": "open", "updated_at": "2026-11-04T10:00:00Z", "user": { "login": "alice" },
            "labels": [{ "name": "bug" }, { "name": "saka:intake" }],
        })
    }

    #[test]
    fn an_issue_becomes_a_summary_without_provider_words() {
        let s = summary(&item()).unwrap();
        assert_eq!((s.number, s.author.as_str()), (42, "alice"));
        assert_eq!(s.labels, ["bug", "saka:intake"]);
        assert_eq!(s.state, IssueState::Open);
        assert!(!s.confidential);
        assert_eq!(s.updated_at, Timestamp(1_793_786_400_000));
    }

    #[test]
    fn a_closed_issue_has_the_closed_state() {
        let mut closed = item();
        closed["state"] = json!("closed");
        assert_eq!(summary(&closed).unwrap().state, IssueState::Closed);
    }

    #[test]
    fn an_issue_without_a_number_is_retried_not_guessed() {
        let mut broken = item();
        broken["number"] = Value::Null;
        assert!(summary(&broken).unwrap_err().retryable);
    }

    #[test]
    fn comments_and_state_events_become_signals_with_their_actor() {
        let c = comment(&json!({
            "id": 11, "created_at": "2026-11-04T10:00:00Z", "user": { "login": "bob" }, "body": "/saka intake",
        }))
        .unwrap();
        assert_eq!((c.id.as_str(), c.actor.as_str()), ("comment:11", "bob"));
        let labeled = event(&json!({
            "id": 12, "event": "labeled", "created_at": "2026-11-04T10:01:00Z",
            "actor": { "login": "carol" }, "label": { "name": "saka:intake" },
        }))
        .unwrap();
        assert_eq!(
            labeled.kind,
            IssueSignalKind::Labeled {
                name: "saka:intake".into()
            }
        );
        for (name, kind) in [
            ("closed", IssueSignalKind::Closed),
            ("reopened", IssueSignalKind::Reopened),
        ] {
            let found = event(&json!({
                "id": 13, "event": name, "created_at": "2026-11-04T10:02:00Z", "actor": { "login": "carol" },
            }))
            .unwrap();
            assert_eq!(found.kind, kind, "{name}");
        }
        assert!(
            event(&json!({
                "id": 14, "event": "unlabeled", "created_at": "2026-11-04T10:03:00Z",
                "actor": { "login": "carol" }, "label": { "name": "x" },
            }))
            .is_none()
        );
    }
}
