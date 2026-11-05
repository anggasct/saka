use saka_core::time::Timestamp;
use saka_core::vocab::ChangeState;
use saka_ports::{
    ChangeSignal, ChangeSummary, PortError, PortResult, RepoRef, SignalKind, clip_text,
    unique_paths,
};
use serde_json::json;

use super::{BASE_BRANCH, FakeWorld, Pr, State};
use saka_ports::Clock;

pub const BOT_AUTHOR: &str = "saka[bot]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrMeta {
    pub author: String,
    pub labels: Vec<String>,
    pub draft: bool,
    pub fork: bool,
    pub files: Vec<String>,
    pub updated_at: Timestamp,
}

impl PrMeta {
    pub fn by_saka(at: Timestamp) -> PrMeta {
        PrMeta {
            author: BOT_AUTHOR.to_string(),
            labels: Vec::new(),
            draft: false,
            fork: false,
            files: Vec::new(),
            updated_at: at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSpec {
    pub title: String,
    pub body: String,
    pub author: String,
    pub branch: String,
    pub labels: Vec<String>,
    pub draft: bool,
    pub fork: bool,
    pub files: Vec<String>,
}

impl ChangeSpec {
    pub fn new(title: &str, author: &str, branch: &str) -> ChangeSpec {
        ChangeSpec {
            title: title.to_string(),
            body: String::new(),
            author: author.to_string(),
            branch: branch.to_string(),
            labels: Vec::new(),
            draft: false,
            fork: false,
            files: vec!["src/lib.rs".to_string()],
        }
    }

    #[must_use]
    pub fn body(mut self, body: &str) -> ChangeSpec {
        self.body = body.to_string();
        self
    }

    #[must_use]
    pub fn labels(mut self, labels: &[&str]) -> ChangeSpec {
        self.labels = labels.iter().map(|l| (*l).to_string()).collect();
        self
    }

    #[must_use]
    pub fn draft(mut self) -> ChangeSpec {
        self.draft = true;
        self
    }

    #[must_use]
    pub fn fork(mut self) -> ChangeSpec {
        self.fork = true;
        self
    }

    #[must_use]
    pub fn files(mut self, files: &[&str]) -> ChangeSpec {
        self.files = files.iter().map(|f| (*f).to_string()).collect();
        self
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct Signals {
    pub(super) all: Vec<(String, u64, ChangeSignal)>,
    pub(super) next: u64,
    pub(super) writers: std::collections::BTreeSet<String>,
}

impl State {
    fn pr_mut(&mut self, repo: &str, number: u64) -> Option<&mut Pr> {
        self.prs
            .iter_mut()
            .find(|p| p.repo == repo && p.number == number)
    }

    fn summary_of(&mut self, pr: &Pr) -> ChangeSummary {
        let head = pr
            .merged_head
            .clone()
            .or_else(|| self.refs(&pr.repo).get(&pr.head_branch).cloned())
            .unwrap_or_default();
        ChangeSummary {
            number: pr.number,
            title: clip_text(&pr.title),
            body: clip_text(&pr.body),
            url: format!("https://scm.test/{}/pull/{}", pr.repo, pr.number),
            head_branch: pr.head_branch.clone(),
            base_branch: pr.base_branch.clone(),
            head_sha: head,
            author: pr.meta.author.clone(),
            labels: pr.meta.labels.clone(),
            draft: pr.meta.draft,
            from_fork: pr.meta.fork,
            state: pr.state,
            updated_at: pr.meta.updated_at,
        }
    }
}

impl FakeWorld {
    pub fn open_external_change(&self, repo: &str, spec: ChangeSpec) -> u64 {
        let now = self.clock.now();
        let mut s = self.state();
        let base = s.refs(repo)[BASE_BRANCH].clone();
        let head = s.new_commit(vec![base]);
        s.refs(repo).insert(spec.branch.clone(), head);
        let number = s.prs.len() as u64 + 1;
        s.prs.push(Pr {
            repo: repo.to_string(),
            number,
            head_branch: spec.branch,
            base_branch: BASE_BRANCH.to_string(),
            title: spec.title,
            body: spec.body,
            state: ChangeState::Open,
            merge_commit_sha: None,
            merged_head: None,
            opened_with: spec.author.clone(),
            meta: PrMeta {
                author: spec.author,
                labels: spec.labels,
                draft: spec.draft,
                fork: spec.fork,
                files: spec.files,
                updated_at: now,
            },
        });
        number
    }

    pub fn push_to_change(&self, repo: &str, number: u64) -> String {
        let now = self.clock.now();
        let mut s = self.state();
        let branch = s
            .pr_mut(repo, number)
            .map(|p| {
                p.meta.updated_at = now;
                p.head_branch.clone()
            })
            .unwrap_or_default();
        let parent = s.refs(repo)[&branch].clone();
        let commit = s.new_commit(vec![parent]);
        s.refs(repo).insert(branch, commit.clone());
        commit
    }

    pub fn comment_on_change(&self, repo: &str, number: u64, author: &str, body: &str) {
        let now = self.clock.now();
        let mut s = self.state();
        s.signals.next += 1;
        let id = format!("comment:{}", s.signals.next);
        s.signals.all.push((
            repo.to_string(),
            number,
            ChangeSignal {
                id,
                at: now,
                actor: author.to_string(),
                kind: SignalKind::Comment {
                    body: body.to_string(),
                },
            },
        ));
        if let Some(pr) = s.pr_mut(repo, number) {
            pr.meta.updated_at = now;
        }
    }

    pub fn label_change(&self, repo: &str, number: u64, actor: &str, label: &str) {
        let now = self.clock.now();
        let mut s = self.state();
        s.signals.next += 1;
        let id = format!("label:{}", s.signals.next);
        s.signals.all.push((
            repo.to_string(),
            number,
            ChangeSignal {
                id,
                at: now,
                actor: actor.to_string(),
                kind: SignalKind::Label {
                    name: label.to_string(),
                },
            },
        ));
        if let Some(pr) = s.pr_mut(repo, number) {
            if !pr.meta.labels.iter().any(|l| l == label) {
                pr.meta.labels.push(label.to_string());
            }
            pr.meta.updated_at = now;
        }
    }

    pub fn set_write_access(&self, login: &str, writes: bool) {
        let mut s = self.state();
        if writes {
            s.signals.writers.insert(login.to_string());
        } else {
            s.signals.writers.remove(login);
        }
    }

    pub fn change_listings(&self) -> u32 {
        self.state().stats.change_listings
    }

    pub fn change_file_reads(&self) -> u32 {
        self.state().stats.change_file_reads
    }

    pub(super) fn listed(&self, repo: &RepoRef, max: usize) -> PortResult<Vec<ChangeSummary>> {
        let mut s = self.state();
        s.call("list_open_changes");
        s.stats.change_listings += 1;
        if s.change_reads_fail {
            return Err(PortError::retryable("scm_unavailable", json!({})));
        }
        let mut open: Vec<Pr> = s
            .prs
            .iter()
            .filter(|p| p.repo == repo.full_name && p.state == ChangeState::Open)
            .cloned()
            .collect();
        open.sort_by(|a, b| {
            b.meta
                .updated_at
                .cmp(&a.meta.updated_at)
                .then(b.number.cmp(&a.number))
        });
        open.truncate(max);
        Ok(open.iter().map(|p| s.summary_of(p)).collect())
    }

    pub(super) fn summarized(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSummary> {
        let mut s = self.state();
        s.call("change_summary");
        if s.change_reads_fail {
            return Err(PortError::retryable("scm_unavailable", json!({})));
        }
        let pr = s
            .prs
            .iter()
            .find(|p| p.repo == repo.full_name && p.number == number)
            .cloned()
            .ok_or_else(|| {
                PortError::permanent(
                    "scm_repo_inaccessible",
                    json!({ "repo": repo.full_name, "status": 404 }),
                )
            })?;
        Ok(s.summary_of(&pr))
    }

    pub(super) fn files_changed(
        &self,
        repo: &RepoRef,
        number: u64,
        max: usize,
    ) -> PortResult<Vec<String>> {
        let mut s = self.state();
        s.call("change_files");
        s.stats.change_file_reads += 1;
        if s.change_reads_fail {
            return Err(PortError::retryable("scm_unavailable", json!({})));
        }
        let files = s
            .prs
            .iter()
            .find(|p| p.repo == repo.full_name && p.number == number)
            .map(|p| p.meta.files.clone())
            .unwrap_or_default();
        let mut unique = unique_paths(files.iter().map(String::as_str));
        unique.truncate(max);
        Ok(unique)
    }

    pub(super) fn signals_since(
        &self,
        repo: &RepoRef,
        number: u64,
        since: Timestamp,
    ) -> PortResult<Vec<ChangeSignal>> {
        let mut s = self.state();
        s.call("change_signals");
        if s.change_reads_fail {
            return Err(PortError::retryable("scm_unavailable", json!({})));
        }
        let mut found: Vec<ChangeSignal> = s
            .signals
            .all
            .iter()
            .filter(|(r, n, sig)| *r == repo.full_name && *n == number && sig.at >= since)
            .map(|(_, _, sig)| sig.clone())
            .collect();
        found.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        let excess = found.len().saturating_sub(saka_ports::MAX_SIGNALS);
        found.drain(..excess);
        Ok(found)
    }

    pub(super) fn writes(&self, login: &str) -> PortResult<bool> {
        let mut s = self.state();
        s.call("can_write");
        if s.change_reads_fail {
            return Err(PortError::retryable("scm_unavailable", json!({})));
        }
        Ok(s.signals.writers.contains(login))
    }
}
