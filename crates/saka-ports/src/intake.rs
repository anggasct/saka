use async_trait::async_trait;
use saka_core::time::Timestamp;

use crate::{PortResult, RepoRef};

pub const MAX_ISSUE_BODY_BYTES: usize = saka_core::issue_text::MAX_BODY_BYTES;
pub const MAX_ISSUES: usize = 100;
pub const TRUNCATION_MARKER: &str = saka_core::issue_text::TRUNCATION_MARKER;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub repo: RepoRef,
    pub number: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueState {
    Open,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueCloseOutcome {
    Closed,
    AlreadyClosed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueSummary {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub state: IssueState,
    pub labels: Vec<String>,
    pub confidential: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub summary: IssueSummary,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueListing {
    pub issues: Vec<IssueSummary>,
    pub resume_from: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueSignalKind {
    Comment { body: String },
    Labeled { name: String },
    Closed,
    Reopened,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueSignal {
    pub id: String,
    pub at: Timestamp,
    pub actor: String,
    pub kind: IssueSignalKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntakeCapabilities {
    pub confidential_issues: bool,
}

pub fn clip_issue_body(text: &str) -> String {
    if text.len() <= MAX_ISSUE_BODY_BYTES {
        return text.to_string();
    }
    let room = MAX_ISSUE_BODY_BYTES - TRUNCATION_MARKER.len() - 1;
    let cut = (0..=room)
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    format!("{}\n{TRUNCATION_MARKER}", &text[..cut])
}

#[async_trait]
pub trait IntakeSource: Send + Sync {
    fn capabilities(&self, integration_id: &str) -> Option<IntakeCapabilities>;

    async fn list_issues(
        &self,
        repo: &RepoRef,
        updated_since: Timestamp,
        labels: &[String],
    ) -> PortResult<IssueListing>;

    async fn issue(&self, issue: &IssueRef) -> PortResult<Issue>;

    async fn issue_signals(
        &self,
        issue: &IssueRef,
        since: Timestamp,
    ) -> PortResult<Vec<IssueSignal>>;

    async fn can_write(&self, repo: &RepoRef, login: &str) -> PortResult<bool>;

    async fn post_issue_comment(&self, issue: &IssueRef, body: &str) -> PortResult<()>;

    async fn close_issue(&self, issue: &IssueRef) -> PortResult<IssueCloseOutcome>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_body_is_kept_as_it_is() {
        assert_eq!(clip_issue_body("steps to reproduce"), "steps to reproduce");
    }

    #[test]
    fn a_long_body_is_cut_on_a_character_boundary_and_marked() {
        let body = "é".repeat(MAX_ISSUE_BODY_BYTES);
        let clipped = clip_issue_body(&body);
        assert!(clipped.len() <= MAX_ISSUE_BODY_BYTES);
        assert!(clipped.ends_with(&format!("\n{TRUNCATION_MARKER}")));
        let kept = clipped.trim_end_matches(&format!("\n{TRUNCATION_MARKER}"));
        assert!(kept.chars().all(|c| c == 'é'));
    }

    #[test]
    fn a_body_of_exactly_the_limit_is_not_marked() {
        let body = "a".repeat(MAX_ISSUE_BODY_BYTES);
        assert_eq!(clip_issue_body(&body), body);
    }
}
