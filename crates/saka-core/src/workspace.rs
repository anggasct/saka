use crate::event::Event;
use crate::model::{Project, Repo};
use crate::time::Timestamp;
use crate::vocab::{Autonomy, WorkflowPreset};

pub fn evolve_repo(repo: Option<Repo>, event: &Event, now: Timestamp) -> Option<Repo> {
    match event {
        Event::RepoRegistered {
            repo_id,
            integration_id,
            provider,
            full_name,
            url,
            default_branch,
        } => Some(Repo {
            id: *repo_id,
            integration_id: integration_id.clone(),
            provider: provider.clone(),
            full_name: full_name.clone(),
            url: url.clone(),
            default_branch: default_branch.clone(),
            created_at: repo.as_ref().map_or(now, |r| r.created_at),
            updated_at: now,
        }),
        _ => repo,
    }
}

pub fn evolve_project(project: Option<Project>, event: &Event, now: Timestamp) -> Option<Project> {
    match event {
        Event::ProjectRegistered {
            project_id,
            name,
            workspace_repo_id,
            project_path,
            base_branch,
            config_commit,
            repos,
        } => Some(Project {
            id: *project_id,
            name: name.clone(),
            workspace_repo_id: *workspace_repo_id,
            project_path: project_path.clone(),
            base_branch: base_branch.clone(),
            config_commit: config_commit.clone(),
            autonomy: Autonomy::Standard,
            workflow_preset: WorkflowPreset::FullLoop,
            repos: repos.clone(),
            created_at: project.as_ref().map_or(now, |p| p.created_at),
            updated_at: now,
        }),
        _ => project,
    }
}
