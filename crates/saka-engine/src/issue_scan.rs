use std::collections::{BTreeMap, BTreeSet};

use saka_core::external;
use saka_core::ids::{IssueIntakeId, ProjectId, RepoId};
use saka_core::issue_intake::{Input, IssueIntake};
use saka_core::job::{Job, JobKind};
use saka_core::rejection::Rejection;
use saka_core::time::Timestamp;
use saka_core::vocab::{IssueCause, IssueIntakeState, IssueTrigger, SpecState, WorkItemState};
use saka_core::workflow::{IssueCandidate, IssueEntry};
use saka_ports::{IssueRef, IssueSignal, IssueSignalKind, IssueState, IssueSummary, RepoRef};
use saka_store::issue_intake as rows;
use saka_store::{Batch, ChangeCursorRow, IssueScanCursorRow, TaskSet, read};
use serde_json::json;

use crate::issue_intake::Incoming;
use crate::worker::{Effect, Execution, internal};
use crate::{Cause, Engine, EngineError, Result};

const SCAN_ACTOR: &str = "system:issue_scan";
const OVERLAP_MS: i64 = 60_000;
const RECHECK_PER_SCAN: usize = 3;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueScanReport {
    pub listed: usize,
    pub accepted: usize,
    pub denied: usize,
    pub closed: usize,
}

pub(crate) fn issue_scan_job(
    project: ProjectId,
    repo: RepoId,
    now: Timestamp,
    period_ms: i64,
) -> Job {
    Job::new(
        JobKind::ScmScanIssues,
        format!("{project}:{repo}:{}", now.0.div_euclid(period_ms.max(1))),
        json!({ "project_id": project, "repo_id": repo }),
        now,
    )
}

fn is_new(signal: &IssueSignal, cursor: Option<&ChangeCursorRow>) -> bool {
    match cursor {
        None => true,
        Some(c) => {
            signal.at > c.through || (signal.at == c.through && !c.seen_ids.contains(&signal.id))
        }
    }
}

#[derive(Clone, Copy)]
struct Sighting<'a> {
    entry: &'a IssueEntry,
    repo: &'a RepoRef,
    summary: &'a IssueSummary,
    since: Timestamp,
    first_sight: bool,
}

enum Trigger {
    Asked { cause: IssueCause, actor: String },
    Denied,
    Nothing,
}

impl Engine {
    pub(crate) fn issue_scan_jobs(&self, now: Timestamp) -> Result<Vec<(ProjectId, Job)>> {
        let period = self.cfg.loop_cfg.poll_interval_ms;
        let projects = self.store.read(|c| {
            let ids = saka_store::list::project_ids(c)?;
            ids.into_iter()
                .filter_map(|id| read::load_project(c, id).transpose())
                .collect::<std::result::Result<Vec<_>, _>>()
        })?;
        let mut jobs = Vec::new();
        for p in projects.iter().filter(|p| p.workflow.issue.is_some()) {
            for r in &p.repos {
                jobs.push((p.id, issue_scan_job(p.id, r.repo_id, now, period)));
            }
        }
        Ok(jobs)
    }

    pub(crate) fn schedule_issue_scans(
        &self,
        batch: &mut Batch,
        repo_id: RepoId,
        row_id: uuid::Uuid,
        cause: &Cause,
    ) -> Result<u32> {
        let now = self.now();
        let projects = self.store.read(|c| {
            saka_store::list::projects_bound_to(c, &[repo_id], ProjectId(uuid::Uuid::nil()))
        })?;
        let mut scheduled = 0;
        for p in projects.iter().filter(|p| p.workflow.issue.is_some()) {
            let job = Job::new(
                JobKind::ScmScanIssues,
                format!("{}:{repo_id}:hook:{row_id}", p.id),
                json!({ "project_id": p.id, "repo_id": repo_id }),
                now,
            );
            self.push_job(batch, job, Some(p.id), cause);
            scheduled += 1;
        }
        Ok(scheduled)
    }

    pub(crate) async fn issue_scan_job(&self, job: &saka_store::ClaimedJob) -> Execution {
        let project: ProjectId = crate::worker::field(&job.payload, "project_id")?;
        let repo: RepoId = crate::worker::field(&job.payload, "repo_id")?;
        match self.scan_issues(project, repo).await {
            Ok(_) | Err(EngineError::Rejected(_)) => Ok(Effect::Nothing),
            Err(EngineError::Port(e)) => Err(e),
            Err(e) => Err(internal(e)),
        }
    }

    async fn trigger_of(
        &self,
        sighting: &Sighting<'_>,
        fresh: &[&IssueSignal],
        writers: &mut BTreeMap<String, bool>,
    ) -> Result<Trigger> {
        let Sighting {
            entry,
            repo,
            summary,
            since,
            first_sight,
        } = *sighting;
        let mut denied = false;
        for signal in fresh {
            let cause = match &signal.kind {
                IssueSignalKind::Labeled { name } if entry.is_trigger_label(name) => {
                    IssueCause::Label
                }
                IssueSignalKind::Comment { body } if external::asks_intake(body) => {
                    IssueCause::Comment
                }
                _ => continue,
            };
            if self.is_writer(repo, &signal.actor, writers).await? {
                return Ok(Trigger::Asked {
                    cause,
                    actor: signal.actor.clone(),
                });
            }
            denied = true;
            self.log_denied(repo, summary.number, &signal.actor);
        }
        if entry.trigger == IssueTrigger::Auto && first_sight && summary.created_at >= since {
            if self.is_writer(repo, &summary.author, writers).await? {
                return Ok(Trigger::Asked {
                    cause: IssueCause::Author,
                    actor: summary.author.clone(),
                });
            }
            denied = true;
            self.log_denied(repo, summary.number, &summary.author);
        }
        Ok(if denied {
            Trigger::Denied
        } else {
            Trigger::Nothing
        })
    }

    fn log_denied(&self, repo: &RepoRef, number: u64, actor: &str) {
        self.counters.issue_denied();
        tracing::info!(repo = %repo.full_name, number, actor, "intake.issue.denied");
    }

    async fn is_writer(
        &self,
        repo: &RepoRef,
        login: &str,
        writers: &mut BTreeMap<String, bool>,
    ) -> Result<bool> {
        if let Some(known) = writers.get(login) {
            return Ok(*known);
        }
        let writes = self.ports.intake.can_write(repo, login).await?;
        writers.insert(login.to_string(), writes);
        Ok(writes)
    }

    pub async fn scan_issues(
        &self,
        project_id: ProjectId,
        repo_id: RepoId,
    ) -> Result<IssueScanReport> {
        let started = self.now();
        let project = self
            .store
            .read(|c| read::load_project(c, project_id))?
            .ok_or(Rejection::NotFound)?;
        let Some(entry) = project.workflow.issue.clone() else {
            return Ok(IssueScanReport::default());
        };
        if !project.repos.iter().any(|r| r.repo_id == repo_id) {
            return Ok(IssueScanReport::default());
        }
        let repo = self.repo_ref_stored(repo_id)?;
        let (cursor, signal_cursors, live, open_count) = self.store.read(|c| {
            Ok((
                rows::issue_scan_cursor(c, project_id, repo_id)?,
                rows::issue_cursors(c, repo_id)?,
                rows::live_intakes_of_repo(c, project_id, repo_id)?,
                rows::open_issue_intakes(c, project_id)?,
            ))
        })?;
        let floor = cursor.unwrap_or_else(|| started.plus_ms(-OVERLAP_MS));
        let since = floor.plus_ms(-OVERLAP_MS);
        let listing = self
            .ports
            .intake
            .list_issues(&repo, since, &entry.labels)
            .await?;
        let cause = self.cause(SCAN_ACTOR);
        let mut report = IssueScanReport {
            listed: listing.issues.len(),
            ..IssueScanReport::default()
        };
        let mut batch = Batch::new(started);
        let mut writers: BTreeMap<String, bool> = BTreeMap::new();
        let mut open = open_count;
        let mut to_close: Vec<IssueIntakeId> = Vec::new();
        for summary in &listing.issues {
            if let Some(intake) = live.iter().find(|i| i.number == summary.number) {
                if summary.state == IssueState::Closed && is_before_approval(intake) {
                    to_close.push(intake.id);
                }
                continue;
            }
            let candidate = IssueCandidate {
                labels: &summary.labels,
                open: summary.state == IssueState::Open,
                confidential: summary.confidential,
            };
            if let Err(skip) = entry.screen(&candidate) {
                tracing::debug!(repo = %repo.full_name, number = summary.number, reason = skip.as_str(), "intake.issue.skipped");
                continue;
            }
            let known = signal_cursors.iter().find(|c| c.number == summary.number);
            if known.is_some_and(|c| c.updated_at >= summary.updated_at) {
                continue;
            }
            let signals = self
                .ports
                .intake
                .issue_signals(
                    &IssueRef {
                        repo: repo.clone(),
                        number: summary.number,
                    },
                    known.map_or(since, |c| c.through),
                )
                .await?;
            let fresh: Vec<&IssueSignal> = signals.iter().filter(|s| is_new(s, known)).collect();
            let trigger = self
                .trigger_of(
                    &Sighting {
                        entry: &entry,
                        repo: &repo,
                        summary,
                        since,
                        first_sight: known.is_none(),
                    },
                    &fresh,
                    &mut writers,
                )
                .await?;
            batch
                .issue_cursors
                .push(next_cursor(repo_id, summary, &fresh, known, since));
            match trigger {
                Trigger::Denied => report.denied += 1,
                Trigger::Nothing => {}
                Trigger::Asked { cause: why, actor } => {
                    if open >= entry.max_open {
                        self.push_limit_comment(
                            &mut batch,
                            &project,
                            repo_id,
                            summary.number,
                            entry.max_open,
                            &cause,
                        );
                        tracing::info!(repo = %repo.full_name, number = summary.number, "intake.issue.limit_reached");
                        continue;
                    }
                    self.accept_issue(
                        &mut batch,
                        &entry,
                        &project,
                        Incoming {
                            repo_id,
                            summary,
                            cause: why,
                            actor,
                        },
                        &cause,
                    )?;
                    open += 1;
                    report.accepted += 1;
                }
            }
        }
        to_close.extend(
            self.unlisted_closed(&repo, &live, &listing.issues, started)
                .await?,
        );
        batch.issue_scan_cursors.push(IssueScanCursorRow {
            project_id,
            repo_id,
            through: listing.resume_from.unwrap_or(started),
        });
        self.commit(&batch)?;
        let seen: BTreeSet<IssueIntakeId> = to_close.into_iter().collect();
        for id in seen {
            match self
                .retire_issue_intake(id, Input::IssueClosed, "issue closed", &cause)
                .await
            {
                Ok(()) => report.closed += 1,
                Err(EngineError::Rejected(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(report)
    }

    async fn unlisted_closed(
        &self,
        repo: &RepoRef,
        live: &[IssueIntake],
        listed: &[IssueSummary],
        now: Timestamp,
    ) -> Result<Vec<IssueIntakeId>> {
        let pending: Vec<&IssueIntake> = live
            .iter()
            .filter(|i| is_before_approval(i) && !listed.iter().any(|s| s.number == i.number))
            .collect();
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        let bucket = usize::try_from(now.0.div_euclid(self.cfg.loop_cfg.poll_interval_ms.max(1)))
            .unwrap_or(0);
        let mut closed = Vec::new();
        for offset in 0..RECHECK_PER_SCAN.min(pending.len()) {
            let intake = pending[(bucket + offset) % pending.len()];
            let issue = IssueRef {
                repo: repo.clone(),
                number: intake.number,
            };
            match self.ports.intake.issue(&issue).await {
                Ok(found) if found.summary.state == IssueState::Closed => closed.push(intake.id),
                Ok(_) => {}
                Err(e) if e.code == "issue_not_found" => closed.push(intake.id),
                Err(e) if e.retryable => return Err(e.into()),
                Err(_) => {}
            }
        }
        Ok(closed)
    }

    pub(crate) fn reconcile_issue_intakes(&self, cause: &Cause) -> Result<usize> {
        let live = self.store.read(rows::live_issue_intakes)?;
        let mut changed = 0;
        for intake in live {
            changed += self.reconcile_intake(&intake, cause)?;
        }
        Ok(changed)
    }

    fn reconcile_intake(&self, intake: &IssueIntake, cause: &Cause) -> Result<usize> {
        let input = match intake.state {
            IssueIntakeState::Drafting => self.drafting_input(intake)?,
            IssueIntakeState::AwaitingApproval => self.approval_input(intake)?,
            IssueIntakeState::Approved | IssueIntakeState::InProgress => {
                self.progress_input(intake, cause)?
            }
            _ => None,
        };
        let Some(input) = input else {
            return Ok(0);
        };
        let _guard = self.locked()?;
        let Some(fresh) = self.store.read(|c| rows::load_issue_intake(c, intake.id))? else {
            return Ok(0);
        };
        if fresh.state != intake.state {
            return Ok(0);
        }
        let mut batch = Batch::new(self.now());
        match self.issue_step(&mut batch, Some(&fresh), input, cause) {
            Ok(_) => {}
            Err(EngineError::Rejected(_)) => return Ok(0),
            Err(e) => return Err(e),
        }
        self.commit(&batch)?;
        Ok(1)
    }

    fn drafting_input(&self, intake: &IssueIntake) -> Result<Option<Input>> {
        let Some(draft_id) = intake.draft_id else {
            return Ok(None);
        };
        let Some(outcome) = self.store.read(|c| rows::draft_outcome(c, draft_id))? else {
            return Ok(None);
        };
        use saka_core::vocab::SpecDraftState as D;
        Ok(match (outcome.state, outcome.spec_id) {
            (D::Succeeded, Some(spec_id)) => {
                let ready = self
                    .store
                    .read(|c| read::load_spec(c, spec_id))?
                    .is_some_and(|s| matches!(s.state, SpecState::Ready | SpecState::Approved));
                ready.then_some(Input::SpecReady { spec_id })
            }
            (D::Failed, _) => Some(Input::Fail {
                code: "draft_failed".into(),
                params: json!({ "draft_failure": outcome.failure.map(|f| f.code) }),
            }),
            (D::Cancelled, _) => Some(Input::Fail {
                code: "draft_cancelled".into(),
                params: json!({}),
            }),
            _ => None,
        })
    }

    fn approval_input(&self, intake: &IssueIntake) -> Result<Option<Input>> {
        let Some(spec_id) = intake.spec_id else {
            return Ok(None);
        };
        let state = self
            .store
            .read(|c| read::load_spec(c, spec_id))?
            .map(|s| s.state);
        Ok(match state {
            Some(SpecState::Approved | SpecState::Shipped) => Some(Input::Approved),
            Some(SpecState::Withdrawn) => Some(Input::Dismiss {
                reason: Some("spec withdrawn".into()),
            }),
            _ => None,
        })
    }

    fn progress_input(&self, intake: &IssueIntake, cause: &Cause) -> Result<Option<Input>> {
        let Some(spec_id) = intake.spec_id else {
            return Ok(None);
        };
        let (spec, tasks) = self.store.read(|c| {
            Ok((
                read::load_spec(c, spec_id)?,
                read::load_tasks(c, TaskSet::Spec(spec_id), &self.cfg.workflow_policy)?,
            ))
        })?;
        if let Some(url) = tasks
            .iter()
            .filter(|t| t.item.state != WorkItemState::Superseded)
            .filter_map(|t| {
                t.change_request
                    .as_ref()
                    .map(|cr| (t.item.step_index, cr.url.clone()))
            })
            .min_by_key(|(step, _)| *step)
            .map(|(_, url)| url)
            && !self.comment_job_exists(intake, "pull_request_opened")?
        {
            let mut batch = Batch::new(self.now());
            self.push_comment(
                &mut batch,
                intake,
                saka_core::issue_text::CommentEvent::PullRequestOpened,
                "pull_request_opened",
                Some(saka_core::issue_text::CommentFacts {
                    pull_request_url: Some(url),
                    ..saka_core::issue_text::CommentFacts::default()
                }),
                cause,
            )?;
            self.commit(&batch)?;
        }
        if spec.as_ref().is_some_and(|s| s.state == SpecState::Shipped) {
            return Ok(Some(Input::Shipped));
        }
        let started = tasks.iter().any(|t| {
            !matches!(
                t.item.state,
                WorkItemState::Queued | WorkItemState::AwaitingStart | WorkItemState::Superseded
            )
        });
        Ok((intake.state == IssueIntakeState::Approved && started).then_some(Input::TaskStarted))
    }
}

fn is_before_approval(intake: &IssueIntake) -> bool {
    matches!(
        intake.state,
        IssueIntakeState::Accepted
            | IssueIntakeState::Drafting
            | IssueIntakeState::AwaitingApproval
            | IssueIntakeState::Failed
    )
}

fn next_cursor(
    repo_id: RepoId,
    summary: &IssueSummary,
    fresh: &[&IssueSignal],
    known: Option<&ChangeCursorRow>,
    since: Timestamp,
) -> ChangeCursorRow {
    let through = fresh
        .iter()
        .map(|s| s.at)
        .max()
        .into_iter()
        .chain(known.map(|c| c.through))
        .max()
        .unwrap_or(since);
    let mut seen_ids: BTreeSet<String> = fresh
        .iter()
        .filter(|s| s.at == through)
        .map(|s| s.id.clone())
        .collect();
    if let Some(c) = known.filter(|c| c.through == through) {
        seen_ids.extend(c.seen_ids.iter().cloned());
    }
    ChangeCursorRow {
        repo_id,
        number: summary.number,
        updated_at: summary.updated_at,
        through,
        seen_ids: seen_ids.into_iter().collect(),
    }
}
