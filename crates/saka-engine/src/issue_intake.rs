use saka_core::Ctx;
use saka_core::event::Event;
use saka_core::ids::{IssueIntakeId, ProjectId, RepoId, SpecDraftId, SpecId};
use saka_core::issue_intake::{self, Input, IssueIntake, NewIntake};
use saka_core::issue_text::{
    CommentEvent, CommentFacts, IssueFacts, architect_idea, clean_body, clean_title,
    quick_description,
};
use saka_core::job::{Job, JobKind};
use saka_core::model::Project;
use saka_core::rejection::Rejection;
use saka_core::vocab::{
    IssueCause, IssueLane, SpecDraftState, SpecOrigin, SpecState, WorkItemState, WorkKind,
};
use saka_core::workflow::IssueEntry;
use saka_ports::{IdsSource, Issue, IssueRef, IssueState, IssueSummary};
use saka_spec::{Locale, QuickSpec, quick_slug};
use saka_store::issue_intake as rows;
use saka_store::list::Page;
use saka_store::{Batch, ClaimedJob, TaskSet, read};
use serde_json::{Value, json};

use crate::action::{Action, Produced};
use crate::quick::QuickRequest;
use crate::views::{Paged, paged, probe};
use crate::worker::{Effect, Execution, field, internal};
use crate::{Cause, Engine, EngineError, NewSpecDraft, Result};

const SYSTEM_ACTOR: &str = "system:issue_intake";
const UNTITLED: &str = "(untitled)";
const URL_MAX: usize = 2048;
const AUTHOR_MAX: usize = 255;
const SLUG_MAX: usize = 63;
const DISMISS_SPEC: &str = "issue intake dismissed";
const CLOSED_SPEC: &str = "issue closed";

pub(crate) fn failure_of(error: &EngineError) -> Option<(String, Value)> {
    match error {
        EngineError::Rejected(r) => Some((r.code().to_string(), r.params())),
        EngineError::Invalid { field, code } => Some((
            "validation_failed".into(),
            json!({ "field": field, "code": code }),
        )),
        EngineError::InvalidItem { field, code } => Some((
            "validation_failed".into(),
            json!({ "field": field, "code": code }),
        )),
        EngineError::Port(e) if !e.retryable => Some((e.code.clone(), e.params.clone())),
        EngineError::Port(_) | EngineError::Store(_) | EngineError::Internal(_) => None,
    }
}

fn issue_spec_path(project: &Project, kind: WorkKind, title: &str, number: u64) -> String {
    let suffix = format!("-{number}");
    let base = quick_slug(kind, title).unwrap_or_else(|| match kind {
        WorkKind::Feature => "issue".to_string(),
        other => format!("{}-issue", other.as_str().replace('_', "-")),
    });
    let room = SLUG_MAX.saturating_sub(suffix.len());
    let kept: String = base.chars().take(room).collect();
    let slug = format!("{}{suffix}", kept.trim_end_matches('-'));
    match project.project_path.as_str() {
        "." => format!("specs/{slug}.md"),
        folder => format!("{folder}/specs/{slug}.md"),
    }
}

fn spec_slug_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(".md").unwrap_or(name).to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueRepoView {
    pub id: RepoId,
    pub name: String,
    pub full_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueSpecView {
    pub id: SpecId,
    pub path: String,
    pub state: SpecState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueDraftView {
    pub id: SpecDraftId,
    pub state: SpecDraftState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueIntakeView {
    pub intake: IssueIntake,
    pub repo: Option<IssueRepoView>,
    pub spec: Option<IssueSpecView>,
    pub draft: Option<IssueDraftView>,
}

fn view_of(project: &Project, intake: IssueIntake, links: rows::IntakeLinks) -> IssueIntakeView {
    let repo = links.repo.map(|repo| IssueRepoView {
        id: repo.id,
        name: project
            .repos
            .iter()
            .find(|r| r.repo_id == repo.id)
            .map_or_else(|| repo.full_name.clone(), |r| r.name.clone()),
        full_name: repo.full_name,
    });
    let spec = links.spec.map(|s| IssueSpecView {
        id: s.id,
        path: s.path,
        state: s.state,
    });
    let draft = intake
        .draft_id
        .zip(links.draft)
        .map(|(id, state)| IssueDraftView { id, state });
    IssueIntakeView {
        intake,
        repo,
        spec,
        draft,
    }
}

impl Engine {
    pub fn issue_intakes(
        &self,
        project: ProjectId,
        states: &[String],
        page: &Page,
    ) -> Result<Paged<IssueIntakeView>> {
        let (project, found) = self.store.read(|c| {
            let Some(project) = read::load_project(c, project)? else {
                return Ok((None, Vec::new()));
            };
            let found = rows::issue_intakes(c, project.id, states, &probe(page))?;
            Ok((Some(project), found))
        })?;
        let project = project.ok_or(Rejection::NotFound)?;
        let found = paged(found, page, |(_, key)| *key);
        let next = found.next;
        let items = self.store.read(|c| {
            found
                .items
                .into_iter()
                .map(|(intake, _)| {
                    let links = rows::intake_links(c, &intake)?;
                    Ok(view_of(&project, intake, links))
                })
                .collect::<saka_store::Result<Vec<_>>>()
        })?;
        Ok(Paged { items, next })
    }

    pub fn issue_intake(&self, id: IssueIntakeId) -> Result<IssueIntakeView> {
        self.store
            .read(|c| {
                let Some(intake) = rows::load_issue_intake(c, id)? else {
                    return Ok(None);
                };
                let Some(project) = read::load_project(c, intake.project_id)? else {
                    return Ok(None);
                };
                let links = rows::intake_links(c, &intake)?;
                Ok(Some(view_of(&project, intake, links)))
            })?
            .ok_or(EngineError::Rejected(Rejection::NotFound))
    }
}

pub(crate) struct Incoming<'a> {
    pub repo_id: RepoId,
    pub summary: &'a IssueSummary,
    pub cause: IssueCause,
    pub actor: String,
}

impl Engine {
    pub(crate) fn issue_step(
        &self,
        batch: &mut Batch,
        current: Option<&IssueIntake>,
        input: Input,
        cause: &Cause,
    ) -> Result<IssueIntake> {
        let now = self.now();
        let mut ids = IdsSource(&*self.ports.ids);
        let mut ctx = Ctx { now, ids: &mut ids };
        let events = issue_intake::decide(current, input, &mut ctx)?;
        let after = issue_intake::evolve_all(current.cloned(), &events, now)
            .ok_or_else(|| EngineError::Internal("issue intake dropped by its events".into()))?;
        for event in &events {
            self.push_event(batch, event, Some(after.project_id), cause, now);
            tracing::info!(
                intake_id = %after.id,
                state = after.state.as_str(),
                event = %event.kind(),
                "intake.issue.step"
            );
        }
        self.issue_effects(batch, &after, &events, cause)?;
        batch.issue_intakes.retain(|i| i.id != after.id);
        batch.issue_intakes.push(after.clone());
        Ok(after)
    }

    fn issue_effects(
        &self,
        batch: &mut Batch,
        after: &IssueIntake,
        events: &[Event],
        cause: &Cause,
    ) -> Result<()> {
        for event in events {
            match event {
                Event::IssueIntakeAccepted {
                    lane,
                    kind,
                    cause: why,
                    ..
                } => {
                    self.counters
                        .issue_intake(lane.as_str(), kind.as_str(), why.as_str());
                    self.push_comment(
                        batch,
                        after,
                        CommentEvent::Accepted,
                        "accepted",
                        None,
                        cause,
                    )?;
                    self.push_render(batch, after, 0, cause);
                }
                Event::IssueIntakeRetried { retries, .. } => {
                    self.push_render(batch, after, *retries, cause);
                }
                Event::IssueIntakeStateChanged { to, .. } => {
                    let (comment, key) = match to {
                        saka_core::vocab::IssueIntakeState::AwaitingApproval => {
                            (CommentEvent::AwaitingApproval, "awaiting_approval")
                        }
                        saka_core::vocab::IssueIntakeState::Approved => {
                            (CommentEvent::Approved, "approved")
                        }
                        saka_core::vocab::IssueIntakeState::Done => (CommentEvent::Done, "done"),
                        _ => continue,
                    };
                    self.push_comment(batch, after, comment, key, None, cause)?;
                }
                Event::IssueIntakeDismissed { reason, .. } => {
                    self.push_comment(
                        batch,
                        after,
                        CommentEvent::Dismissed,
                        "dismissed",
                        Some(CommentFacts {
                            reason: reason.clone(),
                            ..CommentFacts::default()
                        }),
                        cause,
                    )?;
                }
                Event::IssueIntakeFailed { code, .. } => {
                    let key = format!("failed:{}", after.retries);
                    self.push_comment(
                        batch,
                        after,
                        CommentEvent::Failed,
                        &key,
                        Some(CommentFacts {
                            failure_code: Some(code.clone()),
                            ..CommentFacts::default()
                        }),
                        cause,
                    )?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn push_render(&self, batch: &mut Batch, intake: &IssueIntake, attempt: u32, cause: &Cause) {
        let job = Job::new(
            JobKind::IntakeIssueRender,
            format!("{}:{attempt}", intake.id),
            json!({ "intake_id": intake.id }),
            self.now(),
        );
        self.push_job(batch, job, Some(intake.project_id), cause);
    }

    fn comment_facts(
        &self,
        intake: &IssueIntake,
        extra: Option<CommentFacts>,
    ) -> Result<CommentFacts> {
        let mut facts = extra.unwrap_or_default();
        facts.lane = Some(intake.lane);
        facts.kind = Some(intake.kind);
        if let Some(spec_id) = intake.spec_id {
            let (spec, tasks) = self.store.read(|c| {
                Ok((
                    read::load_spec(c, spec_id)?,
                    read::load_tasks(c, TaskSet::Spec(spec_id), &self.cfg.workflow_policy)?,
                ))
            })?;
            facts.spec_path = spec.map(|s| s.path);
            facts.task_key = tasks
                .iter()
                .filter(|t| t.item.state != WorkItemState::Superseded)
                .min_by_key(|t| t.item.step_index)
                .map(|t| t.item.task_key.clone());
        }
        Ok(facts)
    }

    pub(crate) fn comment_job_exists(&self, intake: &IssueIntake, key: &str) -> Result<bool> {
        let name = format!("{}:{key}", intake.id);
        let probe = Job::new(JobKind::ScmIssueComment, name, json!({}), self.now());
        Ok(self
            .store
            .read(|c| rows::job_exists(c, &probe.idempotency_key))?)
    }

    pub(crate) fn push_comment(
        &self,
        batch: &mut Batch,
        intake: &IssueIntake,
        event: CommentEvent,
        key: &str,
        extra: Option<CommentFacts>,
        cause: &Cause,
    ) -> Result<()> {
        let facts = self.comment_facts(intake, extra)?;
        let job = Job::new(
            JobKind::ScmIssueComment,
            format!("{}:{key}", intake.id),
            json!({
                "intake_id": intake.id,
                "repo_id": intake.repo_id,
                "number": intake.number,
                "event": event,
                "key": key,
                "facts": facts,
            }),
            self.now(),
        );
        self.push_job(batch, job, Some(intake.project_id), cause);
        Ok(())
    }

    pub(crate) fn push_limit_comment(
        &self,
        batch: &mut Batch,
        project: &Project,
        repo_id: RepoId,
        number: u64,
        max_open: u32,
        cause: &Cause,
    ) {
        let facts = CommentFacts {
            max_open: Some(max_open),
            ..CommentFacts::default()
        };
        let job = Job::new(
            JobKind::ScmIssueComment,
            format!("limit:{repo_id}:{number}"),
            json!({
                "repo_id": repo_id,
                "number": number,
                "event": CommentEvent::LimitReached,
                "key": "limit_reached",
                "facts": facts,
            }),
            self.now(),
        );
        self.push_job(batch, job, Some(project.id), cause);
    }

    pub(crate) fn accept_issue(
        &self,
        batch: &mut Batch,
        entry: &IssueEntry,
        project: &Project,
        incoming: Incoming<'_>,
        cause: &Cause,
    ) -> Result<IssueIntake> {
        let summary = incoming.summary;
        let kind = entry.kind_for(&summary.labels);
        let lane = entry.lane_for(kind);
        let title = clean_title(&summary.title);
        let author: String = if summary.author.is_empty() {
            "unknown".into()
        } else {
            summary.author.chars().take(AUTHOR_MAX).collect()
        };
        let new = NewIntake {
            project_id: project.id,
            repo_id: incoming.repo_id,
            number: summary.number,
            url: summary.url.chars().take(URL_MAX).collect(),
            title: title.clone().unwrap_or_else(|| UNTITLED.to_string()),
            author,
            lane,
            kind,
            cause: incoming.cause,
            actor: incoming.actor,
        };
        let accepted = self.issue_step(batch, None, Input::Accept(new), cause)?;
        if title.is_none() {
            return self.issue_step(
                batch,
                Some(&accepted),
                Input::Fail {
                    code: "issue_title_empty".into(),
                    params: json!({}),
                },
                cause,
            );
        }
        Ok(accepted)
    }

    pub(crate) async fn retire_issue_intake(
        &self,
        id: IssueIntakeId,
        input: Input,
        why: &str,
        cause: &Cause,
    ) -> Result<()> {
        let Some(intake) = self.store.read(|c| rows::load_issue_intake(c, id))? else {
            return Err(Rejection::NotFound.into());
        };
        if let Some(spec_id) = intake.spec_id {
            match self.withdraw_spec(spec_id, why, cause) {
                Ok(_) => {}
                Err(EngineError::Rejected(
                    Rejection::SpecAlreadyApproved | Rejection::InvalidState { .. },
                )) => {
                    let current = self.store.read(|c| read::load_spec(c, spec_id))?;
                    if current.is_some_and(|s| {
                        matches!(s.state, SpecState::Approved | SpecState::Shipped)
                    }) {
                        return Err(Rejection::IntakeAlreadyApproved.into());
                    }
                }
                Err(e) => return Err(e),
            }
        }
        if let Some(draft_id) = intake.draft_id {
            match self.cancel_spec_draft(draft_id, why, cause, None) {
                Ok(_) | Err(EngineError::Rejected(_)) => {}
                Err(e) => return Err(e),
            }
        }
        let _guard = self.locked()?;
        let Some(fresh) = self.store.read(|c| rows::load_issue_intake(c, id))? else {
            return Err(Rejection::NotFound.into());
        };
        let mut batch = Batch::new(self.now());
        self.issue_step(&mut batch, Some(&fresh), input, cause)?;
        self.commit(&batch)?;
        Ok(())
    }

    pub async fn dismiss_issue_intake_with(
        &self,
        id: IssueIntakeId,
        reason: Option<String>,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<IssueIntakeView> {
        let reason = match reason.map(|r| r.trim().to_string()) {
            Some(r) if r.is_empty() => None,
            Some(r) if r.chars().count() > saka_core::issue_text::MAX_REASON_CHARS => {
                return Err(crate::register::invalid("reason", "too_long"));
            }
            other => other,
        };
        let intake = self
            .store
            .read(|c| rows::load_issue_intake(c, id))?
            .ok_or(Rejection::NotFound)?;
        self.check_dismiss(&intake)?;
        self.retire_issue_intake(id, Input::Dismiss { reason }, DISMISS_SPEC, cause)
            .await?;
        self.finish_issue_action(id, cause, action)
    }

    fn check_dismiss(&self, intake: &IssueIntake) -> Result<()> {
        let mut ids = IdsSource(&*self.ports.ids);
        let mut ctx = Ctx {
            now: self.now(),
            ids: &mut ids,
        };
        issue_intake::decide(Some(intake), Input::Dismiss { reason: None }, &mut ctx)?;
        Ok(())
    }

    pub fn retry_issue_intake_with(
        &self,
        id: IssueIntakeId,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<IssueIntakeView> {
        let before = self
            .store
            .read(|c| rows::load_issue_intake(c, id))?
            .ok_or(Rejection::NotFound)?;
        if let Some(draft_id) = before.draft_id.filter(|_| {
            before.state == saka_core::vocab::IssueIntakeState::Failed
                && before.retries < saka_core::issue_intake::MANUAL_RETRY_LIMIT
        }) {
            match self.cancel_spec_draft(draft_id, "issue intake retried", cause, None) {
                Ok(_) | Err(EngineError::Rejected(_)) => {}
                Err(e) => return Err(e),
            }
        }
        let _guard = self.locked()?;
        let intake = self
            .store
            .read(|c| rows::load_issue_intake(c, id))?
            .ok_or(Rejection::NotFound)?;
        let mut batch = Batch::new(self.now());
        self.issue_step(&mut batch, Some(&intake), Input::Retry, cause)?;
        self.commit(&batch)?;
        drop(_guard);
        self.finish_issue_action(id, cause, action)
    }

    fn finish_issue_action(
        &self,
        id: IssueIntakeId,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<IssueIntakeView> {
        let view = self.issue_intake(id)?;
        if let Some(action) = action {
            let reply = (action.respond)(Produced::IssueIntake(&view));
            let mut batch = Batch::new(self.now());
            batch.action =
                Some(action.record(self, cause, Some(view.intake.project_id), Some(id.0), reply));
            self.commit(&batch)?;
        }
        Ok(view)
    }

    pub async fn create_issue_intake_with(
        &self,
        project_id: ProjectId,
        number: u64,
        repo_name: Option<&str>,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<IssueIntakeView> {
        let project = self
            .store
            .read(|c| read::load_project(c, project_id))?
            .ok_or(Rejection::NotFound)?;
        let entry = project
            .workflow
            .issue
            .clone()
            .ok_or(Rejection::EntryPointDisabled { entry: "issue" })?;
        let binding = match (repo_name, project.repos.as_slice()) {
            (Some(name), repos) => repos
                .iter()
                .find(|r| r.name == name)
                .ok_or_else(|| crate::register::invalid("repo", "unknown"))?,
            (None, [only]) => only,
            (None, _) => return Err(crate::register::invalid("repo", "required")),
        };
        let repo = self.repo_ref_stored(binding.repo_id)?;
        let issue = self
            .ports
            .intake
            .issue(&IssueRef {
                repo: repo.clone(),
                number,
            })
            .await
            .map_err(|e| match e.code.as_str() {
                "issue_not_found" => EngineError::Rejected(Rejection::IssueNotFound),
                "issue_is_change_request" => {
                    crate::register::invalid("number", "is_change_request")
                }
                _ => EngineError::Port(e),
            })?;
        if issue.summary.state == IssueState::Closed {
            return Err(crate::register::invalid("number", "closed"));
        }
        let id = {
            let _guard = self.locked()?;
            if let Some(live) = self
                .store
                .read(|c| rows::live_issue_intake(c, binding.repo_id, number))?
            {
                return Err(Rejection::IntakeActive { intake_id: live.id }.into());
            }
            let mut batch = Batch::new(self.now());
            let created = self.accept_issue(
                &mut batch,
                &entry,
                &project,
                Incoming {
                    repo_id: binding.repo_id,
                    summary: &issue.summary,
                    cause: IssueCause::Operator,
                    actor: cause.actor.clone(),
                },
                cause,
            )?;
            self.commit(&batch)?;
            created.id
        };
        self.finish_issue_action(id, cause, action)
    }

    fn render_failure(&self, intake: &IssueIntake, code: String, params: Value) -> Result<Effect> {
        let _guard = self.locked()?;
        let Some(fresh) = self.store.read(|c| rows::load_issue_intake(c, intake.id))? else {
            return Ok(Effect::Skipped);
        };
        let cause = self.cause(SYSTEM_ACTOR);
        let mut batch = Batch::new(self.now());
        match self.issue_step(
            &mut batch,
            Some(&fresh),
            Input::Fail { code, params },
            &cause,
        ) {
            Ok(_) => {
                self.commit(&batch)?;
                Ok(Effect::Nothing)
            }
            Err(EngineError::Rejected(_)) => Ok(Effect::Skipped),
            Err(e) => Err(e),
        }
    }

    pub(crate) async fn issue_render_job(&self, job: &ClaimedJob) -> Execution {
        let id: IssueIntakeId = field(&job.payload, "intake_id")?;
        let Some(intake) = self
            .store
            .read(|c| rows::load_issue_intake(c, id))
            .map_err(|e| internal(e.into()))?
        else {
            return Ok(Effect::Skipped);
        };
        if intake.state != saka_core::vocab::IssueIntakeState::Accepted {
            return Ok(Effect::Skipped);
        }
        match self.render_intake(&intake).await {
            Ok(effect) => Ok(effect),
            Err(error) => match failure_of(&error) {
                Some((code, params)) => {
                    self.render_failure(&intake, code, params).map_err(internal)
                }
                None => match error {
                    EngineError::Port(e) if job.attempts >= job.max_attempts => self
                        .render_failure(&intake, e.code, e.params)
                        .map_err(internal),
                    EngineError::Port(e) => Err(e),
                    other => Err(internal(other)),
                },
            },
        }
    }

    async fn render_intake(&self, intake: &IssueIntake) -> Result<Effect> {
        let project = self
            .store
            .read(|c| read::load_project(c, intake.project_id))?
            .ok_or(Rejection::NotFound)?;
        let entry = project
            .workflow
            .issue
            .clone()
            .ok_or(Rejection::EntryPointDisabled { entry: "issue" })?;
        let repo = self.repo_ref_stored(intake.repo_id)?;
        let issue = match self
            .ports
            .intake
            .issue(&IssueRef {
                repo,
                number: intake.number,
            })
            .await
        {
            Ok(issue) => issue,
            Err(e) if e.code == "issue_not_found" => {
                return self.close_from_job(intake).await;
            }
            Err(e) => return Err(e.into()),
        };
        if issue.summary.state == IssueState::Closed {
            return self.close_from_job(intake).await;
        }
        let Some(title) = clean_title(&issue.summary.title) else {
            return Err(crate::register::invalid("title", "issue_title_empty"));
        };
        let redactor = self.project_redactor(project.id)?;
        let body = clean_body(&issue.body, &redactor);
        match intake.lane {
            IssueLane::Bugfix => {
                self.render_bugfix(intake, &project, &entry, &issue, &title, &body)
                    .await
            }
            IssueLane::Draft => {
                self.render_draft(intake, &project, &issue, &title, &body)
                    .await
            }
        }
    }

    async fn close_from_job(&self, intake: &IssueIntake) -> Result<Effect> {
        let cause = self.cause(SYSTEM_ACTOR);
        self.retire_issue_intake(intake.id, Input::IssueClosed, CLOSED_SPEC, &cause)
            .await?;
        Ok(Effect::Nothing)
    }

    async fn render_bugfix(
        &self,
        intake: &IssueIntake,
        project: &Project,
        entry: &IssueEntry,
        issue: &Issue,
        title: &str,
        body: &str,
    ) -> Result<Effect> {
        let Some(commands) = &entry.bugfix else {
            return Err(crate::register::invalid("bugfix", "commands_missing"));
        };
        let path = issue_spec_path(project, intake.kind, title, intake.number);
        let request = QuickRequest {
            spec: QuickSpec {
                kind: intake.kind,
                title: title.to_string(),
                description: quick_description(&issue.summary.url, &issue.summary.author, body),
                done_when: commands.done_when.clone(),
                regression: commands.regression.clone(),
                criteria: Vec::new(),
                locale: Locale::En,
                repo: None,
            },
            path: Some(path.clone()),
            approve: false,
            origin: SpecOrigin::Intake,
        };
        let cause = self.cause(SYSTEM_ACTOR);
        let spec_id = match self
            .quick_spec_with(project.id, request, &cause, None)
            .await
        {
            Ok(view) => view.spec.spec.id,
            Err(EngineError::Rejected(
                Rejection::SpecSlugConflict { .. } | Rejection::FilePathConflict { .. },
            )) => self.existing_intake_spec(project.id, &path)?,
            Err(e) => return Err(e),
        };
        self.link_ready_spec(intake.id, spec_id, &cause)?;
        Ok(Effect::Nothing)
    }

    fn existing_intake_spec(&self, project_id: ProjectId, path: &str) -> Result<SpecId> {
        let slug = spec_slug_of(path);
        let found = self
            .store
            .read(|c| saka_store::list::spec_by_slug(c, project_id, &slug))?;
        match found {
            Some(spec)
                if spec.origin == SpecOrigin::Intake && spec.state != SpecState::Withdrawn =>
            {
                Ok(spec.id)
            }
            _ => Err(Rejection::FilePathConflict {
                path: path.to_string(),
            }
            .into()),
        }
    }

    fn link_ready_spec(&self, id: IssueIntakeId, spec_id: SpecId, cause: &Cause) -> Result<()> {
        let _guard = self.locked()?;
        let Some(fresh) = self.store.read(|c| rows::load_issue_intake(c, id))? else {
            return Ok(());
        };
        let mut batch = Batch::new(self.now());
        match self.issue_step(
            &mut batch,
            Some(&fresh),
            Input::SpecReady { spec_id },
            cause,
        ) {
            Ok(_) => {}
            Err(EngineError::Rejected(_)) => return Ok(()),
            Err(e) => return Err(e),
        }
        if let Some(spec) = self.store.read(|c| read::load_spec(c, spec_id))? {
            batch.specs.push(spec);
        }
        self.commit(&batch)?;
        Ok(())
    }

    async fn render_draft(
        &self,
        intake: &IssueIntake,
        project: &Project,
        issue: &Issue,
        title: &str,
        body: &str,
    ) -> Result<Effect> {
        let path = issue_spec_path(project, intake.kind, title, intake.number);
        let idea = architect_idea(
            intake.kind,
            &IssueFacts {
                title,
                url: &issue.summary.url,
                author: &issue.summary.author,
                labels: &issue.summary.labels,
                body,
            },
        );
        let cause = self.cause(SYSTEM_ACTOR);
        let draft = self
            .create_spec_draft(
                project.id,
                NewSpecDraft {
                    path,
                    idea,
                    locale: saka_core::vocab::DraftLocale::En,
                    mode: saka_core::vocab::DraftMode::Draft,
                    target_kind: saka_core::vocab::TargetKind::New,
                    kind: intake.kind,
                    context_paths: Vec::new(),
                },
                &cause,
                None,
            )
            .await?;
        let _guard = self.locked()?;
        let Some(fresh) = self.store.read(|c| rows::load_issue_intake(c, intake.id))? else {
            return Ok(Effect::Skipped);
        };
        let mut batch = Batch::new(self.now());
        match self.issue_step(
            &mut batch,
            Some(&fresh),
            Input::DraftStarted {
                draft_id: draft.draft.id,
            },
            &cause,
        ) {
            Ok(_) => self.commit(&batch).map(|_| ())?,
            Err(EngineError::Rejected(_)) => {}
            Err(e) => return Err(e),
        }
        Ok(Effect::Nothing)
    }

    pub(crate) async fn issue_comment_job(&self, job: &ClaimedJob) -> Execution {
        let p = &job.payload;
        let repo_id: RepoId = field(p, "repo_id")?;
        let number: u64 = field(p, "number")?;
        let event: CommentEvent = field(p, "event")?;
        let key: String = field(p, "key")?;
        let mut facts: CommentFacts = field(p, "facts")?;
        let intake_id: Option<IssueIntakeId> = p
            .get("intake_id")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        if let Some(id) = intake_id
            && self
                .store
                .read(|c| rows::comment_posted(c, id, &key))
                .map_err(|e| internal(e.into()))?
        {
            return Ok(Effect::Skipped);
        }
        facts.public_url = self.cfg.public_url.clone();
        let repo = self.repo_ref_of(repo_id)?;
        let body = saka_core::issue_text::status_comment(event, &facts);
        match self
            .ports
            .intake
            .post_issue_comment(&IssueRef { repo, number }, &body)
            .await
        {
            Ok(()) => {
                self.counters.issue_comment("posted");
                if let Some(id) = intake_id {
                    let cause = self.cause(SYSTEM_ACTOR);
                    let mut batch = Batch::new(self.now());
                    let recorded = Event::IssueIntakeCommentPosted {
                        intake_id: id,
                        event: key,
                    };
                    let project = self
                        .store
                        .read(|c| rows::load_issue_intake(c, id))
                        .map_err(|e| internal(e.into()))?
                        .map(|i| i.project_id);
                    self.push_event(&mut batch, &recorded, project, &cause, self.now());
                    self.commit(&batch).map_err(internal)?;
                }
                Ok(Effect::Nothing)
            }
            Err(e) if e.retryable && job.attempts < job.max_attempts => Err(e),
            Err(e) => {
                self.counters.issue_comment("failed");
                tracing::warn!(code = %e.code, number, "intake.issue.comment_failed");
                Ok(Effect::Nothing)
            }
        }
    }
}
