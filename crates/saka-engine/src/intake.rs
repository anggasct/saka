use std::collections::{BTreeMap, BTreeSet};

use saka_core::Ctx;
use saka_core::external::{self, Command, Incoming};
use saka_core::ids::{ProjectId, RepoId, SpecId};
use saka_core::job::Job;
use saka_core::model::{Project, SpecLink, Task};
use saka_core::rejection::Rejection;
use saka_core::task::{self, TaskInput};
use saka_core::time::Timestamp;
use saka_core::vocab::{ExternalCause, Stage, Trigger, WorkItemOrigin};
use saka_core::workflow::{Candidate, ChangeEntry, Skip};
use saka_ports::{
    ChangeSignal, ChangeSummary, IdsSource, MAX_CHANGE_FILES, MAX_OPEN_CHANGES, RepoRef, SignalKind,
};
use saka_store::{Batch, ChangeCursorRow, read};
use serde_json::json;

use crate::{Cause, Engine, EngineError, Extra, Mode, Result};

pub const SCAN_ACTOR: &str = "system:scan";
const PATH_FILTER_CACHE_MAX: usize = 2_000;

#[derive(Default)]
pub(crate) struct PathFilterCache(
    std::sync::Mutex<std::collections::HashMap<(RepoId, u64, String), bool>>,
);

impl PathFilterCache {
    fn get(&self, key: &(RepoId, u64, String)) -> Option<bool> {
        self.0.lock().ok()?.get(key).copied()
    }

    fn put(&self, key: (RepoId, u64, String), matched: bool) {
        if let Ok(mut map) = self.0.lock() {
            if map.len() >= PATH_FILTER_CACHE_MAX {
                map.clear();
            }
            map.insert(key, matched);
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub listed: usize,
    pub accepted: usize,
    pub requested: usize,
}

pub(crate) fn scan_job(project: ProjectId, repo: RepoId, now: Timestamp, period_ms: i64) -> Job {
    Job::new(
        saka_core::job::JobKind::ScmScanChanges,
        format!("{project}:{repo}:{}", now.0.div_euclid(period_ms.max(1))),
        json!({ "project_id": project, "repo_id": repo }),
        now,
    )
}

pub(crate) struct Taking<'a> {
    pub repo_name: &'a str,
    pub spec: Option<SpecLink>,
    pub cause: ExternalCause,
    pub actor: Option<String>,
}

struct Requested {
    cause: ExternalCause,
    actor: String,
}

struct Seen {
    command: Option<Requested>,
    cursor: ChangeCursorRow,
}

fn is_new(signal: &ChangeSignal, cursor: Option<&ChangeCursorRow>) -> bool {
    match cursor {
        None => true,
        Some(c) => {
            signal.at > c.through || (signal.at == c.through && !c.seen_ids.contains(&signal.id))
        }
    }
}

impl Engine {
    pub(crate) fn scan_jobs(&self, now: Timestamp) -> Result<Vec<(ProjectId, Job)>> {
        let period = self.cfg.loop_cfg.poll_interval_ms;
        let projects = self.store.read(|c| {
            let ids = saka_store::list::project_ids(c)?;
            ids.into_iter()
                .filter_map(|id| read::load_project(c, id).transpose())
                .collect::<std::result::Result<Vec<_>, _>>()
        })?;
        Ok(projects
            .into_iter()
            .filter(|p| p.workflow.change_request.is_some())
            .flat_map(|p| {
                p.repos
                    .iter()
                    .map(|r| (p.id, scan_job(p.id, r.repo_id, now, period)))
                    .collect::<Vec<_>>()
            })
            .collect())
    }

    pub(crate) fn repo_ref_stored(&self, repo_id: RepoId) -> Result<RepoRef> {
        let repo = self
            .store
            .read(|c| read::load_repo(c, repo_id))?
            .ok_or_else(|| EngineError::Internal("scan of a repo that is not stored".into()))?;
        Ok(RepoRef {
            repo_id: repo.id,
            integration_id: repo.integration_id,
            full_name: repo.full_name,
            url: repo.url,
        })
    }

    pub async fn scan_changes(&self, project_id: ProjectId, repo_id: RepoId) -> Result<ScanReport> {
        let started = std::time::Instant::now();
        let project = self
            .store
            .read(|c| read::load_project(c, project_id))?
            .ok_or(Rejection::NotFound)?;
        let (Some(entry), Some(binding)) = (
            project.workflow.change_request.clone(),
            project.repos.iter().find(|r| r.repo_id == repo_id).cloned(),
        ) else {
            return Ok(ScanReport::default());
        };
        let repo = self.repo_ref_stored(repo_id)?;
        let listed = self
            .ports
            .scm
            .list_open_changes(&repo, MAX_OPEN_CHANGES)
            .await?;
        let (tracked, branches, cursors, specs) = self.store.read(|c| {
            Ok((
                read::tracked_changes(c, repo_id)?,
                read::spec_step_branches(c, repo_id)?,
                read::change_cursors(c, repo_id)?,
                read::project_specs(c, project_id)?,
            ))
        })?;
        let cause = self.cause(SCAN_ACTOR);
        let mut report = ScanReport {
            listed: listed.len(),
            ..ScanReport::default()
        };
        let mut writers: BTreeMap<String, bool> = BTreeMap::new();
        let mut written = Batch::new(self.now());
        let review = project.workflow.stage_trigger(Stage::Review);
        for change in &listed {
            let known = tracked.iter().find(|t| t.number == change.number);
            let skip = |reason: &str| {
                self.counters.intake_change(reason);
                tracing::debug!(repo = %repo.full_name, number = change.number, reason, "intake.skipped");
            };
            match known {
                Some(t) if t.origin == WorkItemOrigin::SpecStep => {
                    skip("skipped_owned");
                    continue;
                }
                Some(t) if t.state.is_terminal() => {
                    skip("skipped_tracked");
                    continue;
                }
                None if !change.from_fork && branches.contains(&change.head_branch) => {
                    skip("skipped_owned");
                    continue;
                }
                _ => {}
            }
            let manual_entry = known.is_none() && entry.trigger == Trigger::Manual;
            let manual_review = known.is_some() && review == Some(Trigger::Manual);
            if known.is_none() {
                let candidate = Candidate {
                    labels: &change.labels,
                    author: &change.author,
                    head_branch: &change.head_branch,
                    from_fork: change.from_fork,
                    draft: change.draft,
                };
                if let Err(why) = entry.screen(&candidate) {
                    skip(match why {
                        Skip::Fork => "skipped_fork",
                        Skip::Draft => "skipped_draft",
                        Skip::Label | Skip::Author | Skip::Branch => "skipped_filter",
                    });
                    continue;
                }
                if !self.path_filter_passes(&repo, change, &entry).await? {
                    skip("skipped_filter");
                    continue;
                }
            }
            let mut command = None;
            if manual_entry || manual_review {
                let cursor = cursors.iter().find(|c| c.number == change.number);
                let seen = self
                    .read_commands(&repo, change, &entry, cursor, &mut writers)
                    .await?;
                if let Some(seen) = seen {
                    command = seen.command;
                    written.change_cursors.push(seen.cursor);
                }
                if manual_entry && command.is_none() {
                    continue;
                }
            }
            let (cause_of, actor) = match &command {
                Some(c) => (c.cause, Some(c.actor.clone())),
                None => (ExternalCause::Auto, None),
            };
            match known {
                None => {
                    let link = self.spec_link(change, &specs)?;
                    let taking = Taking {
                        repo_name: &binding.name,
                        spec: link,
                        cause: cause_of,
                        actor: actor.clone(),
                    };
                    match self.accept_external(&project, &repo, change, taking) {
                        Ok(_) => {
                            report.accepted += 1;
                            self.counters.intake_change("accepted");
                            tracing::info!(
                                project_id = %project_id,
                                repo = %repo.full_name,
                                number = change.number,
                                cause = cause_of.as_str(),
                                "intake.accepted"
                            );
                            if cause_of != ExternalCause::Auto || review == Some(Trigger::Auto) {
                                report.requested += 1;
                                self.counters.review_request(cause_of.as_str());
                            }
                        }
                        Err(EngineError::Rejected(r)) => {
                            tracing::warn!(
                                code = r.code(),
                                number = change.number,
                                "intake.refused"
                            );
                        }
                        Err(e) => return Err(e),
                    }
                }
                Some(t) => {
                    let Some(command) = command else { continue };
                    let events = self.decide(
                        t.work_item_id,
                        Some(TaskInput::ReviewRequested {
                            head_sha: change.head_sha.clone(),
                            cause: command.cause,
                            actor: Some(command.actor),
                        }),
                        Mode::Fact,
                        &cause,
                        Extra::default(),
                    )?;
                    if !events.is_empty() {
                        report.requested += 1;
                        self.counters.review_request(cause_of.as_str());
                    }
                }
            }
        }
        written
            .open_changes_seen
            .push((repo_id, listed.iter().map(|c| c.number).collect()));
        self.commit(&written)?;
        self.counters.intake_scan(started.elapsed().as_secs_f64());
        Ok(report)
    }

    async fn path_filter_passes(
        &self,
        repo: &RepoRef,
        change: &ChangeSummary,
        entry: &ChangeEntry,
    ) -> Result<bool> {
        if !entry.needs_files() {
            return Ok(true);
        }
        let key = (repo.repo_id, change.number, change.head_sha.clone());
        if let Some(matched) = self.path_filter.get(&key) {
            return Ok(matched);
        }
        let files = self
            .ports
            .scm
            .change_files(repo, change.number, MAX_CHANGE_FILES)
            .await?;
        let matched = entry.touches(&files);
        self.path_filter.put(key, matched);
        Ok(matched)
    }

    async fn read_commands(
        &self,
        repo: &RepoRef,
        change: &ChangeSummary,
        entry: &ChangeEntry,
        cursor: Option<&ChangeCursorRow>,
        writers: &mut BTreeMap<String, bool>,
    ) -> Result<Option<Seen>> {
        if cursor.is_some_and(|c| c.updated_at >= change.updated_at) {
            return Ok(None);
        }
        let since = cursor.map_or(Timestamp(0), |c| c.through);
        let signals = self
            .ports
            .scm
            .change_signals(repo, change.number, since)
            .await?;
        let fresh: Vec<&ChangeSignal> = signals.iter().filter(|s| is_new(s, cursor)).collect();
        let mut command = None;
        for signal in &fresh {
            let (cause, wanted) = match &signal.kind {
                SignalKind::Comment { body } => {
                    let commands = external::parse_commands(body);
                    for other in commands.iter().filter(|c| **c != Command::Review) {
                        tracing::info!(
                            repo = %repo.full_name,
                            number = change.number,
                            command = other.stage().as_str(),
                            "intake.command.unavailable"
                        );
                    }
                    (ExternalCause::Comment, commands.contains(&Command::Review))
                }
                SignalKind::Label { name } => (
                    ExternalCause::Label,
                    name.eq_ignore_ascii_case(&entry.trigger_label),
                ),
            };
            if !wanted || command.is_some() {
                continue;
            }
            let writes = match writers.get(&signal.actor) {
                Some(w) => *w,
                None => {
                    let w = self.ports.scm.can_write(repo, &signal.actor).await?;
                    writers.insert(signal.actor.clone(), w);
                    w
                }
            };
            if writes {
                command = Some(Requested {
                    cause,
                    actor: signal.actor.clone(),
                });
            } else {
                self.counters.intake_change("command_denied");
                tracing::info!(
                    repo = %repo.full_name,
                    number = change.number,
                    actor = %signal.actor,
                    "intake.command.denied"
                );
            }
        }
        let through = fresh
            .iter()
            .map(|s| s.at)
            .max()
            .into_iter()
            .chain(cursor.map(|c| c.through))
            .max()
            .unwrap_or(since);
        let mut seen_ids: BTreeSet<String> = fresh
            .iter()
            .filter(|s| s.at == through)
            .map(|s| s.id.clone())
            .collect();
        if let Some(c) = cursor.filter(|c| c.through == through) {
            seen_ids.extend(c.seen_ids.iter().cloned());
        }
        Ok(Some(Seen {
            command,
            cursor: ChangeCursorRow {
                repo_id: repo.repo_id,
                number: change.number,
                updated_at: change.updated_at,
                through,
                seen_ids: seen_ids.into_iter().collect(),
            },
        }))
    }

    pub(crate) fn spec_link(
        &self,
        change: &ChangeSummary,
        specs: &[saka_core::model::Spec],
    ) -> Result<Option<SpecLink>> {
        let slugs: Vec<(SpecId, String)> = specs.iter().map(|s| (s.id, s.slug.clone())).collect();
        let text = format!("{}\n{}", change.title, change.body);
        let steps: BTreeMap<SpecId, BTreeSet<u32>> = self.store.read(|c| {
            let mut out = BTreeMap::new();
            for s in specs {
                if text.contains(&s.slug)
                    && let Some(view) = read::revision_view(c, s.latest_revision_id)?
                {
                    out.insert(s.id, view.steps.iter().map(|p| p.index).collect());
                }
            }
            Ok(out)
        })?;
        Ok(external::link_spec(&text, &slugs, |spec, n| {
            steps.get(&spec).is_some_and(|s| s.contains(&n))
        }))
    }

    pub(crate) fn accept_external(
        &self,
        project: &Project,
        repo: &RepoRef,
        change: &ChangeSummary,
        taking: Taking<'_>,
    ) -> Result<Task> {
        let Taking {
            repo_name,
            spec,
            cause: cause_of,
            actor,
        } = taking;
        let _guard = self.locked()?;
        let now = self.now();
        let tracked = self
            .store
            .read(|c| read::tracked_changes(c, repo.repo_id))?;
        if let Some(t) = tracked.iter().find(|t| t.number == change.number) {
            return Err(Rejection::ChangeRequestTracked {
                work_item_id: t.work_item_id.0.to_string(),
            }
            .into());
        }
        let incoming = Incoming {
            project_id: project.id,
            repo_id: repo.repo_id,
            repo_name: repo_name.to_string(),
            number: change.number,
            title: change.title.clone(),
            url: change.url.clone(),
            head_branch: change.head_branch.clone(),
            base_branch: change.base_branch.clone(),
            head_sha: change.head_sha.clone(),
            author: change.author.clone(),
            from_fork: change.from_fork,
            draft: change.draft,
            spec,
        };
        let mut ids = IdsSource(&*self.ports.ids);
        let mut ctx = Ctx { now, ids: &mut ids };
        let events = external::accept(&project.workflow, incoming, cause_of, actor, &mut ctx)?;
        let cause = self.cause(SCAN_ACTOR);
        let mut batch = Batch::new(now);
        let mut current: Option<Task> = None;
        for e in &events {
            current = task::evolve(current, e, now);
            let Some(t) = &current else {
                return Err(EngineError::Internal(
                    "accepted change lost its task".into(),
                ));
            };
            for job in saka_core::react::react(t, e, &self.cfg.loop_cfg, now) {
                self.push_job(&mut batch, job, Some(project.id), &cause);
            }
            self.push_event(&mut batch, e, Some(project.id), &cause, now);
        }
        let mut created =
            current.ok_or_else(|| EngineError::Internal("accepted change made no task".into()))?;
        batch.tasks.push(created.clone());
        self.commit(&batch)?;
        let slug = created.external.as_ref().and_then(|x| {
            let link = x.spec.as_ref()?;
            self.store
                .read(|c| read::load_spec(c, link.spec_id))
                .ok()?
                .map(|s| s.slug)
        });
        created.external.iter_mut().for_each(|x| {
            x.workflow = project.workflow.clone();
            x.spec_slug = slug.clone();
        });
        Ok(created)
    }

    pub(crate) fn schedule_scans(
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
        for p in projects
            .iter()
            .filter(|p| p.workflow.change_request.is_some())
        {
            let job = Job::new(
                saka_core::job::JobKind::ScmScanChanges,
                format!("{}:{repo_id}:hook:{row_id}", p.id),
                json!({ "project_id": p.id, "repo_id": repo_id }),
                now,
            );
            self.push_job(batch, job, Some(p.id), cause);
            scheduled += 1;
        }
        Ok(scheduled)
    }
}
