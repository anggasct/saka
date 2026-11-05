use reqwest::{Method, StatusCode};
use saka_core::time::Timestamp;
use saka_core::vocab::ChangeState;
use saka_ports::{
    ChangeSignal, ChangeSummary, MAX_SIGNALS, PortError, PortResult, RepoRef, SignalKind,
    clip_text, unique_paths,
};
use serde_json::{Value, json};

use super::GitLab;
use super::{head_of, text};
use crate::github::app::parse_time;

const DEVELOPER: u64 = 30;
const MAX_LABEL_EVENTS: usize = 2000;

fn incomplete() -> PortError {
    PortError::retryable(
        "scm_unavailable",
        json!({ "detail": "merge_request_incomplete" }),
    )
}

fn clipped(value: &Value) -> String {
    clip_text(value.as_str().unwrap_or_default())
}

pub(super) fn summary(mr: &Value) -> PortResult<ChangeSummary> {
    let (Some(number), Some(head_sha)) = (mr["iid"].as_u64().filter(|n| *n >= 1), head_of(mr))
    else {
        return Err(incomplete());
    };
    let state = match mr["state"].as_str() {
        Some("merged") => ChangeState::Merged,
        Some("closed" | "locked") => ChangeState::Closed,
        _ => ChangeState::Open,
    };
    Ok(ChangeSummary {
        number,
        title: text(&mr["title"]).unwrap_or_default(),
        body: clipped(&mr["description"]),
        url: text(&mr["web_url"]).unwrap_or_default(),
        head_branch: text(&mr["source_branch"]).unwrap_or_default(),
        base_branch: text(&mr["target_branch"]).unwrap_or_default(),
        head_sha,
        author: text(&mr["author"]["username"]).unwrap_or_default(),
        labels: mr["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l.as_str().map(String::from))
            .collect(),
        draft: mr["draft"]
            .as_bool()
            .or_else(|| mr["work_in_progress"].as_bool())
            .unwrap_or(false),
        from_fork: mr["source_project_id"] != mr["target_project_id"],
        state,
        updated_at: parse_time(&mr["updated_at"]).unwrap_or_default(),
    })
}

fn note(item: &Value) -> Option<ChangeSignal> {
    (item["system"] != true).then_some(())?;
    Some(ChangeSignal {
        id: format!("comment:{}", item["id"].as_u64()?),
        at: parse_time(&item["created_at"])?,
        actor: item["author"]["username"].as_str()?.to_string(),
        kind: SignalKind::Comment {
            body: clipped(&item["body"]),
        },
    })
}

fn label_event(item: &Value) -> Option<ChangeSignal> {
    (item["action"] == "add").then_some(())?;
    Some(ChangeSignal {
        id: format!("label:{}", item["id"].as_u64()?),
        at: parse_time(&item["created_at"])?,
        actor: item["user"]["username"].as_str()?.to_string(),
        kind: SignalKind::Label {
            name: item["label"]["name"].as_str()?.to_string(),
        },
    })
}

impl GitLab {
    pub(super) async fn open_changes(
        &self,
        repo: &RepoRef,
        max: usize,
    ) -> PortResult<Vec<ChangeSummary>> {
        let mut url = self
            .client
            .project_url(&repo.full_name, &["merge_requests"])?;
        url.query_pairs_mut()
            .append_pair("state", "opened")
            .append_pair("order_by", "updated_at")
            .append_pair("sort", "desc");
        let found = self.client.pages_up_to(&repo.full_name, url, max).await?;
        found.iter().map(summary).collect()
    }

    pub(super) async fn summary_of(
        &self,
        repo: &RepoRef,
        number: u64,
    ) -> PortResult<ChangeSummary> {
        summary(&self.merge_request(&repo.full_name, number).await?.body)
    }

    pub(super) async fn files_of(
        &self,
        repo: &RepoRef,
        number: u64,
        max: usize,
    ) -> PortResult<Vec<String>> {
        let n = number.to_string();
        let url = self
            .client
            .project_url(&repo.full_name, &["merge_requests", &n, "diffs"])?;
        let diffs = self.client.pages_up_to(&repo.full_name, url, max).await?;
        Ok(unique_paths(
            diffs
                .iter()
                .flat_map(|d| [d["new_path"].as_str(), d["old_path"].as_str()])
                .flatten(),
        ))
    }

    pub(super) async fn signals_of(
        &self,
        repo: &RepoRef,
        number: u64,
        since: Timestamp,
    ) -> PortResult<Vec<ChangeSignal>> {
        let n = number.to_string();
        let mut notes = self
            .client
            .project_url(&repo.full_name, &["merge_requests", &n, "notes"])?;
        notes
            .query_pairs_mut()
            .append_pair("order_by", "created_at")
            .append_pair("sort", "desc");
        let events = self.client.project_url(
            &repo.full_name,
            &["merge_requests", &n, "resource_label_events"],
        )?;
        let notes = self
            .client
            .pages_up_to(&repo.full_name, notes, MAX_SIGNALS)
            .await?;
        let events = self
            .client
            .pages_up_to(&repo.full_name, events, MAX_LABEL_EVENTS)
            .await?;
        let mut signals: Vec<ChangeSignal> = notes
            .iter()
            .filter_map(note)
            .chain(events.iter().filter_map(label_event))
            .filter(|s| s.at >= since)
            .collect();
        signals.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        let excess = signals.len().saturating_sub(MAX_SIGNALS);
        signals.drain(..excess);
        Ok(signals)
    }

    pub(super) async fn writer(&self, repo: &RepoRef, login: &str) -> PortResult<bool> {
        let mut users = self.client.url(&["users"])?;
        users.query_pairs_mut().append_pair("username", login);
        let reply = self
            .client
            .call(&repo.full_name, Method::GET, users, None)
            .await?;
        if !reply.status.is_success() {
            return Err(self.client.unexpected("can_write", &reply));
        }
        let Some(id) = reply
            .body
            .as_array()
            .and_then(|users| users.iter().find(|u| u["username"] == login))
            .and_then(|u| u["id"].as_u64())
        else {
            return Ok(false);
        };
        let id = id.to_string();
        let member = self
            .project_call(&repo.full_name, Method::GET, &["members", "all", &id], None)
            .await?;
        match member.status {
            StatusCode::NOT_FOUND => Ok(false),
            s if s.is_success() => {
                Ok(member.body["access_level"].as_u64().unwrap_or(0) >= DEVELOPER)
            }
            _ => Err(self.client.unexpected("can_write", &member)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mr() -> Value {
        json!({
            "iid": 4, "title": "Add retries", "description": null, "web_url": "https://gitlab.test/acme/app/-/merge_requests/4",
            "state": "opened", "draft": false, "updated_at": "2026-11-04T10:00:00Z",
            "source_branch": "feature/retries", "target_branch": "main", "sha": "b".repeat(40),
            "author": { "username": "alice" }, "labels": ["needs-review"],
            "source_project_id": 9, "target_project_id": 9,
        })
    }

    #[test]
    fn a_merge_request_becomes_the_same_summary_as_a_pull_request() {
        let s = summary(&mr()).unwrap();
        assert_eq!((s.number, s.author.as_str()), (4, "alice"));
        assert_eq!(s.labels, ["needs-review"]);
        assert!(!s.draft && !s.from_fork);
        assert_eq!(s.state, ChangeState::Open);
        assert_eq!(s.head_sha, "b".repeat(40));
        assert_eq!(
            (s.head_branch.as_str(), s.base_branch.as_str()),
            ("feature/retries", "main")
        );
    }

    #[test]
    fn a_source_in_another_project_is_a_fork_and_states_map_to_the_neutral_ones() {
        let mut forked = mr();
        forked["source_project_id"] = json!(10);
        assert!(summary(&forked).unwrap().from_fork);
        for (gitlab, neutral) in [
            ("merged", ChangeState::Merged),
            ("closed", ChangeState::Closed),
            ("locked", ChangeState::Closed),
            ("opened", ChangeState::Open),
        ] {
            let mut m = mr();
            m["state"] = json!(gitlab);
            assert_eq!(summary(&m).unwrap().state, neutral, "{gitlab}");
        }
    }

    #[test]
    fn system_notes_and_label_removals_are_no_signals() {
        let human = json!({ "id": 1, "system": false, "created_at": "2026-11-04T10:00:00Z",
            "author": { "username": "bob" }, "body": "/saka review" });
        assert_eq!(note(&human).unwrap().actor, "bob");
        let system = json!({ "id": 2, "system": true, "created_at": "2026-11-04T10:00:00Z",
            "author": { "username": "bob" }, "body": "added ~label" });
        assert!(note(&system).is_none());
        let added = json!({ "id": 3, "action": "add", "created_at": "2026-11-04T10:00:00Z",
            "user": { "username": "carol" }, "label": { "name": "saka:review" } });
        assert_eq!(
            label_event(&added).unwrap().kind,
            SignalKind::Label {
                name: "saka:review".into()
            }
        );
        let removed = json!({ "id": 4, "action": "remove", "created_at": "2026-11-04T10:00:00Z",
            "user": { "username": "carol" }, "label": { "name": "saka:review" } });
        assert!(label_event(&removed).is_none());
    }
}
