use saka_core::admission::{Waiting, waiting_on};
use saka_core::ids::{
    PermissionRequestId, ProjectId, RepoId, SessionId, SpecId, SpecRevisionId, WorkItemId,
};
use saka_core::model::{
    AccountQuota, Budget, ContextRef, PermissionRequest, PermissionRule, Project, Repo, Session,
    SessionActivity, Spec, StageRun, StepPlan, Task, Usage, UsageRecord, step_repo,
};
use saka_core::plan::RevisionPlan;
use saka_core::spec as core_spec;
use saka_core::time::Timestamp;
use saka_core::vocab::{SpecState, WorkItemState};
use saka_store::list::{self, Key, Page, PermissionFilter, WorkItemFilter};
use saka_store::read::{RunOf, TimelineCursor, TimelinePage};
use saka_store::{SpecRevisionRow, StoredEvent, TaskSet, read};
use serde_json::Value;

use crate::{Engine, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectView {
    pub project: Project,
    pub repos: Vec<Repo>,
    pub warnings: Vec<ScopeWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeWarning {
    pub repo: String,
    pub repo_id: RepoId,
    pub other_project_id: ProjectId,
    pub other_project: String,
    pub other_repo: String,
}

pub(crate) fn scope_warnings(project: &Project, others: &[Project]) -> Vec<ScopeWarning> {
    let mut out = Vec::new();
    for binding in &project.repos {
        let mine = project.effective_scope(binding);
        for other in others.iter().filter(|o| o.id != project.id) {
            for theirs in other.repos.iter().filter(|r| r.repo_id == binding.repo_id) {
                if saka_core::scope::overlaps(&mine, &other.effective_scope(theirs)) {
                    out.push(ScopeWarning {
                        repo: binding.name.clone(),
                        repo_id: binding.repo_id,
                        other_project_id: other.id,
                        other_project: other.name.clone(),
                        other_repo: theirs.name.clone(),
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| {
        (
            a.repo.as_str(),
            a.other_project.as_str(),
            a.other_project_id,
        )
            .cmp(&(
                b.repo.as_str(),
                b.other_project.as_str(),
                b.other_project_id,
            ))
    });
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct RevisionInfo {
    pub id: SpecRevisionId,
    pub source_commit: String,
    pub content_hash: String,
    pub context_hash: String,
    pub context: Vec<ContextRef>,
    pub lint_ok: bool,
    pub issues: Value,
    pub model: Value,
    pub parser_version: u32,
    pub created_at: Timestamp,
}

impl From<&SpecRevisionRow> for RevisionInfo {
    fn from(r: &SpecRevisionRow) -> RevisionInfo {
        RevisionInfo {
            id: SpecRevisionId(r.id),
            source_commit: r.source_commit.clone(),
            content_hash: r.content_hash.clone(),
            context_hash: r.context_hash.clone(),
            context: r.context.clone(),
            lint_ok: r.lint_ok,
            issues: r.lint_issues.clone(),
            model: r.model.clone(),
            parser_version: r.parser_version,
            created_at: r.created_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpecView {
    pub spec: Spec,
    pub revision: RevisionInfo,
    pub approved_revision: Option<RevisionRef>,
    pub work_items: Vec<StepItem>,
    pub step_repos: Vec<(String, Option<RepoId>)>,
    pub depends_on: Vec<DependencyView>,
    pub revision_plan: Option<RevisionPlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepItem {
    pub step_id: String,
    pub work_item_id: WorkItemId,
    pub repo_id: RepoId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionRef {
    pub id: SpecRevisionId,
    pub source_commit: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DependencyView {
    pub slug: String,
    pub spec_id: Option<SpecId>,
    pub state: Option<SpecState>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskView {
    pub task: Task,
    pub repo_name: Option<String>,
    pub waiting_on: Vec<Waiting>,
    pub budget: Option<BudgetView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BudgetView {
    pub stage_run_id: saka_core::ids::StageRunId,
    pub warn_percent: u32,
    pub limit: Budget,
    pub used: Usage,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccountView {
    pub cli: String,
    pub name: String,
    pub paused: bool,
    pub windows: Vec<AccountQuota>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageView {
    pub record: UsageRecord,
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionView {
    pub request: PermissionRequest,
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskDetail {
    pub task: Task,
    pub repo_name: Option<String>,
    pub stage_runs: Vec<StageRun>,
    pub waiting_on: Vec<Waiting>,
    pub budget: Option<BudgetView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionView {
    pub session: Session,
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Paged<T> {
    pub items: Vec<T>,
    pub next: Option<Key>,
}

pub(crate) fn step_items(tasks: &[Task]) -> Vec<StepItem> {
    tasks
        .iter()
        .filter(|t| t.item.state != WorkItemState::Superseded)
        .filter_map(|t| {
            t.item.step_id.clone().map(|step_id| StepItem {
                step_id,
                work_item_id: t.item.id,
                repo_id: t.item.repo_id,
            })
        })
        .collect()
}

pub(crate) fn paged<T>(mut rows: Vec<T>, page: &Page, key: impl Fn(&T) -> Key) -> Paged<T> {
    let limit = usize::try_from(page.limit).unwrap_or(usize::MAX);
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more { rows.last().map(key) } else { None };
    Paged { items: rows, next }
}

pub(crate) fn probe(page: &Page) -> Page {
    Page {
        after: page.after,
        limit: page.limit.saturating_add(1),
    }
}

impl Engine {
    pub(crate) fn project_view(&self, project: Project) -> Result<ProjectView> {
        self.project_view_with(project, &[])
    }

    pub(crate) fn project_view_with(
        &self,
        project: Project,
        unsaved: &[Repo],
    ) -> Result<ProjectView> {
        let mut ids = vec![project.workspace_repo_id];
        for r in &project.repos {
            if !ids.contains(&r.repo_id) {
                ids.push(r.repo_id);
            }
        }
        let bound: Vec<RepoId> = project.repos.iter().map(|r| r.repo_id).collect();
        let (mut repos, others) = self.store.read(|c| {
            Ok((
                list::repos(c, &ids)?,
                list::projects_bound_to(c, &bound, project.id)?,
            ))
        })?;
        for r in unsaved {
            if ids.contains(&r.id) && !repos.iter().any(|known| known.id == r.id) {
                repos.push(r.clone());
            }
        }
        repos.sort_by_key(|r| ids.iter().position(|id| *id == r.id));
        let warnings = scope_warnings(&project, &others);
        Ok(ProjectView {
            project,
            repos,
            warnings,
        })
    }

    pub fn projects(&self, page: &Page) -> Result<Paged<ProjectView>> {
        let rows = self.store.read(|c| list::projects(c, &probe(page)))?;
        let paged = paged(rows, page, |p| (p.created_at.0, p.id.0));
        Ok(Paged {
            items: paged
                .items
                .into_iter()
                .map(|p| self.project_view(p))
                .collect::<Result<_>>()?,
            next: paged.next,
        })
    }

    pub fn project(&self, id: ProjectId) -> Result<Option<ProjectView>> {
        match self.store.read(|c| read::load_project(c, id))? {
            Some(p) => self.project_view(p).map(Some),
            None => Ok(None),
        }
    }

    pub(crate) fn spec_view(&self, spec: Spec) -> Result<SpecView> {
        let (revision, tasks) = self.store.read(|c| {
            Ok((
                read::load_revision(c, spec.latest_revision_id)?,
                read::load_tasks(c, TaskSet::Spec(spec.id))?,
            ))
        })?;
        let revision = revision
            .as_ref()
            .map(RevisionInfo::from)
            .ok_or_else(|| crate::EngineError::Internal("spec without its revision".into()))?;
        self.spec_view_with(spec, revision, step_items(&tasks))
    }

    pub(crate) fn spec_view_with(
        &self,
        spec: Spec,
        revision: RevisionInfo,
        work_items: Vec<StepItem>,
    ) -> Result<SpecView> {
        let (approved_revision, declared) = match spec.approved_revision_id {
            Some(id) if id != revision.id => {
                let row = self.store.read(|c| read::load_revision(c, id))?;
                (
                    row.as_ref().map(|r| RevisionRef {
                        id,
                        source_commit: r.source_commit.clone(),
                        content_hash: r.content_hash.clone(),
                    }),
                    row.map(|r| r.model).unwrap_or_default(),
                )
            }
            Some(id) => (
                Some(RevisionRef {
                    id,
                    source_commit: revision.source_commit.clone(),
                    content_hash: revision.content_hash.clone(),
                }),
                revision.model.clone(),
            ),
            None => (None, revision.model.clone()),
        };
        let slugs: Vec<String> = declared
            .get("depends_on")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let depends_on = self.store.read(|c| {
            slugs
                .iter()
                .map(|slug| {
                    let target = list::spec_by_slug(c, spec.project_id, slug)?;
                    Ok(DependencyView {
                        slug: slug.clone(),
                        spec_id: target.as_ref().map(|t| t.id),
                        state: target.map(|t| t.state),
                    })
                })
                .collect::<saka_store::Result<Vec<_>>>()
        })?;
        let revision_plan = self.revision_plan(&spec, &revision)?;
        let step_repos = self.step_repos(&spec, &revision, &work_items)?;
        Ok(SpecView {
            spec,
            revision,
            approved_revision,
            work_items,
            step_repos,
            depends_on,
            revision_plan,
        })
    }

    fn step_repos(
        &self,
        spec: &Spec,
        revision: &RevisionInfo,
        work_items: &[StepItem],
    ) -> Result<Vec<(String, Option<RepoId>)>> {
        let steps: Vec<StepPlan> = revision
            .model
            .get("steps")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let project = self
            .store
            .read(|c| read::load_project(c, spec.project_id))?;
        Ok(steps
            .into_iter()
            .map(|step| {
                let repo_id = match work_items.iter().find(|w| w.step_id == step.id) {
                    Some(item) => Some(item.repo_id),
                    None => project
                        .as_ref()
                        .and_then(|p| step_repo(&p.repos, step.repo.as_deref())),
                };
                (step.id, repo_id)
            })
            .collect())
    }

    fn revision_plan(&self, spec: &Spec, latest: &RevisionInfo) -> Result<Option<RevisionPlan>> {
        let Some(approved_id) = spec
            .approved_revision_id
            .filter(|id| spec.state == SpecState::Approved && *id != latest.id)
        else {
            return Ok(None);
        };
        let (approved, tasks) = self.store.read(|c| {
            Ok((
                read::revision_view(c, approved_id)?,
                read::load_tasks(c, TaskSet::Spec(spec.id))?,
            ))
        })?;
        let Some(approved) = approved else {
            return Ok(None);
        };
        let latest = read::revision_view_of(
            latest.id,
            latest.lint_ok,
            &latest.issues,
            &latest.model,
            latest.parser_version,
        )?;
        let items: Vec<_> = tasks.into_iter().map(|t| t.item).collect();
        Ok(core_spec::revision_plan(
            spec,
            &approved,
            &latest,
            &items,
            saka_spec::PARSER_VERSION,
        ))
    }

    fn waiting(&self, tasks: &[Task]) -> Result<Vec<Vec<Waiting>>> {
        let queued: Vec<WorkItemId> = tasks
            .iter()
            .filter(|t| t.item.state == WorkItemState::Queued)
            .map(|t| t.item.id)
            .collect();
        if queued.is_empty() {
            return Ok(vec![Vec::new(); tasks.len()]);
        }
        let (item_deps, spec_deps) = self.store.read(|c| {
            Ok((
                read::work_item_dependencies_of(c, &queued)?,
                read::spec_dependencies(c, None)?,
            ))
        })?;
        let targets: Vec<WorkItemId> = item_deps
            .iter()
            .map(|d| d.depends_on_work_item_id)
            .collect();
        let (dep_tasks, specs) = self.store.read(|c| {
            let dep_tasks = read::load_tasks(c, TaskSet::Ids(&targets))?;
            let mut specs = Vec::new();
            for d in &spec_deps {
                if let Some(s) = read::load_spec(c, d.depends_on_spec_id)? {
                    specs.push(s);
                }
            }
            Ok((dep_tasks, specs))
        })?;
        let quotas = self.store.read(read::account_quotas)?;
        let cfg = &self.cfg.loop_cfg;
        let quota = saka_core::admission::quota_waits(
            &quotas,
            &cfg.account,
            cfg.rate_limit_pause,
            self.now(),
        );
        let ports_full = {
            let live = self
                .store
                .read(|c| read::load_tasks(c, TaskSet::Reconcile))?;
            saka_core::reconcile::free_port_blocks(&saka_core::reconcile::Snapshot {
                tasks: live,
                port_blocks: Some(self.cfg.env.port_blocks()),
                ..Default::default()
            }) == Some(0)
        };
        Ok(tasks
            .iter()
            .map(|t| {
                let mut waits = waiting_on(t, &dep_tasks, &specs, &item_deps, &spec_deps);
                if t.item.state == WorkItemState::Queued {
                    waits.extend(quota.iter().cloned());
                    if ports_full {
                        waits.push(Waiting::PortBlock);
                    }
                }
                waits
            })
            .collect())
    }

    pub(crate) fn budget_view(&self, task: &Task) -> Option<BudgetView> {
        let run = task.stage_run.as_ref().filter(|r| {
            matches!(
                r.state,
                saka_core::vocab::StageRunState::Pending | saka_core::vocab::StageRunState::Running
            )
        })?;
        Some(BudgetView {
            stage_run_id: run.id,
            warn_percent: self.cfg.loop_cfg.budget_warn_percent,
            limit: run.budget?,
            used: task.stage_usage(self.now()),
        })
    }

    pub fn sessions_of(&self, of: RunOf) -> Result<Vec<SessionView>> {
        Ok(self
            .store
            .read(|c| read::sessions(c, of))?
            .into_iter()
            .map(|(session, project_id)| SessionView {
                session,
                project_id,
            })
            .collect())
    }

    pub fn timeline(
        &self,
        session: SessionId,
        cursor: TimelineCursor,
        limit: u32,
    ) -> Result<Option<TimelinePage>> {
        if self.session(session)?.is_none() {
            return Ok(None);
        }
        Ok(Some(
            self.store
                .read(|c| read::timeline(c, session, cursor, limit))?,
        ))
    }

    pub fn last_activity_seq(&self) -> Result<i64> {
        Ok(self.store.read(read::last_activity_seq)?)
    }

    pub fn activities_after(&self, seq: i64, limit: u32) -> Result<Vec<(i64, SessionActivity)>> {
        Ok(self.store.read(|c| read::activities_after(c, seq, limit))?)
    }

    pub fn usage_records(
        &self,
        of: Option<RunOf>,
        session: Option<SessionId>,
    ) -> Result<Vec<UsageView>> {
        Ok(self
            .store
            .read(|c| read::usage_records(c, of, session))?
            .into_iter()
            .map(|(record, project_id)| UsageView { record, project_id })
            .collect())
    }

    pub fn permission_rules(&self, project: ProjectId) -> Result<Option<Vec<PermissionRule>>> {
        if self
            .store
            .read(|c| read::load_project(c, project))?
            .is_none()
        {
            return Ok(None);
        }
        Ok(Some(
            self.store
                .read(|c| read::active_permission_rules(c, project))?,
        ))
    }

    pub fn agent_accounts(&self) -> Result<Vec<AccountView>> {
        let quotas = self.store.read(read::account_quotas)?;
        let cfg = &self.cfg.loop_cfg;
        let mut names: Vec<String> = vec![cfg.account.clone()];
        for q in &quotas {
            if !names.contains(&q.account) {
                names.push(q.account.clone());
            }
        }
        let now = self.now();
        Ok(names
            .into_iter()
            .map(|account| {
                let (cli, name) = account
                    .split_once(':')
                    .map_or((account.clone(), String::new()), |(c, n)| {
                        (c.to_string(), n.to_string())
                    });
                let paused = saka_core::admission::paused_windows(
                    &quotas,
                    &account,
                    cfg.rate_limit_pause,
                    now,
                )
                .next()
                .is_some();
                AccountView {
                    cli,
                    name,
                    paused,
                    windows: quotas
                        .iter()
                        .filter(|q| q.account == account)
                        .cloned()
                        .collect(),
                }
            })
            .collect())
    }

    pub fn specs(
        &self,
        project: ProjectId,
        states: &[String],
        path: Option<&str>,
        page: &Page,
    ) -> Result<Paged<SpecView>> {
        let rows = self
            .store
            .read(|c| list::specs(c, project, states, path, &probe(page)))?;
        let paged = paged(rows, page, |s| (s.updated_at.0, s.id.0));
        Ok(Paged {
            items: paged
                .items
                .into_iter()
                .map(|s| self.spec_view(s))
                .collect::<Result<_>>()?,
            next: paged.next,
        })
    }

    pub fn spec(&self, id: SpecId) -> Result<Option<SpecView>> {
        match self.store.read(|c| read::load_spec(c, id))? {
            Some(s) => self.spec_view(s).map(Some),
            None => Ok(None),
        }
    }

    pub fn work_items(&self, filter: &WorkItemFilter, page: &Page) -> Result<Paged<TaskView>> {
        let rows = self
            .store
            .read(|c| list::work_items(c, filter, &probe(page)))?;
        let paged = paged(rows, page, |(_, key)| *key);
        let ids: Vec<WorkItemId> = paged.items.iter().map(|(id, _)| *id).collect();
        let mut tasks = self
            .store
            .read(|c| read::load_tasks(c, TaskSet::Ids(&ids)))?;
        tasks.sort_by_key(|t| ids.iter().position(|id| *id == t.item.id));
        let waiting = self.waiting(&tasks)?;
        let names = self.repo_names(&tasks)?;
        Ok(Paged {
            items: tasks
                .into_iter()
                .zip(waiting)
                .zip(names)
                .map(|((task, waiting_on), repo_name)| TaskView {
                    budget: self.budget_view(&task),
                    task,
                    repo_name,
                    waiting_on,
                })
                .collect(),
            next: paged.next,
        })
    }

    fn repo_names(&self, tasks: &[Task]) -> Result<Vec<Option<String>>> {
        let mut projects: Vec<Project> = Vec::new();
        let mut names = Vec::with_capacity(tasks.len());
        for t in tasks {
            let id = t.item.project_id;
            if !projects.iter().any(|p| p.id == id)
                && let Some(p) = self.store.read(|c| read::load_project(c, id))?
            {
                projects.push(p);
            }
            names.push(
                projects
                    .iter()
                    .find(|p| p.id == id)
                    .and_then(|p| p.binding(t.item.repo_id))
                    .map(|b| b.name.clone()),
            );
        }
        Ok(names)
    }

    pub fn work_item(&self, id: WorkItemId) -> Result<Option<TaskDetail>> {
        let (task, stage_runs) = self
            .store
            .read(|c| Ok((read::load_task(c, id)?, list::stage_runs(c, id)?)))?;
        let Some(task) = task else {
            return Ok(None);
        };
        let waiting_on = self
            .waiting(std::slice::from_ref(&task))?
            .into_iter()
            .next()
            .unwrap_or_default();
        let repo_name = self
            .repo_names(std::slice::from_ref(&task))?
            .into_iter()
            .next()
            .flatten();
        Ok(Some(TaskDetail {
            budget: self.budget_view(&task),
            task,
            repo_name,
            stage_runs,
            waiting_on,
        }))
    }

    pub fn permission_requests(
        &self,
        filter: &PermissionFilter,
        page: &Page,
    ) -> Result<Paged<PermissionView>> {
        let rows = self
            .store
            .read(|c| list::permissions(c, filter, &probe(page)))?;
        let paged = paged(rows, page, |(p, _)| (p.requested_at.0, p.id.0));
        Ok(Paged {
            items: paged
                .items
                .into_iter()
                .map(|(request, project_id)| PermissionView {
                    request,
                    project_id,
                })
                .collect(),
            next: paged.next,
        })
    }

    pub fn permission(&self, id: PermissionRequestId) -> Result<Option<PermissionView>> {
        Ok(self
            .store
            .read(|c| list::load_permission(c, id))?
            .map(|(request, project_id)| PermissionView {
                request,
                project_id,
            }))
    }

    pub fn session(&self, id: SessionId) -> Result<Option<SessionView>> {
        Ok(self
            .store
            .read(|c| list::load_session(c, id))?
            .map(|(session, project_id)| SessionView {
                session,
                project_id,
            }))
    }

    pub fn events_after(&self, after: i64, limit: u32) -> Result<Vec<StoredEvent>> {
        Ok(self.store.read(|c| read::events_after(c, after, limit))?)
    }

    pub fn last_seq(&self) -> Result<i64> {
        Ok(self.store.read(list::last_seq)?)
    }
}
