//! Registration from the control API: a project from the `saka.toml` in its folder, and a
//! spec from its file on the base branch. Reads go through the ports first; the write is
//! one decision under the engine lock.

use saka_core::ids::{ProjectId, RepoId};
use saka_core::paths::{self, PathIssue};
use saka_core::rejection::Rejection;
use saka_ports::RepoRef;
use saka_store::{list, read};
use serde_json::Value;

use crate::action::Action;
use crate::commands::NewProject;
use crate::project_config;
use crate::views::{ProjectView, SpecView};
use crate::{Cause, Engine, EngineError, Result};

/// An SCM integration from the operator config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Integration {
    pub id: String,
    pub provider: String,
    /// `https://github.com`.
    pub web_url: String,
}

/// `POST /projects`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddProject {
    pub workspace_url: String,
    pub project_path: String,
    pub base_branch: Option<String>,
}

/// `https://<host>/<owner>/<name>` (an optional `.git` or `/` at the end):
/// `(host, owner, name)`.
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

/// A branch name Saka passes to git.
fn branch_ok(branch: &str) -> bool {
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

fn invalid(field: &'static str, code: &str) -> EngineError {
    EngineError::Invalid {
        field,
        code: code.to_string(),
    }
}

fn path_code(issue: PathIssue) -> &'static str {
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

impl Engine {
    /// Registers a project from `saka.toml` at the tip of its base branch.
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
        let integration = self
            .cfg
            .integrations
            .iter()
            .find(|i| web_host(&i.web_url).as_deref() == Some(host.as_str()))
            .cloned()
            .ok_or_else(|| Rejection::ScmIntegrationUnknown { host: host.clone() })?;
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
        let config_path = paths::project_config_path(&req.project_path);
        let file = self
            .ports
            .docs
            .read_at_tip(&repo, &base_branch, &config_path)
            .await
            .map_err(base_branch_error)?
            .ok_or_else(|| Rejection::ProjectConfigInvalid {
                issues: Value::Array(vec![
                    project_config::Issue {
                        code: "file_missing",
                        field: None,
                        line: None,
                    }
                    .to_json(),
                ]),
            })?;
        let config = project_config::parse(&file.bytes).map_err(|issues| {
            Rejection::ProjectConfigInvalid {
                issues: issues.iter().map(project_config::Issue::to_json).collect(),
            }
        })?;
        if config.repos.len() > 1 {
            return Err(Rejection::MultiRepoUnsupported {
                repos: config.repos.len(),
            }
            .into());
        }
        let entry = config.repos.first().cloned().ok_or_else(|| {
            EngineError::Internal("project config without a repo passed parsing".into())
        })?;
        // Code in a repo other than the workspace comes with multi-repo projects.
        let same_repo = parse_repo_url(&entry.url).is_some_and(|(h, o, n)| {
            h == host && format!("{o}/{n}").eq_ignore_ascii_case(&info.full_name)
        });
        if !same_repo {
            return Err(Rejection::ProjectConfigInvalid {
                issues: Value::Array(vec![
                    project_config::Issue {
                        code: "field_invalid",
                        field: Some("repo[0].url".into()),
                        line: None,
                    }
                    .to_json(),
                ]),
            }
            .into());
        }
        let new = NewProject {
            name: config.name,
            integration_id: integration.id.clone(),
            provider: integration.provider.clone(),
            full_name: info.full_name.clone(),
            url: info.url.clone(),
            default_branch: info.default_branch.clone(),
            project_path: req.project_path.clone(),
            base_branch,
            config_commit: file.commit,
            repo_name: entry.name,
        };
        self.register_project_with(&new, Some(repo_id), cause, action)
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

    /// Reads a spec file at the tip of the project's base branch and records it.
    pub async fn add_spec(
        &self,
        project_id: ProjectId,
        path: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<(SpecView, bool)> {
        let project = self
            .store
            .read(|c| read::load_project(c, project_id))?
            .ok_or(Rejection::NotFound)?;
        paths::check_spec_path(&project.project_path, path)
            .map_err(|issue| invalid("path", path_code(issue)))?;
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
        self.register_spec_with(project_id, path, &file.commit, &file.bytes, cause, action)
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
