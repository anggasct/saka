use saka_core::Ctx;
use saka_core::event::Event;
use saka_core::ids::{
    PermissionRequestId, PermissionRuleId, ProjectId, RepoId, SessionId, SpecId, SpecRevisionId,
    WorkItemId,
};
use saka_core::model::{PermissionRule, ProjectRepo};
use saka_core::rejection::Rejection;
use saka_core::spec::{self, NewRevision};
use saka_core::task::{self, TaskInput};
use saka_core::workspace;
use saka_ports::IdsSource;
use saka_spec::{LintIssue, PARSER_VERSION};
use saka_store::{Batch, SpecRevisionRow, TaskSet, list, read};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::action::{Action, Produced};
use crate::views::{ProjectView, RevisionInfo, SpecView, step_items};
use crate::{Cause, Engine, EngineError, Extra, Mode, OPERATOR, Result, Shows};

const REASON_MAX: usize = 500;
const STEER_MAX: usize = 4000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionCommand {
    Steer { text: String },
    Interrupt,
    Stop { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProject {
    pub name: String,
    pub integration_id: String,
    pub provider: String,
    pub full_name: String,
    pub url: String,
    pub default_branch: String,
    pub project_path: String,
    pub base_branch: String,
    pub config_commit: String,
    pub repo_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegisteredSpec {
    pub spec_id: SpecId,
    pub revision_id: SpecRevisionId,
    pub lint_ok: bool,
    pub issues: Vec<LintIssue>,
}

fn reason(value: &str) -> Result<String> {
    let trimmed = value.trim();
    let len = trimmed.chars().count();
    if len == 0 || len > REASON_MAX {
        return Err(EngineError::Invalid {
            field: "reason",
            code: "length".into(),
        });
    }
    Ok(trimmed.to_string())
}

fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

impl Engine {
    pub fn register_project(&self, p: &NewProject, cause: &Cause) -> Result<(ProjectId, RepoId)> {
        let view = self.register_project_with(p, None, cause, None)?;
        Ok((view.project.id, view.project.workspace_repo_id))
    }

    pub(crate) fn register_project_with(
        &self,
        p: &NewProject,
        repo_id: Option<RepoId>,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<ProjectView> {
        let _guard = self.locked()?;
        let now = self.now();
        let existing = self
            .store
            .read(|c| read::repo_by_name(c, &p.integration_id, &p.full_name))?;
        if let Some(repo) = &existing {
            self.refuse_existing_project(repo.id, &p.project_path)?;
        }
        let mut batch = Batch::new(now);
        let mut events = Vec::new();
        let mut repos = Vec::new();
        let repo_id = match existing {
            Some(repo) => {
                let id = repo.id;
                repos.push(repo);
                id
            }
            None => {
                let repo_id = repo_id.unwrap_or_else(|| RepoId(self.new_id()));
                events.push(Event::RepoRegistered {
                    repo_id,
                    integration_id: p.integration_id.clone(),
                    provider: p.provider.clone(),
                    full_name: p.full_name.clone(),
                    url: p.url.clone(),
                    default_branch: p.default_branch.clone(),
                });
                repo_id
            }
        };
        let project_id = ProjectId(self.new_id());
        events.push(Event::ProjectRegistered {
            project_id,
            name: p.name.clone(),
            workspace_repo_id: repo_id,
            project_path: p.project_path.clone(),
            base_branch: p.base_branch.clone(),
            config_commit: p.config_commit.clone(),
            repos: vec![ProjectRepo {
                repo_id,
                name: p.repo_name.clone(),
                is_default: true,
                path_scope: vec!["**".into()],
            }],
        });
        let mut project = None;
        for e in &events {
            if let Some(repo) = workspace::evolve_repo(None, e, now) {
                batch.repos.push(repo.clone());
                repos.push(repo);
            }
            if let Some(p) = workspace::evolve_project(None, e, now) {
                batch.projects.push(p.clone());
                project = Some(p);
            }
            self.push_event(&mut batch, e, Some(project_id), cause, now);
        }
        let project =
            project.ok_or_else(|| EngineError::Internal("project not registered".into()))?;
        let view = ProjectView { project, repos };
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Project(&view));
            batch.action =
                Some(action.record(self, cause, Some(project_id), Some(project_id.0), reply));
        }
        self.commit(&batch)?;
        Ok(view)
    }

    pub fn register_spec(
        &self,
        project_id: ProjectId,
        path: &str,
        source_commit: &str,
        bytes: &[u8],
        cause: &Cause,
    ) -> Result<RegisteredSpec> {
        let (view, _) =
            self.register_spec_with(project_id, path, source_commit, bytes, cause, None)?;
        Ok(RegisteredSpec {
            spec_id: view.spec.id,
            revision_id: view.revision.id,
            lint_ok: view.revision.lint_ok,
            issues: serde_json::from_value(view.revision.issues)
                .map_err(|e| EngineError::Internal(e.to_string()))?,
        })
    }

    pub(crate) fn register_spec_with(
        &self,
        project_id: ProjectId,
        path: &str,
        source_commit: &str,
        bytes: &[u8],
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<(SpecView, bool)> {
        let _guard = self.locked()?;
        let now = self.now();
        let file_name = path.rsplit('/').next().unwrap_or(path);
        let stem = file_name.strip_suffix(".md").unwrap_or(file_name);
        let parsed = saka_spec::parse(file_name, bytes);
        let hash = content_hash(bytes);
        let (existing, known) = self.store.read(|c| {
            let existing = read::spec_by_path(c, project_id, path)?;
            let known = match &existing {
                Some(s) => read::revision_by_hash(c, s.id, &hash, PARSER_VERSION)?,
                None => None,
            };
            Ok((existing, known))
        })?;
        let lint_ok = parsed.is_ready();
        let revision = NewRevision {
            source_commit: source_commit.to_string(),
            content_hash: hash.clone(),
            lint_ok,
            issue_codes: parsed
                .issues
                .iter()
                .map(|i| i.code.as_str().to_string())
                .collect(),
            title: parsed
                .model
                .as_ref()
                .map_or_else(|| stem.to_string(), |m| m.title.clone()),
            parser_version: PARSER_VERSION,
            priority: parsed.model.as_ref().map_or(0, |m| i64::from(m.priority)),
        };
        let slug = parsed
            .model
            .as_ref()
            .map_or_else(|| stem.to_string(), |m| m.slug.clone());
        if existing.is_none()
            && let Some(other) = self
                .store
                .read(|c| list::spec_by_slug(c, project_id, &slug))?
        {
            return Err(Rejection::SpecSlugConflict {
                slug,
                path: other.path,
            }
            .into());
        }
        let mut ids = IdsSource(&*self.ports.ids);
        let mut ctx = Ctx { now, ids: &mut ids };
        let events = spec::record_revision(
            existing.as_ref(),
            project_id,
            path,
            &slug,
            revision,
            known,
            &mut ctx,
        );

        let mut batch = Batch::new(now);
        let mut after = existing.clone();
        let mut revision_id = existing.as_ref().map(|s| s.latest_revision_id);
        for e in &events {
            after = spec::evolve(after, e, now);
            if let Event::SpecRevisionRecorded {
                spec_id,
                revision_id: id,
                reused,
                ..
            } = e
            {
                revision_id = Some(*id);
                if !reused {
                    batch.revisions.push(SpecRevisionRow {
                        id: id.0,
                        spec_id: spec_id.0,
                        project_id,
                        source_commit: source_commit.to_string(),
                        content_hash: hash.clone(),
                        content: bytes.to_vec(),
                        lint_ok,
                        lint_issues: serde_json::to_value(&parsed.issues)
                            .map_err(|e| EngineError::Internal(e.to_string()))?,
                        model: match &parsed.model {
                            Some(m) => serde_json::to_value(m)
                                .map_err(|e| EngineError::Internal(e.to_string()))?,
                            None => json!({}),
                        },
                        parser_version: PARSER_VERSION,
                        created_at: now,
                    });
                }
            }
            self.push_event(&mut batch, e, Some(project_id), cause, now);
        }
        let spec = after.ok_or_else(|| EngineError::Internal("spec not recorded".into()))?;
        let revision_id = revision_id.ok_or_else(|| EngineError::Internal("no revision".into()))?;
        batch.specs.push(spec.clone());
        let revision = match batch.revisions.iter().find(|r| r.id == revision_id.0) {
            Some(row) => RevisionInfo::from(row),
            None => self
                .store
                .read(|c| read::load_revision(c, revision_id))?
                .as_ref()
                .map(RevisionInfo::from)
                .ok_or_else(|| EngineError::Internal("revision not found".into()))?,
        };
        let work_items = step_items(
            &self
                .store
                .read(|c| read::load_tasks(c, TaskSet::Spec(spec.id)))?,
        );
        let created = existing.is_none();
        let view = self.spec_view_with(spec, revision, work_items)?;
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Spec(&view, created));
            batch.action =
                Some(action.record(self, cause, Some(project_id), Some(view.spec.id.0), reply));
        }
        self.commit(&batch)?;
        Ok((view, created))
    }

    pub fn approve_spec(
        &self,
        spec_id: SpecId,
        if_match: SpecRevisionId,
        reason: Option<String>,
        cause: &Cause,
    ) -> Result<Vec<WorkItemId>> {
        self.approve_spec_with(spec_id, if_match, reason, cause, None)
            .map(|view| view.work_items.into_iter().map(|(_, id)| id).collect())
    }

    pub fn approve_spec_with(
        &self,
        spec_id: SpecId,
        if_match: SpecRevisionId,
        reason: Option<String>,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<SpecView> {
        if let Some(r) = &reason {
            self::reason(r)?;
        }
        let _guard = self.locked()?;
        let now = self.now();
        let (spec, view, row) = self.store.read(|c| {
            let spec = read::load_spec(c, spec_id)?;
            let (view, row) = match &spec {
                Some(s) => (
                    read::revision_view(c, s.latest_revision_id)?,
                    read::load_revision(c, s.latest_revision_id)?,
                ),
                None => (None, None),
            };
            Ok((spec, view, row))
        })?;
        let (Some(spec), Some(view), Some(row)) = (spec, view, row) else {
            return Err(Rejection::NotFound.into());
        };
        let (project_specs, project_graph, approved, tasks) = self.store.read(|c| {
            let approved = match spec.approved_revision_id {
                Some(id) => read::revision_view(c, id)?,
                None => None,
            };
            Ok((
                read::project_specs(c, spec.project_id)?,
                read::spec_dependencies(c, Some(spec.project_id))?,
                approved,
                read::load_tasks(c, TaskSet::Spec(spec.id))?,
            ))
        })?;
        let items: Vec<_> = tasks.iter().map(|t| t.item.clone()).collect();
        let file_name = spec.path.rsplit('/').next().unwrap_or(&spec.path);
        let depends_on_line = saka_spec::parse(file_name, &row.content)
            .depends_on_line
            .unwrap_or(1);
        let project = self
            .store
            .read(|c| read::load_project(c, spec.project_id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let default_repo = project
            .default_repo()
            .ok_or_else(|| EngineError::Internal("project has no default repo".into()))?;
        let mut ids = IdsSource(&*self.ports.ids);
        let mut ctx = Ctx { now, ids: &mut ids };
        let approval = spec::Approval {
            if_match,
            reason,
            default_repo,
            parser_version: PARSER_VERSION,
            depends_on_line,
            project_specs: &project_specs,
            project_graph: &project_graph,
            approved: approved.as_ref(),
            tasks: &items,
        };
        let events = spec::approve(&spec, &view, approval, &mut ctx)?;

        let mut batch = Batch::new(now);
        (batch.spec_dependencies, batch.work_item_dependencies) =
            spec::approval_graph(&spec, &view, &events, &items);
        batch.replaced_spec_dependencies.push(spec.id);
        let mut after = Some(spec.clone());
        let mut created = Vec::new();
        let mut current = tasks.clone();
        for e in &events {
            after = spec::evolve(after, e, now);
            match e {
                Event::WorkItemCreated { .. } => {
                    if let Some(t) = task::evolve(None, e, now) {
                        created.push(t.clone());
                        batch.tasks.push(t.clone());
                        current.push(t);
                    }
                }
                Event::WorkItemRepinned { work_item_id, .. }
                | Event::WorkItemSuperseded { work_item_id, .. } => {
                    if matches!(e, Event::WorkItemRepinned { .. }) {
                        batch.replaced_work_item_dependencies.push(*work_item_id);
                    }
                    if let Some(t) = current.iter_mut().find(|t| t.item.id == *work_item_id)
                        && let Some(next) = task::evolve(Some(t.clone()), e, now)
                    {
                        *t = next.clone();
                        batch.tasks.push(next);
                    }
                }
                _ => {}
            }
            self.push_event(&mut batch, e, Some(spec.project_id), cause, now);
        }
        let branches: Vec<String> = created.iter().map(|t| t.item.branch.clone()).collect();
        let taken = self
            .store
            .read(|c| list::branches_in_use(c, default_repo, &branches, spec.project_id))?;
        if !taken.is_empty() {
            return Err(Rejection::TaskBranchConflict { branches: taken }.into());
        }
        let after = after.ok_or_else(|| EngineError::Internal("spec lost on approve".into()))?;
        batch.specs.push(after.clone());
        let revision = self
            .store
            .read(|c| read::load_revision(c, after.latest_revision_id))?
            .as_ref()
            .map(RevisionInfo::from)
            .ok_or_else(|| EngineError::Internal("revision not found".into()))?;
        let result = self.spec_view_with(after, revision, step_items(&current))?;
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Spec(&result, false));
            batch.action =
                Some(action.record(self, cause, Some(spec.project_id), Some(spec_id.0), reply));
        }
        self.commit(&batch)?;
        if let Some(from) = spec.approved_revision_id {
            let count = |kind: &str| events.iter().filter(|e| e.kind() == kind).count();
            let restored = events
                .iter()
                .filter(|e| matches!(e, Event::WorkItemRepinned { restored: true, .. }))
                .count();
            tracing::info!(
                spec_id = %spec_id.0,
                from_revision_id = %from.0,
                to_revision_id = %view.id.0,
                kept = tasks.iter().filter(|t| t.item.state.has_started()).count(),
                repinned = count("delivery.work_item.repinned") - restored,
                restored,
                added = count("delivery.work_item.created"),
                superseded = count("delivery.work_item.superseded"),
                "spec re-approved"
            );
        }
        Ok(result)
    }

    pub fn withdraw_spec(&self, spec_id: SpecId, why: &str, cause: &Cause) -> Result<SpecView> {
        self.withdraw_spec_with(spec_id, why, cause, None)
    }

    pub fn withdraw_spec_with(
        &self,
        spec_id: SpecId,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<SpecView> {
        let reason = reason(why)?;
        let _guard = self.locked()?;
        let now = self.now();
        let (spec, tasks) = self.store.read(|c| {
            Ok((
                read::load_spec(c, spec_id)?,
                read::load_tasks(c, TaskSet::Spec(spec_id))?,
            ))
        })?;
        let spec = spec.ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let items: Vec<_> = tasks.iter().map(|t| t.item.clone()).collect();
        let events = spec::withdraw(&spec, &items, reason)?;
        let mut batch = Batch::new(now);
        let mut after = Some(spec.clone());
        let mut current = tasks;
        for e in &events {
            after = spec::evolve(after, e, now);
            if let Event::WorkItemSuperseded { work_item_id, .. } = e
                && let Some(t) = current.iter_mut().find(|t| t.item.id == *work_item_id)
                && let Some(next) = task::evolve(Some(t.clone()), e, now)
            {
                *t = next.clone();
                batch.tasks.push(next);
            }
            self.push_event(&mut batch, e, Some(spec.project_id), cause, now);
        }
        let after = after.ok_or_else(|| EngineError::Internal("spec lost on withdraw".into()))?;
        batch.specs.push(after.clone());
        let revision = self
            .store
            .read(|c| read::load_revision(c, after.latest_revision_id))?
            .as_ref()
            .map(RevisionInfo::from)
            .ok_or_else(|| EngineError::Internal("revision not found".into()))?;
        let result = self.spec_view_with(after, revision, step_items(&current))?;
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Spec(&result, false));
            batch.action =
                Some(action.record(self, cause, Some(spec.project_id), Some(spec_id.0), reply));
        }
        self.commit(&batch)?;
        tracing::info!(
            spec_id = %spec_id.0,
            superseded = events.len() - 1,
            "spec withdrawn"
        );
        Ok(result)
    }

    pub fn retry(&self, id: WorkItemId, why: &str, cause: &Cause) -> Result<Vec<Event>> {
        self.retry_with(id, why, cause, None)
    }

    pub fn retry_with(
        &self,
        id: WorkItemId,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        let reason = reason(why)?;
        self.decide(
            id,
            Some(TaskInput::Retry { reason }),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Task)),
                ..Extra::default()
            },
        )
    }

    pub fn cancel(&self, id: WorkItemId, why: &str, cause: &Cause) -> Result<Vec<Event>> {
        self.cancel_with(id, why, cause, None)
    }

    pub fn cancel_with(
        &self,
        id: WorkItemId,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        let reason = reason(why)?;
        self.decide(
            id,
            Some(TaskInput::Cancel { reason }),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Task)),
                ..Extra::default()
            },
        )
    }

    pub fn request_merge(
        &self,
        id: WorkItemId,
        expected_version: u64,
        why: Option<&str>,
        cause: &Cause,
    ) -> Result<Vec<Event>> {
        self.request_merge_with(id, expected_version, why, cause, None)
    }

    pub fn request_merge_with(
        &self,
        id: WorkItemId,
        expected_version: u64,
        why: Option<&str>,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        let reason = why.map(reason).transpose()?;
        self.decide(
            id,
            Some(TaskInput::RequestMerge {
                expected_version,
                reason,
            }),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Task)),
                ..Extra::default()
            },
        )
    }

    pub fn session_command(
        &self,
        session: SessionId,
        command: SessionCommand,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        let work_item = self
            .store
            .read(|c| read::session_work_item(c, session))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let steering = matches!(command, SessionCommand::Steer { .. });
        let input = match command {
            SessionCommand::Steer { text } => {
                let len = text.trim().chars().count();
                if len == 0 || len > STEER_MAX {
                    return Err(EngineError::Invalid {
                        field: "text",
                        code: "length".into(),
                    });
                }
                TaskInput::Steer {
                    session_id: session,
                    text,
                }
            }
            SessionCommand::Interrupt => TaskInput::Interrupt {
                session_id: session,
            },
            SessionCommand::Stop { reason: why } => TaskInput::StopSession {
                session_id: session,
                reason: reason(&why)?,
            },
        };
        let events = self.decide(
            work_item,
            Some(input),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Session(session))),
                ..Extra::default()
            },
        )?;
        let steered = events
            .iter()
            .any(|e| matches!(e, Event::SessionSteered { .. }));
        if steering && !steered {
            // The turn budget ran out: the task is now blocked and the steer was not sent.
            return Err(EngineError::Rejected(Rejection::InvalidState {
                state: "idle".into(),
            }));
        }
        Ok(events)
    }

    pub fn revoke_rule(
        &self,
        id: PermissionRuleId,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<PermissionRule> {
        let why = reason(why)?;
        let _guard = self.locked()?;
        let now = self.now();
        let rule = self
            .store
            .read(|c| read::load_permission_rule(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let events = saka_core::rule::revoke(&rule, why)?;
        let mut batch = Batch::new(now);
        let mut after = Some(rule.clone());
        for e in &events {
            after = saka_core::rule::evolve(after, e, now);
            self.push_event(&mut batch, e, Some(rule.project_id), cause, now);
            crate::log_runtime(e, &cause.actor);
        }
        let after = after.ok_or_else(|| EngineError::Internal("revoke dropped the rule".into()))?;
        if let Some(a) = action {
            let reply = (a.respond)(Produced::Rule(&after));
            batch.action = Some(a.record(self, cause, Some(rule.project_id), Some(id.0), reply));
        }
        batch.permission_rules.push(after.clone());
        self.commit(&batch)?;
        Ok(after)
    }

    pub fn answer_permission(
        &self,
        request: PermissionRequestId,
        allow: bool,
        message: Option<String>,
        cause: &Cause,
    ) -> Result<Vec<Event>> {
        self.answer_permission_with(request, allow, message, false, cause, None)
    }

    pub fn answer_permission_with(
        &self,
        request: PermissionRequestId,
        allow: bool,
        message: Option<String>,
        remember: bool,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<Vec<Event>> {
        if let Some(m) = &message {
            self::reason(m).map_err(|_| EngineError::Invalid {
                field: "message",
                code: "length".into(),
            })?;
        }
        let work_item = self
            .store
            .read(|c| read::permission_work_item(c, request))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let active_rules = if remember {
            let task = self.load_task(work_item)?;
            self.store
                .read(|c| read::active_permission_rules(c, task.item.project_id))?
        } else {
            Vec::new()
        };
        let input = TaskInput::AnswerPermission {
            permission_request_id: request,
            allow,
            message,
            actor: OPERATOR.to_string(),
            remember,
            active_rules,
        };
        self.decide(
            work_item,
            Some(input),
            Mode::Command,
            cause,
            Extra {
                action: action.map(|a| (a, Shows::Permission(request))),
                ..Extra::default()
            },
        )
    }
}
