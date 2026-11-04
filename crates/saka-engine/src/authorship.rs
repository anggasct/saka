use saka_core::authorship::{
    self, GitIdentity, ProjectAuthorship, UserLink, link_problem, mode_of,
};
use saka_core::ids::ProjectId;
use saka_core::model::Project;
use saka_core::rejection::Rejection;
use saka_core::time::Timestamp;
use saka_core::vocab::{AuthorSource, AuthorshipMode, UserLinkCause};
use saka_store::authorship as rows;
use saka_store::user_link::user_link;
use saka_store::{Batch, list, read};
use serde_json::json;

use crate::action::{Action, Produced};
use crate::{Cause, Engine, EngineError, OPERATOR, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorPreview {
    pub source: AuthorSource,
    pub identity: GitIdentity,
    pub login: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationLink {
    pub integration_id: String,
    pub problem: Option<UserLinkCause>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorshipView {
    pub project_id: ProjectId,
    pub mode: AuthorshipMode,
    pub mode_updated_at: Option<Timestamp>,
    pub author: Option<AuthorPreview>,
    pub integrations: Vec<IntegrationLink>,
}

struct Bound {
    integration_id: String,
    link: Option<UserLink>,
    is_default: bool,
}

impl Engine {
    pub fn authorship_mode(&self, project: ProjectId) -> Result<AuthorshipMode> {
        let current = self.store.read(|c| rows::project_authorship(c, project))?;
        Ok(mode_of(current.as_ref()))
    }

    fn bound_integrations(&self, project: &Project) -> Result<Vec<Bound>> {
        let ids: Vec<_> = project.repos.iter().map(|r| r.repo_id).collect();
        let repos = self.store.read(|c| list::repos(c, &ids))?;
        let mut out: Vec<Bound> = Vec::new();
        for binding in &project.repos {
            let Some(repo) = repos.iter().find(|r| r.id == binding.repo_id) else {
                continue;
            };
            if let Some(known) = out
                .iter_mut()
                .find(|b| b.integration_id == repo.integration_id)
            {
                known.is_default |= binding.is_default;
                continue;
            }
            let link = self
                .store
                .read(|c| user_link(c, OPERATOR, &repo.integration_id))?
                .map(|r| r.link);
            out.push(Bound {
                integration_id: repo.integration_id.clone(),
                link,
                is_default: binding.is_default,
            });
        }
        Ok(out)
    }

    fn authorship_view(
        &self,
        project: &Project,
        current: Option<&ProjectAuthorship>,
    ) -> Result<AuthorshipView> {
        let bound = self.bound_integrations(project)?;
        let mode = mode_of(current);
        let author = match mode {
            AuthorshipMode::Bot => Some(AuthorPreview {
                source: AuthorSource::Saka,
                identity: self.cfg.git_identity.clone(),
                login: None,
            }),
            AuthorshipMode::User => bound
                .iter()
                .find(|b| b.is_default)
                .and_then(|b| b.link.as_ref())
                .filter(|l| l.is_active())
                .map(|l| AuthorPreview {
                    source: AuthorSource::UserLink,
                    identity: l.git_identity(),
                    login: Some(l.login.clone()),
                }),
            AuthorshipMode::UserPat => None,
        };
        Ok(AuthorshipView {
            project_id: project.id,
            mode,
            mode_updated_at: current.map(|a| a.mode_updated_at),
            author,
            integrations: bound
                .into_iter()
                .map(|b| IntegrationLink {
                    problem: link_problem(b.link.as_ref()),
                    integration_id: b.integration_id,
                })
                .collect(),
        })
    }

    fn project_of(&self, id: ProjectId) -> Result<Project> {
        self.store
            .read(|c| read::load_project(c, id))?
            .ok_or(EngineError::Rejected(Rejection::NotFound))
    }

    pub fn project_authorship(&self, id: ProjectId) -> Result<AuthorshipView> {
        let project = self.project_of(id)?;
        let current = self.store.read(|c| rows::project_authorship(c, id))?;
        self.authorship_view(&project, current.as_ref())
    }

    fn check_mode(&self, project: &Project, mode: AuthorshipMode) -> Result<()> {
        match mode {
            AuthorshipMode::Bot => Ok(()),
            AuthorshipMode::User => {
                let bound = self.bound_integrations(project)?;
                for b in &bound {
                    let supported = self
                        .ports
                        .scm
                        .capabilities(&b.integration_id)
                        .is_some_and(|c| c.user_authorship);
                    if !supported {
                        return Err(Rejection::UserAuthorshipUnsupported {
                            integration_id: b.integration_id.clone(),
                        }
                        .into());
                    }
                }
                let unusable: Vec<_> = bound
                    .iter()
                    .filter_map(|b| {
                        let state = match link_problem(b.link.as_ref())? {
                            UserLinkCause::Missing => "missing",
                            _ => "needs_relink",
                        };
                        Some(json!({ "integration_id": b.integration_id, "state": state }))
                    })
                    .collect();
                if unusable.is_empty() {
                    Ok(())
                } else {
                    Err(Rejection::UserLinkMissing {
                        integrations: json!(unusable),
                    }
                    .into())
                }
            }
            AuthorshipMode::UserPat => Err(Rejection::UserTokenMissing.into()),
        }
    }

    pub fn set_authorship(
        &self,
        id: ProjectId,
        mode: AuthorshipMode,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<AuthorshipView> {
        let project = self.project_of(id)?;
        let _guard = self.locked()?;
        self.check_mode(&project, mode)?;
        let now = self.now();
        let current = self.store.read(|c| rows::project_authorship(c, id))?;
        let mut batch = Batch::new(now);
        let changed = authorship::set_mode(id, current.as_ref(), mode, now);
        let after = match &changed {
            Some((event, after)) => {
                self.push_event(&mut batch, event, Some(id), cause, now);
                batch.project_authorships.push(after.clone());
                Some(after.clone())
            }
            None => current,
        };
        let view = self.authorship_view(&project, after.as_ref())?;
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Authorship(&view));
            batch.action = Some(action.record(self, cause, Some(id), Some(id.0), reply));
        }
        self.commit(&batch)?;
        if changed.is_some() {
            tracing::info!(project_id = %id.0, mode = mode.as_str(), "workspace.project.authorship_set");
        }
        Ok(view)
    }

    pub(crate) fn user_mode_projects(
        &self,
        integration_id: &str,
    ) -> Result<Vec<rows::ProjectName>> {
        Ok(self
            .store
            .read(|c| rows::user_mode_projects_on(c, integration_id))?)
    }
}
