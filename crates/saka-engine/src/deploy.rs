use std::collections::BTreeMap;

use saka_core::deploy::{CheckRecord, Deployment};
use saka_core::diagnostics::{
    self as eval, DeployEnvironmentFacts, DeployFacts, Finding, VerifyFacts,
};
use saka_core::environment::{DeployAction, Environment, VerifySpec};
use saka_core::ids::{DeploymentId, ProjectId, RepoId};
use saka_core::model::{Project, Repo};
use saka_core::rejection::Rejection;
use saka_core::vocab::{CheckId, DeployActionKind, OnFailure};
use saka_store::deployment::{DeploymentFilter, MergeRef, TaskRef};
use saka_store::list::Page;
use saka_store::{deployment, read};

use crate::views::Paged;
use crate::{Engine, EngineError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub id: RepoId,
    pub name: String,
    pub full_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerificationView {
    pub ready: bool,
    pub commands: usize,
    pub warnings: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnvironmentView {
    pub name: String,
    pub repo: RepoInfo,
    pub branch: String,
    pub action: DeployActionKind,
    pub target_url: Option<String>,
    pub gate: bool,
    pub on_failure: OnFailure,
    pub verification: VerificationView,
    pub queued: Option<Deployment>,
    pub active: Option<Deployment>,
    pub last_succeeded: Option<Deployment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeploymentRow {
    pub deployment: Deployment,
    pub repo: RepoInfo,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeploymentDetail {
    pub row: DeploymentRow,
    pub checks: Vec<CheckRecord>,
    pub merges: Vec<MergeRef>,
    pub tasks: Vec<TaskRef>,
}

pub(crate) fn verification_view(
    action: &DeployAction,
    verify: Option<&VerifySpec>,
) -> VerificationView {
    let mut warnings = Vec::new();
    if verify.is_none() {
        warnings.push("verify_missing");
    }
    let ready = verify.is_some_and(|v| v.ready.is_some());
    if matches!(action, DeployAction::Webhook { .. }) && !ready {
        warnings.push("ready_missing");
    }
    VerificationView {
        ready,
        commands: verify.map_or(0, |v| v.commands.len()),
        warnings,
    }
}

fn repo_info(project: &Project, repo: &Repo) -> RepoInfo {
    RepoInfo {
        id: repo.id,
        name: project
            .repos
            .iter()
            .find(|b| b.repo_id == repo.id)
            .map_or_else(|| repo.full_name.clone(), |b| b.name.clone()),
        full_name: repo.full_name.clone(),
    }
}

impl Engine {
    pub(crate) fn environments_of(&self, project: &Project) -> Vec<&Environment> {
        self.cfg
            .environments
            .iter()
            .filter(|e| e.is_for(project.id, &project.name))
            .collect()
    }

    fn load_project_row(&self, id: ProjectId) -> Result<Project> {
        self.store
            .read(|c| read::load_project(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))
    }

    fn repo_infos(&self, project: &Project) -> Result<BTreeMap<RepoId, RepoInfo>> {
        let repos = self.store.read(|c| {
            project
                .repos
                .iter()
                .map(|b| read::load_repo(c, b.repo_id))
                .collect::<std::result::Result<Vec<_>, _>>()
        })?;
        Ok(repos
            .into_iter()
            .flatten()
            .map(|r| (r.id, repo_info(project, &r)))
            .collect())
    }

    pub fn environments(&self, project_id: ProjectId) -> Result<Vec<EnvironmentView>> {
        let project = self.load_project_row(project_id)?;
        let infos = self.repo_infos(&project)?;
        let mut views = Vec::new();
        for env in self.environments_of(&project) {
            let Ok(bound) = env.bind(&project.repos) else {
                continue;
            };
            let Some(repo) = infos.get(&bound.repo.repo_id) else {
                continue;
            };
            let (queued, active, last_succeeded) = self.store.read(|c| {
                Ok((
                    deployment::queued_of(c, project.id, &env.name)?,
                    deployment::active_of(c, project.id, &env.name)?,
                    deployment::last_succeeded(c, project.id, &env.name)?,
                ))
            })?;
            views.push(EnvironmentView {
                name: env.name.clone(),
                repo: repo.clone(),
                branch: bound.branch,
                action: env.action.kind(),
                target_url: env.target_url.clone(),
                gate: env.gate,
                on_failure: env.on_failure,
                verification: verification_view(&env.action, project.verify.get(&env.name)),
                queued,
                active,
                last_succeeded,
            });
        }
        views.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(views)
    }

    pub fn deployments(
        &self,
        project_id: ProjectId,
        environment: Option<String>,
        state: Option<saka_core::vocab::DeploymentState>,
        page: &Page,
    ) -> Result<Paged<DeploymentRow>> {
        let project = self.load_project_row(project_id)?;
        let infos = self.repo_infos(&project)?;
        let filter = DeploymentFilter {
            project: Some(project_id),
            environment,
            state,
        };
        let rows = self.store.read(|c| {
            deployment::list(
                c,
                &filter,
                &Page {
                    after: page.after,
                    limit: page.limit.saturating_add(1),
                },
            )
        })?;
        let paged = crate::views::paged(rows, page, |(_, key)| *key);
        let mut items = Vec::new();
        for (deployment, _) in paged.items {
            let repo = infos
                .get(&deployment.repo_id)
                .cloned()
                .ok_or_else(|| EngineError::Internal("deployment without its repo".into()))?;
            items.push(DeploymentRow { deployment, repo });
        }
        Ok(Paged {
            items,
            next: paged.next,
        })
    }

    pub fn deployment(&self, id: DeploymentId) -> Result<DeploymentDetail> {
        let found = self
            .store
            .read(|c| deployment::load(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        let project = self.load_project_row(found.project_id)?;
        let repo = self
            .repo_infos(&project)?
            .remove(&found.repo_id)
            .ok_or_else(|| EngineError::Internal("deployment without its repo".into()))?;
        let (checks, merges, tasks) = self.store.read(|c| {
            Ok((
                deployment::checks(c, id)?,
                deployment::merge_refs(c, id)?,
                deployment::tasks_of(c, id)?,
            ))
        })?;
        Ok(DeploymentDetail {
            row: DeploymentRow {
                deployment: found,
                repo,
            },
            checks,
            merges,
            tasks,
        })
    }
}

fn secret_names(env: &Environment) -> Vec<String> {
    let own: Vec<String> = match &env.action {
        DeployAction::Run { secrets, .. } => secrets.clone(),
        DeployAction::Webhook { secret, .. } => vec![secret.clone()],
        DeployAction::RunHost { .. } => Vec::new(),
    };
    let mut names: Vec<String> = own.into_iter().chain(env.verify_secrets.clone()).collect();
    names.sort();
    names.dedup();
    names
}

impl Engine {
    async fn deploy_facts(&self, project: &Project, remote: bool) -> Result<DeployFacts> {
        let stored: Vec<String> = if remote {
            self.store
                .read(|c| read::project_secrets(c, project.id))?
                .into_iter()
                .map(|s| s.name)
                .collect()
        } else {
            Vec::new()
        };
        let mut environments = Vec::new();
        for env in self.environments_of(project) {
            let bound = env.bind(&project.repos);
            let branch_found = match (&bound, remote) {
                (Ok(b), true) => self.deploy_branch_found(b.repo.repo_id, &b.branch).await,
                _ => None,
            };
            environments.push(DeployEnvironmentFacts {
                name: env.name.clone(),
                action: env.action.kind(),
                binding: bound.map(|b| (b.repo.name.clone(), b.branch)),
                branch_found,
                missing_secrets: if remote {
                    secret_names(env)
                        .into_iter()
                        .filter(|n| !stored.contains(n))
                        .collect()
                } else {
                    Vec::new()
                },
                verify: project.verify.get(&env.name).map(|v| VerifyFacts {
                    has_ready: v.ready.is_some(),
                }),
            });
        }
        let unknown_verify = project
            .verify
            .keys()
            .filter(|name| !environments.iter().any(|e| &e.name == *name))
            .cloned()
            .collect();
        Ok(DeployFacts {
            environments,
            unknown_verify,
        })
    }

    async fn deploy_branch_found(&self, repo_id: RepoId, branch: &str) -> Option<bool> {
        let repo = self.repo_ref_of(repo_id).ok()?;
        match self.ports.env.branch_tip(&repo, branch).await {
            Ok(tip) => Some(tip.is_some()),
            Err(_) => None,
        }
    }

    pub(crate) async fn deploy_findings(&self, project: &Project, id: CheckId) -> Vec<Finding> {
        let remote = id == CheckId::DeployConfig;
        let facts = match self.deploy_facts(project, remote).await {
            Ok(facts) => facts,
            Err(EngineError::Store(e)) => return vec![eval::check_failed(e.code())],
            Err(_) => return vec![eval::check_failed("deploy_facts_failed")],
        };
        match id {
            CheckId::DeployConfig => eval::deploy_config(&facts),
            CheckId::DeployVerifyMissing => eval::deploy_verify_missing(&facts),
            _ => eval::deploy_ready_missing(&facts),
        }
    }
}
