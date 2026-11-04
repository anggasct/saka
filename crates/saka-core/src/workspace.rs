use std::collections::BTreeSet;

use serde_json::json;

use crate::event::Event;
use crate::ids::{ProjectId, RepoId, WorkItemId};
use crate::model::{ConfigError, Project, ProjectRepo, Repo};
use crate::rejection::Rejection;
use crate::time::Timestamp;
use crate::vocab::{Autonomy, ReloadTrigger, WorkItemState, WorkflowPreset};

pub const MAX_BINDINGS: usize = 16;

pub fn bindings_hold(repos: &[ProjectRepo]) -> bool {
    let names: BTreeSet<&str> = repos.iter().map(|r| r.name.as_str()).collect();
    let ids: BTreeSet<RepoId> = repos.iter().map(|r| r.repo_id).collect();
    (1..=MAX_BINDINGS).contains(&repos.len())
        && repos.iter().filter(|r| r.is_default).count() == 1
        && names.len() == repos.len()
        && ids.len() == repos.len()
}

pub fn holds_repo(state: WorkItemState) -> bool {
    matches!(
        state,
        WorkItemState::Queued
            | WorkItemState::AwaitingStart
            | WorkItemState::Admitted
            | WorkItemState::InStage
            | WorkItemState::Blocked
            | WorkItemState::TakenOver
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoTask {
    pub work_item_id: WorkItemId,
    pub task_key: String,
    pub state: WorkItemState,
    pub repo_id: RepoId,
}

pub fn removed_in_use(
    project: &Project,
    next: &[ProjectRepo],
    tasks: &[RepoTask],
) -> Option<Rejection> {
    let repos: Vec<_> = project
        .repos
        .iter()
        .filter(|old| !next.iter().any(|n| n.repo_id == old.repo_id))
        .filter_map(|old| {
            let held: Vec<_> = tasks
                .iter()
                .filter(|t| t.repo_id == old.repo_id && holds_repo(t.state))
                .map(|t| {
                    json!({
                        "work_item_id": t.work_item_id,
                        "task_key": t.task_key,
                        "state": t.state,
                    })
                })
                .collect();
            (!held.is_empty())
                .then(|| json!({ "name": old.name, "repo_id": old.repo_id, "tasks": held }))
        })
        .collect();
    (!repos.is_empty()).then_some(Rejection::ProjectRepoInUse {
        repos: serde_json::Value::Array(repos),
    })
}

pub fn config_unchanged(project: &Project, commit: &str) -> Vec<Event> {
    match project.config_error {
        Some(_) => vec![Event::ProjectConfigErrorCleared {
            project_id: project.id,
            commit: commit.to_string(),
        }],
        None => Vec::new(),
    }
}

pub fn config_rejected(
    project: &Project,
    commit: &str,
    rejection: &Rejection,
    trigger: ReloadTrigger,
) -> Vec<Event> {
    let code = rejection.code();
    let recorded = project
        .config_error
        .as_ref()
        .is_some_and(|e| e.commit == commit && e.code == code);
    if recorded {
        return Vec::new();
    }
    vec![Event::ProjectConfigRejected {
        project_id: project.id,
        commit: commit.to_string(),
        code: code.to_string(),
        params: rejection.params(),
        trigger,
    }]
}

pub struct NewConfig<'a> {
    pub name: &'a str,
    pub commit: &'a str,
    pub sha256: &'a str,
    pub repos: Vec<ProjectRepo>,
    pub trigger: ReloadTrigger,
}

pub fn config_changed(project_id: ProjectId, config: NewConfig<'_>) -> Option<Event> {
    bindings_hold(&config.repos).then(|| Event::ProjectConfigChanged {
        project_id,
        name: config.name.to_string(),
        config_commit: config.commit.to_string(),
        config_sha256: config.sha256.to_string(),
        repos: config.repos,
        trigger: config.trigger,
    })
}

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
            config_sha256,
            repos,
        } => Some(Project {
            id: *project_id,
            name: name.clone(),
            workspace_repo_id: *workspace_repo_id,
            project_path: project_path.clone(),
            base_branch: base_branch.clone(),
            config_commit: config_commit.clone(),
            config_sha256: config_sha256.clone(),
            config_error: None,
            autonomy: Autonomy::Standard,
            workflow_preset: WorkflowPreset::FullLoop,
            repos: sorted(repos),
            created_at: project.as_ref().map_or(now, |p| p.created_at),
            updated_at: now,
        }),
        Event::ProjectConfigChanged {
            name,
            config_commit,
            config_sha256,
            repos,
            ..
        } => project.map(|p| Project {
            name: name.clone(),
            config_commit: config_commit.clone(),
            config_sha256: config_sha256.clone(),
            config_error: None,
            repos: sorted(repos),
            updated_at: now,
            ..p
        }),
        Event::ProjectConfigRejected {
            commit,
            code,
            params,
            ..
        } => project.map(|p| Project {
            config_error: Some(ConfigError {
                commit: commit.clone(),
                code: code.clone(),
                params: params.clone(),
                observed_at: now,
            }),
            updated_at: now,
            ..p
        }),
        Event::ProjectConfigErrorCleared { .. } => project.map(|p| Project {
            config_error: None,
            updated_at: now,
            ..p
        }),
        _ => project,
    }
}

fn sorted(repos: &[ProjectRepo]) -> Vec<ProjectRepo> {
    let mut out = repos.to_vec();
    out.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}
