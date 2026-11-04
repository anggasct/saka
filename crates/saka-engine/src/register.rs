use saka_core::ids::{ProjectId, RepoId};
use saka_core::model::Project;
use saka_core::paths::{self, PathIssue};
use saka_core::rejection::Rejection;
use saka_ports::{Blob, RepoRef};
use saka_store::{list, read};

use crate::action::Action;
use crate::commands::{NewBinding, NewProject, NewRepo, SpecFile};
use crate::project_config;
use crate::views::{ProjectView, SpecView};
use crate::{Cause, Engine, EngineError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Integration {
    pub id: String,
    pub provider: String,
    pub web_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddProject {
    pub workspace_url: String,
    pub project_path: String,
    pub base_branch: Option<String>,
}

pub fn parse_repo_url(url: &str) -> Option<(String, String, String)> {
    let rest = url.strip_prefix("https://")?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    let (host, owner, name) = (parts.next()?, parts.next()?, parts.next()?);
    let plain = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'));
    (parts.next().is_none() && host_ok && plain(owner) && plain(name)).then(|| {
        (
            host.to_ascii_lowercase(),
            owner.to_string(),
            name.to_string(),
        )
    })
}

fn web_host(web_url: &str) -> Option<String> {
    let rest = web_url.strip_prefix("https://")?;
    Some(rest.trim_end_matches('/').to_ascii_lowercase())
}

pub(crate) fn branch_ok(branch: &str) -> bool {
    !branch.is_empty()
        && branch.len() <= 255
        && !branch.starts_with('-')
        && !branch.contains("..")
        && branch
            .split('/')
            .all(|s| !s.is_empty() && !s.ends_with(".lock"))
        && branch
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
}

pub(crate) fn invalid(field: &'static str, code: &str) -> EngineError {
    EngineError::Invalid {
        field,
        code: code.to_string(),
    }
}

pub(crate) fn path_code(issue: PathIssue) -> &'static str {
    match issue {
        PathIssue::Invalid => "invalid",
        PathIssue::OutsideProject => "outside_project",
        PathIssue::NotMarkdown => "not_markdown",
    }
}

fn base_branch_error(e: saka_ports::PortError) -> EngineError {
    if e.code == "base_branch_missing" {
        invalid("base_branch", "not_found")
    } else {
        EngineError::Port(e)
    }
}

pub(crate) fn config_invalid(issues: &[project_config::Issue]) -> Rejection {
    Rejection::ProjectConfigInvalid {
        issues: project_config::issues_json(issues),
    }
}

pub(crate) const MAX_CONFIG_BYTES: u64 = 65_536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigContent {
    Missing,
    TooLarge,
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigFile {
    pub commit: String,
    pub content: ConfigContent,
}

impl ConfigFile {
    pub(crate) fn bytes(self) -> std::result::Result<Vec<u8>, Rejection> {
        match self.content {
            ConfigContent::Bytes(b) => Ok(b),
            ConfigContent::Missing => Err(config_invalid(&[project_config::Issue::file_missing()])),
            ConfigContent::TooLarge => Err(config_invalid(&[project_config::Issue {
                code: "toml_invalid",
                field: None,
                line: None,
            }])),
        }
    }
}

pub(crate) struct WorkspaceRepo<'a> {
    pub repo: &'a RepoRef,
    pub host: &'a str,
    pub default_branch: &'a str,
    pub provider: &'a str,
    pub base_branch: &'a str,
}

impl Engine {
    pub(crate) fn integration_for(&self, host: &str) -> Option<Integration> {
        self.cfg
            .integrations
            .iter()
            .find(|i| web_host(&i.web_url).as_deref() == Some(host))
            .cloned()
    }

    pub(crate) async fn read_config(
        &self,
        workspace: &RepoRef,
        base_branch: &str,
        project_path: &str,
    ) -> Result<ConfigFile> {
        let commit = self
            .ports
            .docs
            .branch_tip(workspace, base_branch)
            .await?
            .ok_or_else(|| {
                saka_ports::PortError::permanent(
                    "base_branch_missing",
                    serde_json::json!({ "base_branch": base_branch }),
                )
            })?;
        let path = paths::project_config_path(project_path);
        let blob = self
            .ports
            .docs
            .read_at(workspace, &commit, &path, MAX_CONFIG_BYTES)
            .await?;
        let content = match blob {
            Blob::Missing => ConfigContent::Missing,
            Blob::Bytes(b) => ConfigContent::Bytes(b),
            Blob::TooLarge { .. } => ConfigContent::TooLarge,
        };
        Ok(ConfigFile { commit, content })
    }

    pub(crate) async fn resolve_bindings(
        &self,
        config: &project_config::ProjectConfig,
        workspace: &WorkspaceRepo<'_>,
    ) -> Result<Vec<NewBinding>> {
        let mut out = Vec::with_capacity(config.repos.len());
        for entry in &config.repos {
            let integration = self.integration_for(&entry.host).ok_or_else(|| {
                Rejection::ScmIntegrationUnknown {
                    host: entry.host.clone(),
                    repo: Some(entry.name.clone()),
                }
            })?;
            let is_workspace = integration.id == workspace.repo.integration_id
                && entry.same_repo(workspace.host, &workspace.repo.full_name);
            let repo = if is_workspace {
                NewRepo {
                    repo_id: Some(workspace.repo.repo_id),
                    integration_id: integration.id.clone(),
                    provider: workspace.provider.to_string(),
                    full_name: workspace.repo.full_name.clone(),
                    url: workspace.repo.url.clone(),
                    default_branch: workspace.default_branch.to_string(),
                }
            } else {
                let info = self
                    .ports
                    .scm
                    .repository(&integration.id, &entry.full_name)
                    .await
                    .map_err(|e| {
                        if e.code == "scm_repo_inaccessible" {
                            EngineError::Rejected(Rejection::ScmRepoInaccessible {
                                repo: Some(entry.name.clone()),
                            })
                        } else {
                            EngineError::Port(e)
                        }
                    })?;
                let known = self
                    .store
                    .read(|c| read::repo_by_name(c, &integration.id, &info.full_name))?;
                NewRepo {
                    repo_id: Some(known.map_or_else(|| RepoId(self.new_id()), |r| r.id)),
                    integration_id: integration.id.clone(),
                    provider: integration.provider.clone(),
                    full_name: info.full_name,
                    url: info.url,
                    default_branch: info.default_branch,
                }
            };
            let base_branch = match (&entry.base_branch, is_workspace) {
                (Some(b), _) => b.clone(),
                (None, true) => workspace.base_branch.to_string(),
                (None, false) => repo.default_branch.clone(),
            };
            out.push(NewBinding {
                repo,
                name: entry.name.clone(),
                is_default: entry.is_default,
                base_branch,
                path_scope: entry.path_scope.clone(),
            });
        }
        let mut missing = Vec::new();
        for (entry, binding) in config.repos.iter().zip(&out) {
            let known_tip = binding.repo.repo_id == Some(workspace.repo.repo_id)
                && binding.base_branch == workspace.base_branch;
            if known_tip {
                continue;
            }
            let repo = binding.repo.repo_ref();
            if self
                .ports
                .docs
                .branch_tip(&repo, &binding.base_branch)
                .await?
                .is_none()
            {
                missing.push(project_config::Issue {
                    code: "base_branch_not_found",
                    field: Some(format!("repo[{}].base_branch", entry.index)),
                    line: entry.base_branch_line.or(Some(entry.line)),
                });
            }
        }
        if !missing.is_empty() {
            return Err(config_invalid(&missing).into());
        }
        Ok(out)
    }

    pub async fn add_project(
        &self,
        req: &AddProject,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<ProjectView> {
        paths::check_project_path(&req.project_path)
            .map_err(|_| invalid("project_path", "invalid"))?;
        let (host, owner, name) = parse_repo_url(&req.workspace_url)
            .ok_or_else(|| invalid("workspace_url", "invalid"))?;
        let integration =
            self.integration_for(&host)
                .ok_or_else(|| Rejection::ScmIntegrationUnknown {
                    host: host.clone(),
                    repo: None,
                })?;
        if let Some(b) = &req.base_branch
            && !branch_ok(b)
        {
            return Err(invalid("base_branch", "invalid"));
        }
        let info = self
            .ports
            .scm
            .repository(&integration.id, &format!("{owner}/{name}"))
            .await?;
        let base_branch = req
            .base_branch
            .clone()
            .unwrap_or_else(|| info.default_branch.clone());
        let existing = self
            .store
            .read(|c| read::repo_by_name(c, &integration.id, &info.full_name))?;
        let repo_id = existing
            .as_ref()
            .map_or_else(|| RepoId(self.new_id()), |r| r.id);
        self.refuse_existing_project(repo_id, &req.project_path)?;
        let repo = RepoRef {
            repo_id,
            integration_id: integration.id.clone(),
            full_name: info.full_name.clone(),
            url: info.url.clone(),
        };
        let file = self
            .read_config(&repo, &base_branch, &req.project_path)
            .await
            .map_err(|e| match e {
                EngineError::Port(p) => base_branch_error(p),
                other => other,
            })?;
        let commit = file.commit.clone();
        let bytes = file.bytes()?;
        let config = project_config::parse(&bytes).map_err(|issues| config_invalid(&issues))?;
        let workspace = WorkspaceRepo {
            repo: &repo,
            host: &host,
            default_branch: &info.default_branch,
            provider: &integration.provider,
            base_branch: &base_branch,
        };
        let bindings = self.resolve_bindings(&config, &workspace).await?;
        let new = NewProject {
            name: config.name,
            workspace: NewRepo {
                repo_id: Some(repo_id),
                integration_id: integration.id.clone(),
                provider: integration.provider.clone(),
                full_name: info.full_name.clone(),
                url: info.url.clone(),
                default_branch: info.default_branch.clone(),
            },
            project_path: req.project_path.clone(),
            base_branch,
            config_commit: commit,
            config_sha256: project_config::sha256(&bytes),
            bindings,
        };
        self.register_project_with(&new, cause, action)
    }

    pub(crate) fn refuse_existing_project(&self, repo: RepoId, project_path: &str) -> Result<()> {
        if let Some(id) = self
            .store
            .read(|c| list::project_at(c, repo, project_path))?
        {
            return Err(Rejection::ProjectExists {
                project_id: id.0.to_string(),
            }
            .into());
        }
        Ok(())
    }

    pub(crate) fn workspace_of(&self, project_id: ProjectId) -> Result<(Project, RepoRef)> {
        let project = self
            .store
            .read(|c| read::load_project(c, project_id))?
            .ok_or(Rejection::NotFound)?;
        let repo = self
            .store
            .read(|c| read::load_repo(c, project.workspace_repo_id))?
            .ok_or_else(|| EngineError::Internal("project without its workspace repo".into()))?;
        let repo_ref = RepoRef {
            repo_id: repo.id,
            integration_id: repo.integration_id.clone(),
            full_name: repo.full_name.clone(),
            url: repo.url.clone(),
        };
        Ok((project, repo_ref))
    }

    pub async fn add_spec(
        &self,
        project_id: ProjectId,
        path: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<(SpecView, bool)> {
        let (project, repo_ref) = self.workspace_of(project_id)?;
        paths::check_spec_path(&project.project_path, path)
            .map_err(|issue| invalid("path", path_code(issue)))?;
        let file = self
            .ports
            .docs
            .read_at_tip(&repo_ref, &project.base_branch, path)
            .await
            .map_err(|e| {
                if e.code == "base_branch_missing" {
                    EngineError::Rejected(Rejection::SpecFileNotFound {
                        path: path.to_string(),
                        base_branch: project.base_branch.clone(),
                    })
                } else {
                    EngineError::Port(e)
                }
            })?
            .ok_or_else(|| Rejection::SpecFileNotFound {
                path: path.to_string(),
                base_branch: project.base_branch.clone(),
            })?;
        let spec_file = SpecFile {
            path,
            source_commit: &file.commit,
            bytes: &file.bytes,
            origin: saka_core::vocab::SpecOrigin::Manual,
        };
        self.register_at(&project, &repo_ref, spec_file, cause, action)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_urls_are_https_with_owner_and_name() {
        assert_eq!(
            parse_repo_url("https://GitHub.com/acme/app.git"),
            Some(("github.com".into(), "acme".into(), "app".into()))
        );
        assert!(parse_repo_url("https://github.com/acme/app/").is_some());
        for bad in [
            "http://github.com/acme/app",
            "https://github.com/acme",
            "https://github.com/acme/app/tree",
            "https://github.com/../app",
            "https://user@github.com/acme/app",
        ] {
            assert_eq!(parse_repo_url(bad), None, "{bad}");
        }
        assert!(branch_ok("release/1.2"));
        for bad in ["", "-x", "a..b", "a b", "a/", "x.lock"] {
            assert!(!branch_ok(bad), "{bad}");
        }
    }
}
