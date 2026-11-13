use async_trait::async_trait;
use saka_core::time::Timestamp;
use saka_ports::{
    Clock, IntakeCapabilities, IntakeSource, Issue, IssueListing, IssueRef, IssueSignal,
    IssueSignalKind, IssueState, IssueSummary, MAX_ISSUES, MAX_SIGNALS, PortError, PortResult,
    RepoRef, check_comment_body, clip_issue_body,
};
use serde_json::json;

use super::{FakeWorld, PostedComment};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueSpec {
    pub title: String,
    pub body: String,
    pub author: String,
    pub labels: Vec<String>,
    pub confidential: bool,
}

impl IssueSpec {
    pub fn new(title: &str, author: &str) -> IssueSpec {
        IssueSpec {
            title: title.to_string(),
            body: String::new(),
            author: author.to_string(),
            labels: Vec::new(),
            confidential: false,
        }
    }

    #[must_use]
    pub fn body(mut self, body: &str) -> IssueSpec {
        self.body = body.to_string();
        self
    }

    #[must_use]
    pub fn labels(mut self, labels: &[&str]) -> IssueSpec {
        self.labels = labels.iter().map(|l| (*l).to_string()).collect();
        self
    }

    #[must_use]
    pub fn confidential(mut self) -> IssueSpec {
        self.confidential = true;
        self
    }
}

#[derive(Debug, Clone)]
pub(super) struct StoredIssue {
    repo: String,
    number: u64,
    spec: IssueSpec,
    state: IssueState,
    updated_at: Timestamp,
}

#[derive(Debug, Clone)]
pub(super) struct Issues {
    all: Vec<StoredIssue>,
    signals: Vec<(String, u64, IssueSignal)>,
    next_signal: u64,
    comments: Vec<PostedComment>,
    reads_fail: bool,
    limited_until: Option<Timestamp>,
    comments_refused: bool,
    confidential_issues: bool,
    listings: u32,
}

impl Default for Issues {
    fn default() -> Issues {
        Issues {
            all: Vec::new(),
            signals: Vec::new(),
            next_signal: 0,
            comments: Vec::new(),
            reads_fail: false,
            limited_until: None,
            comments_refused: false,
            confidential_issues: true,
            listings: 0,
        }
    }
}

impl Issues {
    fn issue_mut(&mut self, repo: &str, number: u64) -> Option<&mut StoredIssue> {
        self.all
            .iter_mut()
            .find(|i| i.repo == repo && i.number == number)
    }

    fn signal(
        &mut self,
        repo: &str,
        number: u64,
        at: Timestamp,
        actor: &str,
        kind: IssueSignalKind,
    ) {
        self.next_signal += 1;
        let prefix = match kind {
            IssueSignalKind::Comment { .. } => "comment",
            IssueSignalKind::Labeled { .. } => "label",
            IssueSignalKind::Closed | IssueSignalKind::Reopened => "state",
        };
        let id = format!("{prefix}:{}", self.next_signal);
        self.signals.push((
            repo.to_string(),
            number,
            IssueSignal {
                id,
                at,
                actor: actor.to_string(),
                kind,
            },
        ));
        if let Some(issue) = self.issue_mut(repo, number) {
            issue.updated_at = at;
        }
    }

    fn gate(&self, now: Timestamp) -> PortResult<()> {
        if let Some(until) = self.limited_until.filter(|until| now < *until) {
            let mut limited = PortError::retryable(
                "scm_unavailable",
                json!({ "status": 429, "detail": "rate_limited" }),
            );
            limited.retry_at = Some(until);
            return Err(limited);
        }
        if self.reads_fail {
            return Err(PortError::retryable("scm_unavailable", json!({})));
        }
        Ok(())
    }
}

impl StoredIssue {
    fn summary(&self) -> IssueSummary {
        IssueSummary {
            number: self.number,
            title: self.spec.title.clone(),
            url: format!("https://scm.test/{}/issues/{}", self.repo, self.number),
            author: self.spec.author.clone(),
            state: self.state,
            labels: self.spec.labels.clone(),
            confidential: self.spec.confidential,
            updated_at: self.updated_at,
        }
    }
}

impl FakeWorld {
    pub fn open_issue(&self, repo: &str, spec: IssueSpec) -> u64 {
        let now = self.clock.now();
        let mut s = self.state();
        let number = s.issues.all.iter().filter(|i| i.repo == repo).count() as u64 + 1;
        s.issues.all.push(StoredIssue {
            repo: repo.to_string(),
            number,
            spec,
            state: IssueState::Open,
            updated_at: now,
        });
        number
    }

    pub fn comment_on_issue(&self, repo: &str, number: u64, author: &str, body: &str) {
        let now = self.clock.now();
        let kind = IssueSignalKind::Comment {
            body: body.to_string(),
        };
        self.state().issues.signal(repo, number, now, author, kind);
    }

    pub fn label_issue(&self, repo: &str, number: u64, actor: &str, label: &str) {
        let now = self.clock.now();
        let mut s = self.state();
        if let Some(issue) = s.issues.issue_mut(repo, number)
            && !issue.spec.labels.iter().any(|l| l == label)
        {
            issue.spec.labels.push(label.to_string());
        }
        let kind = IssueSignalKind::Labeled {
            name: label.to_string(),
        };
        s.issues.signal(repo, number, now, actor, kind);
    }

    pub fn close_issue(&self, repo: &str, number: u64, actor: &str) {
        let now = self.clock.now();
        let mut s = self.state();
        if let Some(issue) = s.issues.issue_mut(repo, number) {
            issue.state = IssueState::Closed;
        }
        s.issues
            .signal(repo, number, now, actor, IssueSignalKind::Closed);
    }

    pub fn reopen_issue(&self, repo: &str, number: u64, actor: &str) {
        let now = self.clock.now();
        let mut s = self.state();
        if let Some(issue) = s.issues.issue_mut(repo, number) {
            issue.state = IssueState::Open;
        }
        s.issues
            .signal(repo, number, now, actor, IssueSignalKind::Reopened);
    }

    pub fn delete_issue(&self, repo: &str, number: u64) {
        self.state()
            .issues
            .all
            .retain(|i| !(i.repo == repo && i.number == number));
    }

    pub fn issue_comments(&self) -> Vec<PostedComment> {
        self.state().issues.comments.clone()
    }

    pub fn issue_listings(&self) -> u32 {
        self.state().issues.listings
    }

    pub fn fail_issue_reads(&self, fail: bool) {
        self.state().issues.reads_fail = fail;
    }

    pub fn rate_limit_issues(&self, until: Option<Timestamp>) {
        self.state().issues.limited_until = until;
    }

    pub fn refuse_issue_comments(&self, refuse: bool) {
        self.state().issues.comments_refused = refuse;
    }

    pub fn set_confidential_issues(&self, supported: bool) {
        self.state().issues.confidential_issues = supported;
    }
}

#[async_trait]
impl IntakeSource for FakeWorld {
    fn capabilities(&self, _integration_id: &str) -> Option<IntakeCapabilities> {
        Some(IntakeCapabilities {
            confidential_issues: self.state().issues.confidential_issues,
        })
    }

    async fn list_issues(
        &self,
        repo: &RepoRef,
        updated_since: Timestamp,
        labels: &[String],
    ) -> PortResult<IssueListing> {
        let now = self.clock.now();
        let mut s = self.state();
        s.call("list_issues");
        s.issues.listings += 1;
        s.issues.gate(now)?;
        let mut found: Vec<IssueSummary> = s
            .issues
            .all
            .iter()
            .filter(|i| i.repo == repo.full_name && i.updated_at >= updated_since)
            .filter(|i| labels.iter().all(|l| i.spec.labels.contains(l)))
            .map(StoredIssue::summary)
            .collect();
        found.sort_by(|a, b| {
            a.updated_at
                .cmp(&b.updated_at)
                .then(a.number.cmp(&b.number))
        });
        let more = found.len() > MAX_ISSUES;
        found.truncate(MAX_ISSUES);
        let resume_from = found.last().map(|i| i.updated_at).filter(|_| more);
        Ok(IssueListing {
            issues: found,
            resume_from,
        })
    }

    async fn issue(&self, issue: &IssueRef) -> PortResult<Issue> {
        let now = self.clock.now();
        let mut s = self.state();
        s.call("issue");
        s.issues.gate(now)?;
        let stored = s
            .issues
            .all
            .iter()
            .find(|i| i.repo == issue.repo.full_name && i.number == issue.number)
            .ok_or_else(|| {
                PortError::permanent("issue_not_found", json!({ "number": issue.number }))
            })?;
        Ok(Issue {
            summary: stored.summary(),
            body: clip_issue_body(&stored.spec.body),
        })
    }

    async fn issue_signals(
        &self,
        issue: &IssueRef,
        since: Timestamp,
    ) -> PortResult<Vec<IssueSignal>> {
        let now = self.clock.now();
        let mut s = self.state();
        s.call("issue_signals");
        s.issues.gate(now)?;
        let mut found: Vec<IssueSignal> = s
            .issues
            .signals
            .iter()
            .filter(|(r, n, sig)| {
                *r == issue.repo.full_name && *n == issue.number && sig.at >= since
            })
            .map(|(_, _, sig)| sig.clone())
            .collect();
        found.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        let excess = found.len().saturating_sub(MAX_SIGNALS);
        found.drain(..excess);
        Ok(found)
    }

    async fn can_write(&self, _repo: &RepoRef, login: &str) -> PortResult<bool> {
        self.writes(login)
    }

    async fn post_issue_comment(&self, issue: &IssueRef, body: &str) -> PortResult<()> {
        check_comment_body(body)?;
        let mut s = self.state();
        s.call("post_issue_comment");
        let known = s
            .issues
            .all
            .iter()
            .any(|i| i.repo == issue.repo.full_name && i.number == issue.number);
        if s.issues.comments_refused || !known {
            return Err(PortError::permanent(
                "comment_refused",
                json!({ "status": 403 }),
            ));
        }
        s.issues.comments.push(PostedComment {
            repo: issue.repo.full_name.clone(),
            number: issue.number,
            body: body.to_string(),
        });
        Ok(())
    }
}
