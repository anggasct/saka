use saka_core::ids::{ProjectId, RepoId};
use saka_core::paths;
use saka_core::rejection::Rejection;
use saka_core::scope::PathScope;
use saka_ports::{PortError, RepoRef};
use saka_store::{Batch, list, read};
use serde_json::Value;

use crate::action::{Action, Produced};
use crate::commands::NewBinding;
use crate::project_config::{self, Issue};
use crate::register::{
    BranchLookup, MAX_CONFIG_BYTES, WorkspaceRepo, branch_ok, config_invalid, invalid,
    parse_repo_url,
};
use crate::{Cause, Engine, EngineError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckConfig {
    pub workspace_url: String,
    pub project_path: String,
    pub base_branch: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedWorkspace {
    pub integration_id: String,
    pub full_name: String,
    pub default_branch: String,
    pub base_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedRepo {
    pub name: String,
    pub integration_id: String,
    pub full_name: String,
    pub url: String,
    pub is_workspace: bool,
    pub is_default: bool,
    pub base_branch: String,
    pub path_scope: Vec<String>,
    pub effective_path_scope: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigProblem {
    pub code: String,
    pub params: Value,
}

impl ConfigProblem {
    fn from_rejection(rejection: &Rejection) -> ConfigProblem {
        ConfigProblem {
            code: rejection.code().to_string(),
            params: rejection.params(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteConfig {
    pub commit: String,
    pub present: bool,
    pub config_sha256: Option<String>,
    pub matches: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredProject {
    pub id: ProjectId,
    pub name: String,
    pub config_commit: String,
    pub config_sha256: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigCheckView {
    pub ok: bool,
    pub content_sha256: String,
    pub workspace: Option<CheckedWorkspace>,
    pub repos: Vec<CheckedRepo>,
    pub problems: Vec<ConfigProblem>,
    pub remote: Option<RemoteConfig>,
    pub project: Option<RegisteredProject>,
}

struct Workspace {
    repo: RepoRef,
    host: String,
    provider: String,
    default_branch: String,
    base_branch: String,
    branch_head: Option<String>,
    known: bool,
}

impl Workspace {
    fn as_repo(&self) -> WorkspaceRepo<'_> {
        WorkspaceRepo {
            repo: &self.repo,
            host: &self.host,
            default_branch: &self.default_branch,
            provider: &self.provider,
            base_branch: &self.base_branch,
        }
    }
}

enum WorkspaceLookup {
    Found(Workspace),
    Problem(Rejection),
}

fn checked_repo(
    binding: &NewBinding,
    workspace: Option<&Workspace>,
    project_path: &str,
) -> CheckedRepo {
    let is_workspace = workspace.is_some_and(|w| binding.repo.repo_id == Some(w.repo.repo_id));
    let effective = PathScope::effective(&binding.path_scope, is_workspace.then_some(project_path));
    CheckedRepo {
        name: binding.name.clone(),
        integration_id: binding.repo.integration_id.clone(),
        full_name: binding.repo.full_name.clone(),
        url: binding.repo.url.clone(),
        is_workspace,
        is_default: binding.is_default,
        base_branch: binding.base_branch.clone(),
        path_scope: binding.path_scope.clone(),
        effective_path_scope: effective,
    }
}

impl Engine {
    pub async fn check_project_config(
        &self,
        req: &CheckConfig,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<ConfigCheckView> {
        let result = self.check_config(req).await;
        if let Ok(view) = &result {
            let outcome = if view.ok { "ok" } else { "problems" };
            self.counters.config_check(outcome);
            tracing::info!(
                workspace = %req.workspace_url,
                project_path = %req.project_path,
                ok = view.ok,
                problems = view.problems.len(),
                "workspace.config.checked"
            );
            if let Some(action) = action {
                let reply = (action.respond)(Produced::ConfigCheck(view));
                let project = view.project.as_ref().map(|p| p.id);
                let mut batch = Batch::new(self.now());
                batch.action = Some(action.record(self, cause, project, None, reply));
                self.commit(&batch)?;
            }
        }
        result
    }

    async fn check_config(&self, req: &CheckConfig) -> Result<ConfigCheckView> {
        paths::check_project_path(&req.project_path)
            .map_err(|_| invalid("project_path", "invalid"))?;
        let (host, owner, name) = parse_repo_url(&req.workspace_url)
            .ok_or_else(|| invalid("workspace_url", "invalid"))?;
        if let Some(b) = &req.base_branch
            && !branch_ok(b)
        {
            return Err(invalid("base_branch", "invalid"));
        }
        let bytes = req.content.as_bytes();
        if bytes.is_empty() {
            return Err(invalid("content", "required"));
        }
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(invalid("content", "too_large"));
        }
        let content_sha256 = project_config::sha256(bytes);
        let mut issues: Vec<Issue> = Vec::new();
        let mut access = Vec::new();
        let config = project_config::parse(bytes)
            .map_err(|found| issues.extend(found))
            .ok();

        let workspace = match self.lookup_workspace(&host, &owner, &name, req).await? {
            WorkspaceLookup::Found(w) => Some(w),
            WorkspaceLookup::Problem(rejection) => {
                access.push(ConfigProblem::from_rejection(&rejection));
                None
            }
        };
        if workspace.as_ref().is_some_and(|w| w.branch_head.is_none()) {
            issues.push(Issue {
                code: "base_branch_not_found",
                field: Some("base_branch".into()),
                line: None,
            });
        }
        let remote = match &workspace {
            Some(w) => {
                self.remote_config(w, &req.project_path, &content_sha256)
                    .await?
            }
            None => None,
        };
        let ws_repo = workspace.as_ref().map(Workspace::as_repo);
        let mut bindings = Vec::new();
        if let Some(config) = &config {
            let mut resolved = Vec::new();
            for entry in &config.repos {
                match self.resolve_binding(entry, ws_repo.as_ref()).await? {
                    Ok(binding) => resolved.push((entry, binding)),
                    Err(rejection) => access.push(ConfigProblem::from_rejection(&rejection)),
                }
            }
            let pairs: Vec<_> = resolved.iter().map(|(e, b)| (*e, b)).collect();
            issues.extend(
                self.missing_base_branches(&pairs, ws_repo.as_ref(), BranchLookup::Provider)
                    .await?,
            );
            bindings = resolved.into_iter().map(|(_, b)| b).collect();
        }
        let mut repos: Vec<CheckedRepo> = bindings
            .iter()
            .map(|b| checked_repo(b, workspace.as_ref(), &req.project_path))
            .collect();
        repos.sort_by(|a, b| {
            b.is_default
                .cmp(&a.is_default)
                .then_with(|| a.name.cmp(&b.name))
        });

        let mut problems = Vec::new();
        if !issues.is_empty() {
            problems.push(ConfigProblem::from_rejection(&config_invalid(&issues)));
        }
        problems.extend(access);
        let project = match &workspace {
            Some(w) if w.known => self.registered_at(w.repo.repo_id, &req.project_path)?,
            _ => None,
        };
        Ok(ConfigCheckView {
            ok: problems.is_empty(),
            content_sha256,
            workspace: workspace.as_ref().map(|w| CheckedWorkspace {
                integration_id: w.repo.integration_id.clone(),
                full_name: w.repo.full_name.clone(),
                default_branch: w.default_branch.clone(),
                base_branch: w.base_branch.clone(),
            }),
            repos,
            problems,
            remote,
            project,
        })
    }

    async fn lookup_workspace(
        &self,
        host: &str,
        owner: &str,
        name: &str,
        req: &CheckConfig,
    ) -> Result<WorkspaceLookup> {
        let Some(integration) = self.integration_for(host) else {
            return Ok(WorkspaceLookup::Problem(Rejection::ScmIntegrationUnknown {
                host: host.to_string(),
                repo: None,
            }));
        };
        let info = match self
            .ports
            .scm
            .repository(&integration.id, &format!("{owner}/{name}"))
            .await
        {
            Ok(info) => info,
            Err(e) if e.code == "scm_repo_inaccessible" => {
                return Ok(WorkspaceLookup::Problem(Rejection::ScmRepoInaccessible {
                    repo: None,
                }));
            }
            Err(e) => return Err(EngineError::Port(e)),
        };
        let base_branch = req
            .base_branch
            .clone()
            .unwrap_or_else(|| info.default_branch.clone());
        let branch_head = self
            .ports
            .scm
            .branch_head(&integration.id, &info.full_name, &base_branch)
            .await?;
        let existing = self
            .store
            .read(|c| read::repo_by_name(c, &integration.id, &info.full_name))?;
        let known = existing.is_some();
        let repo_id = existing.map_or_else(|| RepoId(self.new_id()), |r| r.id);
        Ok(WorkspaceLookup::Found(Workspace {
            repo: RepoRef {
                repo_id,
                integration_id: integration.id,
                full_name: info.full_name,
                url: info.url,
            },
            host: host.to_string(),
            provider: integration.provider,
            default_branch: info.default_branch,
            base_branch,
            branch_head,
            known,
        }))
    }

    async fn remote_config(
        &self,
        workspace: &Workspace,
        project_path: &str,
        content_sha256: &str,
    ) -> Result<Option<RemoteConfig>> {
        let Some(commit) = workspace.branch_head.clone() else {
            return Ok(None);
        };
        let path = paths::project_config_path(project_path);
        let file = self
            .ports
            .scm
            .file_at(
                &workspace.repo.integration_id,
                &workspace.repo.full_name,
                &commit,
                &path,
                MAX_CONFIG_BYTES,
            )
            .await;
        let (present, config_sha256) = match file {
            Ok(Some(bytes)) => (true, Some(project_config::sha256(&bytes))),
            Ok(None) => (false, None),
            Err(PortError { code, .. }) if code == "file_too_large" => (true, None),
            Err(e) => return Err(EngineError::Port(e)),
        };
        Ok(Some(RemoteConfig {
            commit,
            present,
            matches: config_sha256.as_deref() == Some(content_sha256),
            config_sha256,
        }))
    }

    fn registered_at(&self, repo: RepoId, project_path: &str) -> Result<Option<RegisteredProject>> {
        let Some(id) = self
            .store
            .read(|c| list::project_at(c, repo, project_path))?
        else {
            return Ok(None);
        };
        let project = self
            .store
            .read(|c| read::load_project(c, id))?
            .ok_or_else(|| EngineError::Internal("listed project without a row".into()))?;
        Ok(Some(RegisteredProject {
            id,
            name: project.name,
            config_commit: project.config_commit,
            config_sha256: project.config_sha256,
        }))
    }
}
