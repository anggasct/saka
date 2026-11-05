use reqwest::{Method, StatusCode};
use saka_core::time::Timestamp;
use saka_core::vocab::ChangeState;
use saka_ports::{
    ChangeSignal, ChangeSummary, MAX_SIGNALS, PortError, PortResult, RepoRef, SignalKind,
    clip_text, unique_paths,
};
use serde_json::{Value, json};

use super::GitHub;
use super::app::{inaccessible, parse_time, split_repo};

fn incomplete(detail: &'static str) -> PortError {
    PortError::retryable("scm_unavailable", json!({ "detail": detail }))
}

fn clipped(value: &Value) -> String {
    clip_text(value.as_str().unwrap_or_default())
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

pub(super) fn summary(pr: &Value) -> PortResult<ChangeSummary> {
    let number = pr["number"].as_u64().filter(|n| *n >= 1);
    let head_sha = pr["head"]["sha"].as_str().filter(|s| !s.is_empty());
    let (Some(number), Some(head_sha)) = (number, head_sha) else {
        return Err(incomplete("pull_request_incomplete"));
    };
    let merged = pr["merged"].as_bool().unwrap_or(false) || !pr["merged_at"].is_null();
    let state = match (pr["state"].as_str(), merged) {
        (_, true) => ChangeState::Merged,
        (Some("closed"), false) => ChangeState::Closed,
        _ => ChangeState::Open,
    };
    let head_repo = pr["head"]["repo"]["full_name"].as_str();
    let base_repo = pr["base"]["repo"]["full_name"].as_str();
    Ok(ChangeSummary {
        number,
        title: text(&pr["title"]),
        body: clipped(&pr["body"]),
        url: text(&pr["html_url"]),
        head_branch: text(&pr["head"]["ref"]),
        base_branch: text(&pr["base"]["ref"]),
        head_sha: head_sha.to_string(),
        author: text(&pr["user"]["login"]),
        labels: pr["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str().map(String::from))
            .collect(),
        draft: pr["draft"].as_bool().unwrap_or(false),
        from_fork: head_repo.is_none_or(|head| Some(head) != base_repo),
        state,
        updated_at: parse_time(&pr["updated_at"]).unwrap_or_default(),
    })
}

fn comment(item: &Value) -> Option<ChangeSignal> {
    Some(ChangeSignal {
        id: format!("comment:{}", item["id"].as_u64()?),
        at: parse_time(&item["created_at"])?,
        actor: item["user"]["login"].as_str()?.to_string(),
        kind: SignalKind::Comment {
            body: clipped(&item["body"]),
        },
    })
}

fn label(item: &Value) -> Option<ChangeSignal> {
    (item["event"] == "labeled").then_some(())?;
    Some(ChangeSignal {
        id: format!("label:{}", item["id"].as_u64()?),
        at: parse_time(&item["created_at"])?,
        actor: item["actor"]["login"].as_str()?.to_string(),
        kind: SignalKind::Label {
            name: item["label"]["name"].as_str()?.to_string(),
        },
    })
}

fn writes(role: &str) -> bool {
    matches!(role, "admin" | "maintain" | "write")
}

impl GitHub {
    pub(super) async fn open_changes(
        &self,
        repo: &RepoRef,
        max: usize,
    ) -> PortResult<Vec<ChangeSummary>> {
        let (owner, name) = split_repo(&repo.full_name)?;
        let mut url = self.app.url(&["repos", owner, name, "pulls"])?;
        url.query_pairs_mut()
            .append_pair("state", "open")
            .append_pair("sort", "updated")
            .append_pair("direction", "desc")
            .append_pair("per_page", "100");
        let found = self
            .app
            .pages_up_to(&repo.full_name, url, None, max)
            .await?;
        found.iter().map(summary).collect()
    }

    pub(super) async fn summary_of(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> PortResult<ChangeSummary> {
        let n = number.to_string();
        let reply = self
            .repo_call(&repo.full_name, Method::GET, &["pulls", &n], None)
            .await?;
        if reply.status == StatusCode::NOT_FOUND {
            return Err(inaccessible(&repo.full_name, 404));
        }
        if !reply.status.is_success() {
            return Err(self.app.unexpected("change_summary", &reply));
        }
        summary(&reply.body)
    }

    pub(super) async fn files_of(
        &self,
        repo: &RepoRef,
        number: u64,
        max: usize,
    ) -> PortResult<Vec<String>> {
        let (owner, name) = split_repo(&repo.full_name)?;
        let n = number.to_string();
        let mut url = self
            .app
            .url(&["repos", owner, name, "pulls", &n, "files"])?;
        url.query_pairs_mut().append_pair("per_page", "100");
        let files = self
            .app
            .pages_up_to(&repo.full_name, url, None, max)
            .await?;
        Ok(unique_paths(
            files
                .iter()
                .flat_map(|f| [f["filename"].as_str(), f["previous_filename"].as_str()])
                .flatten(),
        ))
    }

    pub(super) async fn signals_of(
        &self,
        repo: &RepoRef,
        number: u64,
        since: Timestamp,
    ) -> PortResult<Vec<ChangeSignal>> {
        let (owner, name) = split_repo(&repo.full_name)?;
        let n = number.to_string();
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
        let comments = self.app.pages(&repo.full_name, comments, None).await?;
        let events = self.app.pages(&repo.full_name, events, None).await?;
        let mut signals: Vec<ChangeSignal> = comments
            .iter()
            .filter_map(comment)
            .chain(events.iter().filter_map(label))
            .filter(|s| s.at >= since)
            .collect();
        signals.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        let excess = signals.len().saturating_sub(MAX_SIGNALS);
        signals.drain(..excess);
        Ok(signals)
    }

    pub(super) async fn writer(&self, repo: &RepoRef, login: &str) -> PortResult<bool> {
        let reply = self
            .repo_call(
                &repo.full_name,
                Method::GET,
                &["collaborators", login, "permission"],
                None,
            )
            .await?;
        match reply.status {
            StatusCode::NOT_FOUND => Ok(false),
            s if s.is_success() => {
                let role = reply.body["role_name"]
                    .as_str()
                    .or_else(|| reply.body["permission"].as_str())
                    .unwrap_or_default();
                Ok(writes(role))
            }
            _ => Err(self.app.unexpected("can_write", &reply)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use saka_ports::MAX_CHANGE_TEXT_BYTES;

    fn pr() -> Value {
        json!({
            "number": 7, "title": "Add retries", "body": null, "html_url": "https://github.com/acme/app/pull/7",
            "state": "open", "draft": true, "updated_at": "2026-11-04T10:00:00Z",
            "user": { "login": "alice" },
            "labels": [{ "name": "needs-review" }, { "name": "docs" }],
            "head": { "ref": "feature/retries", "sha": "a".repeat(40), "repo": { "full_name": "acme/app" } },
            "base": { "ref": "main", "repo": { "full_name": "acme/app" } },
        })
    }

    #[test]
    fn a_pull_request_becomes_a_summary_without_provider_words() {
        let s = summary(&pr()).unwrap();
        assert_eq!(s.number, 7);
        assert_eq!(
            (s.title.as_str(), s.body.as_str(), s.author.as_str()),
            ("Add retries", "", "alice")
        );
        assert_eq!(s.labels, ["needs-review", "docs"]);
        assert!(s.draft && !s.from_fork);
        assert_eq!(s.state, ChangeState::Open);
        assert_eq!(s.updated_at, Timestamp(1_793_786_400_000));
        assert_eq!(
            (s.head_branch.as_str(), s.base_branch.as_str()),
            ("feature/retries", "main")
        );
    }

    #[test]
    fn a_head_repo_that_differs_or_is_gone_is_a_fork() {
        let mut other = pr();
        other["head"]["repo"]["full_name"] = json!("mallory/app");
        assert!(summary(&other).unwrap().from_fork);
        let mut gone = pr();
        gone["head"]["repo"] = Value::Null;
        assert!(summary(&gone).unwrap().from_fork);
    }

    #[test]
    fn merged_and_closed_pull_requests_have_their_own_state() {
        let mut merged = pr();
        merged["state"] = json!("closed");
        merged["merged_at"] = json!("2026-11-04T11:00:00Z");
        assert_eq!(summary(&merged).unwrap().state, ChangeState::Merged);
        let mut closed = pr();
        closed["state"] = json!("closed");
        assert_eq!(summary(&closed).unwrap().state, ChangeState::Closed);
    }

    #[test]
    fn a_pull_request_without_a_number_or_head_is_retried_not_guessed() {
        let mut broken = pr();
        broken["head"]["sha"] = Value::Null;
        assert!(summary(&broken).unwrap_err().retryable);
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        let mut long = pr();
        long["body"] = json!("é".repeat(MAX_CHANGE_TEXT_BYTES));
        let body = summary(&long).unwrap().body;
        assert!(body.len() <= MAX_CHANGE_TEXT_BYTES);
        assert!(body.chars().all(|c| c == 'é'));
    }

    #[test]
    fn comments_and_label_events_become_signals_with_their_actor() {
        let c = comment(&json!({
            "id": 11, "created_at": "2026-11-04T10:00:00Z", "user": { "login": "bob" }, "body": "/saka review",
        }))
        .unwrap();
        assert_eq!(c.id, "comment:11");
        assert_eq!(c.actor, "bob");
        assert_eq!(
            c.kind,
            SignalKind::Comment {
                body: "/saka review".into()
            }
        );
        let l = label(&json!({
            "id": 12, "event": "labeled", "created_at": "2026-11-04T10:01:00Z",
            "actor": { "login": "carol" }, "label": { "name": "saka:review" },
        }))
        .unwrap();
        assert_eq!(l.id, "label:12");
        assert_eq!(
            l.kind,
            SignalKind::Label {
                name: "saka:review".into()
            }
        );
        assert!(
            label(
                &json!({ "id": 13, "event": "unlabeled", "created_at": "2026-11-04T10:02:00Z",
            "actor": { "login": "carol" }, "label": { "name": "x" } })
            )
            .is_none()
        );
    }

    #[test]
    fn only_admin_maintain_and_write_roles_can_write() {
        for role in ["admin", "maintain", "write"] {
            assert!(writes(role), "{role}");
        }
        for role in ["triage", "read", "none", ""] {
            assert!(!writes(role), "{role}");
        }
    }
}
