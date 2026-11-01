use saka_core::admission::{Waiting, waiting_on};
use saka_core::ids::{
    PermissionRequestId, ProjectId, SessionId, SpecId, SpecRevisionId, WorkItemId,
};
use saka_core::model::{PermissionRequest, Project, Repo, Session, Spec, StageRun, Task};
use saka_core::time::Timestamp;
use saka_core::vocab::{SpecState, WorkItemState};
use saka_store::list::{self, Key, Page, PermissionFilter, WorkItemFilter};
use saka_store::{SpecRevisionRow, StoredEvent, TaskSet, read};
use serde_json::Value;

use crate::{Engine, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectView {
    pub project: Project,
    pub repos: Vec<Repo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RevisionInfo {
    pub id: SpecRevisionId,
    pub source_commit: String,
    pub content_hash: String,
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
    pub work_items: Vec<(String, WorkItemId)>,
    pub depends_on: Vec<DependencyView>,
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
    pub waiting_on: Vec<Waiting>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PermissionView {
    pub request: PermissionRequest,
    pub project_id: ProjectId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskDetail {
    pub task: Task,
    pub stage_runs: Vec<StageRun>,
    pub waiting_on: Vec<Waiting>,
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

fn paged<T>(mut rows: Vec<T>, page: &Page, key: impl Fn(&T) -> Key) -> Paged<T> {
    let limit = usize::try_from(page.limit).unwrap_or(usize::MAX);
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more { rows.last().map(key) } else { None };
    Paged { items: rows, next }
}

fn probe(page: &Page) -> Page {
    Page {
        after: page.after,
        limit: page.limit.saturating_add(1),
    }
}

impl Engine {
    pub(crate) fn project_view(&self, project: Project) -> Result<ProjectView> {
        let mut ids = vec![project.workspace_repo_id];
        ids.extend(project.repos.iter().map(|r| r.repo_id));
        ids.dedup();
        let mut repos = self.store.read(|c| list::repos(c, &ids))?;
        repos.sort_by_key(|r| ids.iter().position(|id| *id == r.id));
        Ok(ProjectView { project, repos })
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
        let work_items = tasks
            .iter()
            .filter_map(|t| t.item.step_id.clone().map(|s| (s, t.item.id)))
            .collect();
        self.spec_view_with(spec, revision, work_items)
    }

    pub(crate) fn spec_view_with(
        &self,
        spec: Spec,
        revision: RevisionInfo,
        work_items: Vec<(String, WorkItemId)>,
    ) -> Result<SpecView> {
        let declared = match spec.approved_revision_id {
            Some(id) if id != revision.id => self
                .store
                .read(|c| read::load_revision(c, id))?
                .map(|r| r.model)
                .unwrap_or_default(),
            _ => revision.model.clone(),
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
        Ok(SpecView {
            spec,
            revision,
            work_items,
            depends_on,
        })
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
        Ok(tasks
            .iter()
            .map(|t| waiting_on(t, &dep_tasks, &specs, &item_deps, &spec_deps))
            .collect())
    }

    pub fn specs(
        &self,
        project: ProjectId,
        states: &[String],
        page: &Page,
    ) -> Result<Paged<SpecView>> {
        let rows = self
            .store
            .read(|c| list::specs(c, project, states, &probe(page)))?;
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
        Ok(Paged {
            items: tasks
                .into_iter()
                .zip(waiting)
                .map(|(task, waiting_on)| TaskView { task, waiting_on })
                .collect(),
            next: paged.next,
        })
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
        Ok(Some(TaskDetail {
            task,
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
