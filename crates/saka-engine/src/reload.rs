use saka_core::event::Event;
use saka_core::ids::ProjectId;
use saka_core::job::{Job, JobKind};
use saka_core::model::{Project, ProjectRepo};
use saka_core::rejection::Rejection;
use saka_core::time::Timestamp;
use saka_core::vocab::{ReloadOutcome, ReloadTrigger};
use saka_core::workspace::{self, NewConfig};
use saka_store::{Batch, list, read};
use serde_json::json;

use crate::action::{Action, Produced};
use crate::commands::NewBinding;
use crate::register::{WorkspaceRepo, parse_repo_url};
use crate::views::ProjectView;
use crate::{Cause, Engine, EngineError, Result, project_config};

#[derive(Debug, Clone, PartialEq)]
pub struct ReloadView {
    pub project: ProjectView,
    pub outcome: ReloadOutcome,
}

pub(crate) fn reload_job(project: ProjectId, now: Timestamp, poll_secs: u32) -> Job {
    let period = i64::from(poll_secs.max(1)) * 1000;
    Job::new(
        JobKind::WorkspaceReloadConfig,
        format!("{project}:{}", now.0.div_euclid(period)),
        json!({ "project_id": project }),
        now,
    )
}

struct Changes {
    added: Vec<String>,
    removed: Vec<String>,
    changed: Vec<String>,
}

fn changes(before: &[ProjectRepo], after: &[ProjectRepo]) -> Changes {
    let find = |list: &[ProjectRepo], r: &ProjectRepo| {
        list.iter().find(|x| x.repo_id == r.repo_id).cloned()
    };
    Changes {
        added: after
            .iter()
            .filter(|r| find(before, r).is_none())
            .map(|r| r.name.clone())
            .collect(),
        removed: before
            .iter()
            .filter(|r| find(after, r).is_none())
            .map(|r| r.name.clone())
            .collect(),
        changed: after
            .iter()
            .filter(|r| find(before, r).is_some_and(|old| old != **r))
            .map(|r| r.name.clone())
            .collect(),
    }
}

impl Engine {
    pub async fn reload_project_config(
        &self,
        project_id: ProjectId,
        trigger: ReloadTrigger,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<ReloadView> {
        let result = self.reload(project_id, trigger, cause, action).await;
        let outcome = match &result {
            Ok(v) => v.outcome.as_str(),
            Err(EngineError::Rejected(_)) => "rejected",
            Err(_) => "failed",
        };
        self.counters.config_reload(outcome);
        result
    }

    async fn reload(
        &self,
        project_id: ProjectId,
        trigger: ReloadTrigger,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<ReloadView> {
        let (project, workspace) = self.workspace_of(project_id)?;
        let repo = self
            .store
            .read(|c| read::load_repo(c, project.workspace_repo_id))?
            .ok_or_else(|| EngineError::Internal("project without its workspace repo".into()))?;
        let file = self
            .read_config(&workspace, &project.base_branch, &project.project_path)
            .await?;
        let commit = file.commit.clone();
        let reject = |r: Rejection| self.reject_config(project_id, &commit, r, trigger, cause);
        let bytes = match file.bytes() {
            Ok(b) => b,
            Err(r) => return Err(reject(r)),
        };
        let sha256 = project_config::sha256(&bytes);
        if sha256 == project.config_sha256 {
            return self.config_unchanged(project_id, &commit, cause, action);
        }
        let config = match project_config::parse(&bytes) {
            Ok(c) => c,
            Err(issues) => return Err(reject(crate::register::config_invalid(&issues))),
        };
        let host = parse_repo_url(&repo.url)
            .map(|(h, _, _)| h)
            .unwrap_or_default();
        let ws = WorkspaceRepo {
            repo: &workspace,
            host: &host,
            default_branch: &repo.default_branch,
            provider: &repo.provider,
            base_branch: &project.base_branch,
        };
        let bindings = match self.resolve_bindings(&config, &ws).await {
            Ok(b) => b,
            Err(EngineError::Rejected(r)) => return Err(reject(r)),
            Err(e) => return Err(e),
        };
        let applied = NewConfig {
            name: &config.name,
            commit: &commit,
            sha256: &sha256,
            workflow: config.workflow.clone(),
            repos: Vec::new(),
            trigger,
        };
        self.apply_config(project_id, applied, &bindings, cause, action)
    }

    fn config_unchanged(
        &self,
        project_id: ProjectId,
        commit: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<ReloadView> {
        let _guard = self.locked()?;
        let project = self.current_project(project_id)?;
        let events = workspace::config_unchanged(&project, commit);
        self.commit_reload(project, &events, ReloadOutcome::Unchanged, cause, action)
    }

    fn apply_config(
        &self,
        project_id: ProjectId,
        mut config: NewConfig<'_>,
        bindings: &[NewBinding],
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<ReloadView> {
        let guard = self.locked()?;
        let project = self.current_project(project_id)?;
        if project.config_sha256 == config.sha256 {
            let events = workspace::config_unchanged(&project, config.commit);
            return self.commit_reload(project, &events, ReloadOutcome::Unchanged, cause, action);
        }
        let mut events = Vec::new();
        let (repos, _) = self.bind_repos(bindings, None, &mut events)?;
        let gone: Vec<_> = project
            .repos
            .iter()
            .filter(|old| !repos.iter().any(|r| r.repo_id == old.repo_id))
            .map(|old| old.repo_id)
            .collect();
        let tasks = self
            .store
            .read(|c| list::repo_tasks(c, project_id, &gone))?;
        if let Some(rejection) = workspace::removed_in_use(&project, &repos, &tasks) {
            drop(guard);
            return Err(self.reject_config(
                project_id,
                config.commit,
                rejection,
                config.trigger,
                cause,
            ));
        }
        let diff = changes(&project.repos, &repos);
        config.repos = repos;
        let (commit, trigger) = (config.commit.to_string(), config.trigger);
        let changed = workspace::config_changed(project_id, config).ok_or_else(|| {
            EngineError::Internal("project config with invalid bindings passed parsing".into())
        })?;
        events.push(changed);
        let view = self.commit_reload(project, &events, ReloadOutcome::Changed, cause, action)?;
        tracing::info!(
            project_id = %project_id,
            commit,
            added = ?diff.added,
            removed = ?diff.removed,
            changed = ?diff.changed,
            trigger = trigger.as_str(),
            "workspace.config.changed"
        );
        Ok(view)
    }

    fn current_project(&self, project_id: ProjectId) -> Result<Project> {
        self.store
            .read(|c| read::load_project(c, project_id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))
    }

    fn commit_reload(
        &self,
        project: Project,
        events: &[Event],
        outcome: ReloadOutcome,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<ReloadView> {
        let project_id = project.id;
        let mut batch = Batch::new(self.now());
        let after = if events.is_empty() {
            project
        } else {
            self.evolve_workspace(&mut batch, Some(project), events, project_id, cause)
                .ok_or_else(|| EngineError::Internal("reload dropped the project".into()))?
        };
        let view = ReloadView {
            project: self.project_view_with(after, &batch.repos)?,
            outcome,
        };
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Reload(&view));
            batch.action =
                Some(action.record(self, cause, Some(project_id), Some(project_id.0), reply));
        }
        if !events.is_empty() || batch.action.is_some() {
            self.commit(&batch)?;
        }
        Ok(view)
    }

    fn reject_config(
        &self,
        project_id: ProjectId,
        commit: &str,
        rejection: Rejection,
        trigger: ReloadTrigger,
        cause: &Cause,
    ) -> EngineError {
        tracing::warn!(
            project_id = %project_id,
            commit,
            code = rejection.code(),
            trigger = trigger.as_str(),
            "workspace.config.rejected"
        );
        let recorded = self.locked().and_then(|_guard| {
            let project = self.current_project(project_id)?;
            let events = workspace::config_rejected(&project, commit, &rejection, trigger);
            if events.is_empty() {
                return Ok(());
            }
            let mut batch = Batch::new(self.now());
            self.evolve_workspace(&mut batch, Some(project), &events, project_id, cause);
            self.commit(&batch).map(|_| ())
        });
        match recorded {
            Ok(()) => EngineError::Rejected(rejection),
            Err(e) => e,
        }
    }

    pub(crate) fn reload_jobs(&self, now: Timestamp) -> Result<Vec<(ProjectId, Job)>> {
        let secs = self.cfg.config_poll_secs;
        if secs == 0 {
            return Ok(Vec::new());
        }
        let ids = self.store.read(list::project_ids)?;
        Ok(ids
            .into_iter()
            .map(|id| (id, reload_job(id, now, secs)))
            .collect())
    }
}
