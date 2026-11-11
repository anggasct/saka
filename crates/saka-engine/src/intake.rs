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

use crate::action::{Action, Produced};
use crate::{Cause, Engine, EngineError, Extra, Mode, Result, Shows};

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
    pub by: Option<&'a Cause>,
    pub action: Option<&'a Action<'a>>,
}

struct Requested {
    cause: ExternalCause,
    actor: String,
}

struct Seen {
    command: Option<Requested>,
    fix: Option<Requested>,
    merge: Option<Requested>,
    cursor: ChangeCursorRow,
}

#[derive(Clone, Copy)]
struct Wanted {
    review: bool,
    fix: bool,
    merge: bool,
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
        let mut jobs = Vec::new();
        for p in &projects {
            for r in &p.repos {
                if self.scans(p, r.repo_id)? {
                    jobs.push((p.id, scan_job(p.id, r.repo_id, now, period)));
                }
            }
        }
        Ok(jobs)
    }

    fn scans(&self, project: &Project, repo_id: RepoId) -> Result<bool> {
        Ok(project.workflow.change_request.is_some()
            || (project.workflow.has_stage(Stage::Merge)
                && self
                    .store
                    .read(|c| read::repo_has_live_change(c, repo_id))?))
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
        let Some(binding) = project.repos.iter().find(|r| r.repo_id == repo_id).cloned() else {
            return Ok(ScanReport::default());
        };
        let entry = project.workflow.change_request.clone();
        let repo = self.repo_ref_stored(repo_id)?;
        let (tracked, branches, cursors, specs) = self.store.read(|c| {
            Ok((
                read::tracked_changes(c, repo_id)?,
                read::spec_step_branches(c, repo_id)?,
                read::change_cursors(c, repo_id)?,
                read::project_specs(c, project_id)?,
            ))
        })?;
        if entry.is_none() && tracked.iter().all(|t| t.state.is_terminal()) {
            return Ok(ScanReport::default());
        }
        let listed = self
            .ports
            .scm
            .list_open_changes(&repo, MAX_OPEN_CHANGES)
            .await?;
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
                Some(t) if t.origin == WorkItemOrigin::SpecStep && !t.state.is_terminal() => {
                    if let Some(asked) = self
                        .merge_asked(&repo, change, &cursors, &mut writers, &mut written)
                        .await?
                    {
                        self.merge_on_command(&repo, change, t.work_item_id, &asked, &mut written);
                    }
                    continue;
                }
                Some(t) if t.origin == WorkItemOrigin::SpecStep => {
                    skip("skipped_owned");
                    continue;
                }
                Some(t) if t.state.is_terminal() => {
                    skip("skipped_tracked");
                    continue;
                }
                None if !change.from_fork
                    && branches.iter().any(|(b, _)| *b == change.head_branch) =>
                {
                    skip("skipped_owned");
                    continue;
                }
                _ => {}
            }
            let Some(entry) = entry.as_ref() else {
                continue;
            };
            let manual_entry = known.is_none() && entry.trigger == Trigger::Manual;
            let manual_review = known.is_some() && review == Some(Trigger::Manual);
            let fixes = known.is_some() && project.workflow.has_stage(Stage::Fix);
            let merges = known.is_some();
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
                if !self.path_filter_passes(&repo, change, entry).await? {
                    skip("skipped_filter");
                    continue;
                }
            }
            let mut command = None;
            let mut fix = None;
            let mut merge = None;
            if manual_entry || manual_review || fixes || merges {
                let cursor = cursors.iter().find(|c| c.number == change.number);
                let wanted = Wanted {
                    review: manual_entry || manual_review,
                    fix: fixes,
                    merge: merges,
                };
                let seen = self
                    .read_commands(&repo, change, entry, cursor, &mut writers, wanted)
                    .await?;
                if let Some(seen) = seen {
                    command = seen.command;
                    fix = seen.fix;
                    merge = seen.merge;
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
                        by: None,
                        action: None,
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
                    if let Some(asked) = &fix {
                        self.fix_on_command(&repo, change.number, t.work_item_id, asked);
                    }
                    if let Some(asked) = &merge {
                        self.merge_on_command(&repo, change, t.work_item_id, asked, &mut written);
                    }
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
        wanted: Wanted,
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
        let mut fix = None;
        let mut merge = None;
        for signal in &fresh {
            let (cause, asks_review, asks_fix, asks_merge) = match &signal.kind {
                SignalKind::Comment { body } => {
                    let commands = external::parse_commands(body);
                    (
                        ExternalCause::Comment,
                        commands.contains(&Command::Review),
                        commands.contains(&Command::Fix),
                        commands.contains(&Command::Merge),
                    )
                }
                SignalKind::Label { name } => (
                    ExternalCause::Label,
                    name.eq_ignore_ascii_case(&entry.trigger_label),
                    false,
                    false,
                ),
            };
            let asks_review = asks_review && wanted.review && command.is_none();
            let asks_fix = asks_fix && wanted.fix && fix.is_none();
            let asks_merge = asks_merge && wanted.merge && merge.is_none();
            if !asks_review && !asks_fix && !asks_merge {
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
                let asked = || Requested {
                    cause,
                    actor: signal.actor.clone(),
                };
                if asks_review {
                    command = Some(asked());
                }
                if asks_fix {
                    fix = Some(asked());
                }
                if asks_merge {
                    merge = Some(asked());
                }
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
            fix,
            merge,
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
            by,
            action,
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
        let cause = by.cloned().unwrap_or_else(|| self.cause(SCAN_ACTOR));
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
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Taken(&created, true));
            batch.action = Some(action.record(
                self,
                &cause,
                Some(project.id),
                Some(created.item.id.0),
                reply,
            ));
        }
        self.commit(&batch)?;
        Ok(created)
    }

    pub async fn request_review_with(
        &self,
        id: saka_core::ids::WorkItemId,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<saka_core::event::Event>> {
        let task = self.load_task(id)?;
        let (Some(change), true) = (
            task.change_request.as_ref(),
            task.item.origin == WorkItemOrigin::ExternalChange,
        ) else {
            return Err(Rejection::Unsupported.into());
        };
        if !task.is_at_stage(Stage::Review) {
            return Err(Rejection::InvalidState {
                state: task.item.state.as_str().to_string(),
            }
            .into());
        }
        let repo = self.repo_ref_stored(task.item.repo_id)?;
        let summary = self.ports.scm.change_summary(&repo, change.number).await?;
        if summary.state != saka_core::vocab::ChangeState::Open {
            return Err(Rejection::ChangeRequestNotOpen {
                number: change.number,
                state: summary.state.as_str(),
            }
            .into());
        }
        let events = self.decide(
            id,
            Some(TaskInput::ReviewRequested {
                head_sha: summary.head_sha,
                cause: ExternalCause::Operator,
                actor: None,
            }),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Task)),
                ..Extra::default()
            },
        )?;
        self.counters
            .review_request(ExternalCause::Operator.as_str());
        Ok(events)
    }

    async fn merge_asked(
        &self,
        repo: &RepoRef,
        change: &ChangeSummary,
        cursors: &[ChangeCursorRow],
        writers: &mut BTreeMap<String, bool>,
        written: &mut Batch,
    ) -> Result<Option<Requested>> {
        let cursor = cursors.iter().find(|c| c.number == change.number);
        let wanted = Wanted {
            review: false,
            fix: false,
            merge: true,
        };
        let Some(seen) = self
            .read_commands(
                repo,
                change,
                &ChangeEntry::default(),
                cursor,
                writers,
                wanted,
            )
            .await?
        else {
            return Ok(None);
        };
        written.change_cursors.push(seen.cursor);
        Ok(seen.merge)
    }

    fn merge_on_command(
        &self,
        repo: &RepoRef,
        change: &ChangeSummary,
        id: saka_core::ids::WorkItemId,
        asked: &Requested,
        written: &mut Batch,
    ) {
        let cause = self.cause(SCAN_ACTOR);
        let why = format!("comment by {}", asked.actor);
        let outcome = self
            .load_task(id)
            .and_then(|task| self.request_merge(id, task.item.version, Some(&why), &cause));
        match outcome {
            Ok(_) => {
                self.counters.intake_change("command_accepted");
                tracing::info!(repo = %repo.full_name, number = change.number, actor = %asked.actor, "intake.command.accepted");
            }
            Err(EngineError::Rejected(rejection)) => {
                self.counters.intake_change("command_rejected");
                let code = rejection.code();
                tracing::info!(repo = %repo.full_name, number = change.number, actor = %asked.actor, code, "intake.command.rejected");
                let reply = self.load_task(id).ok().and_then(|task| {
                    saka_core::react::comment_job(
                        &task,
                        &change.head_sha,
                        code,
                        json!({ "message": rejection.to_string() }),
                        self.now(),
                    )
                });
                if let Some(job) = reply {
                    self.push_job(written, job, None, &cause);
                }
            }
            Err(e) => {
                tracing::warn!(repo = %repo.full_name, number = change.number, error = %e, "intake.command.failed");
            }
        }
    }

    fn fix_on_command(
        &self,
        repo: &RepoRef,
        number: u64,
        id: saka_core::ids::WorkItemId,
        asked: &Requested,
    ) {
        let outcome = self.decide(
            id,
            Some(TaskInput::FixRequested {
                cause: asked.cause,
                actor: Some(asked.actor.clone()),
            }),
            Mode::Command,
            &self.cause(SCAN_ACTOR),
            Extra::default(),
        );
        match outcome {
            Ok(_) => {
                tracing::info!(repo = %repo.full_name, number, actor = %asked.actor, "intake.fix.requested")
            }
            Err(EngineError::Rejected(Rejection::FixNotAvailable { reason })) => {
                self.counters.intake_change("command_denied");
                tracing::info!(repo = %repo.full_name, number, actor = %asked.actor, reason, "intake.command.denied");
            }
            Err(e) => {
                tracing::warn!(repo = %repo.full_name, number, error = %e, "intake.fix.failed")
            }
        }
    }

    pub async fn request_fix_with(
        &self,
        id: saka_core::ids::WorkItemId,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<saka_core::event::Event>> {
        let task = self.load_task(id)?;
        let (Some(change), true) = (
            task.change_request.as_ref(),
            task.item.origin == WorkItemOrigin::ExternalChange,
        ) else {
            return Err(Rejection::Unsupported.into());
        };
        let repo = self.repo_ref_stored(task.item.repo_id)?;
        let summary = self.ports.scm.change_summary(&repo, change.number).await?;
        if summary.state != saka_core::vocab::ChangeState::Open {
            return Err(Rejection::ChangeRequestNotOpen {
                number: change.number,
                state: summary.state.as_str(),
            }
            .into());
        }
        self.decide(
            id,
            Some(TaskInput::FixRequested {
                cause: ExternalCause::Operator,
                actor: None,
            }),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Task)),
                ..Extra::default()
            },
        )
    }

    pub async fn take_change_with(
        &self,
        project_id: ProjectId,
        repo_name: Option<&str>,
        number: u64,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<(Task, bool)> {
        if number == 0 {
            return Err(EngineError::Invalid {
                field: "number",
                code: "range".into(),
            });
        }
        let project = self
            .store
            .read(|c| read::load_project(c, project_id))?
            .ok_or(Rejection::NotFound)?;
        if project.workflow.change_request.is_none() {
            return Err(Rejection::EntryPointDisabled {
                entry: "change_request",
            }
            .into());
        }
        if !project.workflow.has_stage(Stage::Review) {
            return Err(Rejection::StageNotInWorkflow { stage: "review" }.into());
        }
        let binding = match repo_name {
            None => project.repos.iter().find(|r| r.is_default),
            Some(name) => project.repos.iter().find(|r| r.name == name),
        }
        .cloned()
        .ok_or_else(|| EngineError::Invalid {
            field: "repo",
            code: "unknown".into(),
        })?;
        let repo = self.repo_ref_stored(binding.repo_id)?;
        let summary = match self.ports.scm.change_summary(&repo, number).await {
            Ok(summary) => summary,
            Err(e) if e.code == "scm_repo_inaccessible" => {
                return Err(Rejection::ChangeRequestNotFound {
                    repo: binding.name,
                    number,
                }
                .into());
            }
            Err(e) => return Err(e.into()),
        };
        if summary.state != saka_core::vocab::ChangeState::Open {
            return Err(Rejection::ChangeRequestNotOpen {
                number,
                state: summary.state.as_str(),
            }
            .into());
        }
        let (tracked, branches, specs) = self.store.read(|c| {
            Ok((
                read::tracked_changes(c, binding.repo_id)?,
                read::spec_step_branches(c, binding.repo_id)?,
                read::project_specs(c, project_id)?,
            ))
        })?;
        match tracked.iter().find(|t| t.number == number) {
            Some(t) if t.origin == WorkItemOrigin::SpecStep => {
                return Err(Rejection::ChangeRequestOwned {
                    work_item_id: t.work_item_id.0.to_string(),
                }
                .into());
            }
            Some(t) if t.state.is_terminal() => {
                return Err(Rejection::ChangeRequestTracked {
                    work_item_id: t.work_item_id.0.to_string(),
                }
                .into());
            }
            Some(t) => {
                let events = self.decide(
                    t.work_item_id,
                    Some(TaskInput::ReviewRequested {
                        head_sha: summary.head_sha.clone(),
                        cause: ExternalCause::Operator,
                        actor: None,
                    }),
                    Mode::Command,
                    cause,
                    Extra {
                        action: action.map(|a| (a, Shows::Taken(false))),
                        ..Extra::default()
                    },
                )?;
                if !events.is_empty() {
                    self.counters
                        .review_request(ExternalCause::Operator.as_str());
                }
                return Ok((self.load_task(t.work_item_id)?, false));
            }
            None => {}
        }
        if let Some((_, owner)) = branches
            .iter()
            .find(|(b, _)| *b == summary.head_branch && !summary.from_fork)
        {
            return Err(Rejection::ChangeRequestOwned {
                work_item_id: owner.0.to_string(),
            }
            .into());
        }
        let link = self.spec_link(&summary, &specs)?;
        let taking = Taking {
            repo_name: &binding.name,
            spec: link,
            cause: ExternalCause::Operator,
            actor: None,
            by: Some(cause),
            action,
        };
        let created = self.accept_external(&project, &repo, &summary, taking)?;
        self.counters.intake_change("accepted");
        self.counters
            .review_request(ExternalCause::Operator.as_str());
        Ok((created, true))
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
        for p in &projects {
            if !self.scans(p, repo_id)? {
                continue;
            }
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
