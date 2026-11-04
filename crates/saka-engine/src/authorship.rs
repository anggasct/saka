use saka_core::authorship::{
    self, GitIdentity, ProjectAuthorship, ProjectUserToken, UserLink, link_problem, mode_of,
};
use saka_core::ids::ProjectId;
use saka_core::model::{Project, Repo};
use saka_core::rejection::Rejection;
use saka_core::secret::{SecretScope, SecretText};
use saka_core::time::Timestamp;
use saka_core::vocab::{AuthorSource, AuthorshipMode, TokenInvalidCause, UserLinkCause};
use saka_ports::{IdentityLookup, PortError, RepoAccess, Sealed};
use saka_store::authorship::{self as rows, ProjectAuthorshipRow};
use saka_store::user_link::{SealedColumns, user_link};
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
pub struct UserTokenView {
    pub token: ProjectUserToken,
    pub problem: Option<TokenInvalidCause>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorshipView {
    pub project_id: ProjectId,
    pub mode: AuthorshipMode,
    pub mode_updated_at: Option<Timestamp>,
    pub author: Option<AuthorPreview>,
    pub integrations: Vec<IntegrationLink>,
    pub user_token: Option<UserTokenView>,
}

fn token_scope(project_id: ProjectId) -> SecretScope<'static> {
    SecretScope::ProjectUserToken { project_id }
}

pub(crate) struct OpenedToken {
    pub value: SecretText,
    pub token: ProjectUserToken,
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
        let now = self.now();
        let default_integration = bound
            .iter()
            .find(|b| b.is_default)
            .map(|b| b.integration_id.clone());
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
            AuthorshipMode::UserPat => default_integration
                .and_then(|i| authorship::user_token_author(current, &i, now).ok())
                .map(|r| AuthorPreview {
                    source: r.author.source,
                    identity: r.identity,
                    login: r.author.login,
                }),
        };
        let user_token = authorship::token_of(current).map(|t| UserTokenView {
            problem: t.problem(now),
            token: t.clone(),
        });
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
            user_token,
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
            AuthorshipMode::UserPat => {
                let current = self
                    .store
                    .read(|c| rows::project_authorship(c, project.id))?;
                Ok(authorship::check_user_pat_mode(
                    current.as_ref(),
                    self.now(),
                )?)
            }
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
        let row = self.store.read(|c| rows::project_authorship_row(c, id))?;
        let current = row.as_ref().map(|r| r.authorship.clone());
        let mut batch = Batch::new(now);
        let changed = authorship::set_mode(id, current.as_ref(), mode, now);
        let after = match &changed {
            Some((event, after)) => {
                self.push_event(&mut batch, event, Some(id), cause, now);
                batch.project_authorships.push(ProjectAuthorshipRow {
                    authorship: after.clone(),
                    token: row.and_then(|r| r.token),
                });
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

    pub fn user_token_fingerprint(&self, project: ProjectId, value: &SecretText) -> String {
        self.ports.secrets.fingerprint(&token_scope(project), value)
    }

    fn token_integration(&self, project: &Project) -> Result<String> {
        let mut ids: Vec<String> = self
            .bound_integrations(project)?
            .into_iter()
            .map(|b| b.integration_id)
            .collect();
        ids.sort();
        match ids.as_slice() {
            [only] => Ok(only.clone()),
            _ => Err(Rejection::UserTokenMultiIntegration { integrations: ids }.into()),
        }
    }

    async fn inaccessible_repos(
        &self,
        project: &Project,
        integration: &str,
        value: &SecretText,
    ) -> Result<Vec<serde_json::Value>> {
        let ids: Vec<_> = project.repos.iter().map(|r| r.repo_id).collect();
        let repos: Vec<Repo> = self.store.read(|c| list::repos(c, &ids))?;
        let mut refused = Vec::new();
        for binding in &project.repos {
            let Some(repo) = repos.iter().find(|r| r.id == binding.repo_id) else {
                continue;
            };
            let access = self
                .ports
                .scm
                .user_repository_access(integration, &repo.full_name, value)
                .await?;
            if let RepoAccess::Inaccessible { status } = access {
                refused.push(json!({
                    "name": binding.name,
                    "full_name": repo.full_name,
                    "status": status,
                }));
            }
        }
        Ok(refused)
    }

    // The provider is asked before anything is written; the value is sealed in the same
    // transaction as the event that records the token without it.
    pub async fn set_user_token(
        &self,
        id: ProjectId,
        value: &SecretText,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<AuthorshipView> {
        let project = self.project_of(id)?;
        authorship::check_user_token_value(value.expose())?;
        let integration = self.token_integration(&project)?;
        let supported = self
            .ports
            .scm
            .capabilities(&integration)
            .is_some_and(|c| c.user_authorship);
        if !supported {
            return Err(Rejection::UserAuthorshipUnsupported {
                integration_id: integration,
            }
            .into());
        }
        let (identity, expires_at) = match self.ports.scm.user_identity(&integration, value).await?
        {
            IdentityLookup::Found {
                identity,
                token_expires_at,
            } if identity.is_valid() => (identity, token_expires_at),
            IdentityLookup::Found { .. } => {
                return Err(PortError::retryable(
                    "scm_unavailable",
                    json!({ "detail": "user_incomplete" }),
                )
                .into());
            }
            IdentityLookup::Rejected { status } => {
                return Err(Rejection::UserTokenRejected { status }.into());
            }
        };
        if expires_at.is_some_and(|at| at <= self.now()) {
            return Err(Rejection::UserTokenRejected { status: 401 }.into());
        }
        let refused = self
            .inaccessible_repos(&project, &integration, value)
            .await?;
        if !refused.is_empty() {
            return Err(Rejection::UserTokenRepoInaccessible {
                repos: json!(refused),
            }
            .into());
        }
        let sealed = self.ports.secrets.seal(&token_scope(id), value)?;
        let _guard = self.locked()?;
        let now = self.now();
        let current = self.store.read(|c| rows::project_authorship(c, id))?;
        let token = ProjectUserToken {
            integration_id: integration,
            identity,
            expires_at,
            set_at: now,
            invalid_cause: None,
        };
        let (event, after) = authorship::store_token(id, current.as_ref(), token, now);
        let mut batch = Batch::new(now);
        self.push_event(&mut batch, &event, Some(id), cause, now);
        batch.project_authorships.push(ProjectAuthorshipRow {
            authorship: after.clone(),
            token: Some(SealedColumns {
                nonce: sealed.nonce,
                ciphertext: sealed.ciphertext,
            }),
        });
        let view = self.authorship_view(&project, Some(&after))?;
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Authorship(&view));
            batch.action = Some(action.record(self, cause, Some(id), Some(id.0), reply));
        }
        self.commit(&batch)?;
        tracing::info!(
            project_id = %id.0,
            login = %after.token.as_ref().map_or("", |t| t.identity.login.as_str()),
            "workspace.project.user_token_set"
        );
        Ok(view)
    }

    pub fn remove_user_token(
        &self,
        id: ProjectId,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<()> {
        self.project_of(id)?;
        let _guard = self.locked()?;
        let now = self.now();
        let current = self.store.read(|c| rows::project_authorship(c, id))?;
        if mode_of(current.as_ref()) == AuthorshipMode::UserPat {
            return Err(Rejection::UserTokenInUse.into());
        }
        let mut batch = Batch::new(now);
        let removed = authorship::remove_token(current.as_ref(), now);
        if let Some((event, after)) = &removed {
            self.push_event(&mut batch, event, Some(id), cause, now);
            batch.project_authorships.push(ProjectAuthorshipRow {
                authorship: after.clone(),
                token: None,
            });
        }
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Nothing);
            batch.action = Some(action.record(self, cause, Some(id), Some(id.0), reply));
        }
        self.commit(&batch)?;
        if removed.is_some() {
            tracing::info!(project_id = %id.0, "workspace.project.user_token_removed");
        }
        Ok(())
    }

    pub(crate) fn project_authorship_now(
        &self,
        id: ProjectId,
    ) -> Result<Option<ProjectAuthorship>> {
        Ok(self.store.read(|c| rows::project_authorship(c, id))?)
    }

    // A token that cannot be opened (another key) is as good as refused: it is marked so the
    // operator stores it again.
    pub(crate) fn open_user_token(&self, id: ProjectId) -> Result<Option<OpenedToken>> {
        let Some(row) = self.store.read(|c| rows::project_authorship_row(c, id))? else {
            return Ok(None);
        };
        let (Some(token), Some(columns)) = (row.authorship.token.clone(), row.token) else {
            return Ok(None);
        };
        let sealed = Sealed {
            nonce: columns.nonce,
            ciphertext: columns.ciphertext,
        };
        match self.ports.secrets.open(&token_scope(id), &sealed) {
            Ok(value) => Ok(Some(OpenedToken { value, token })),
            Err(e) if !e.retryable => {
                self.invalidate_user_token(id, token.set_at, TokenInvalidCause::Rejected)?;
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) fn invalidate_user_token(
        &self,
        id: ProjectId,
        used_set_at: Timestamp,
        cause_code: TokenInvalidCause,
    ) -> Result<()> {
        let cause = self.cause("system:user_token:use");
        let _guard = self.locked()?;
        let now = self.now();
        let Some(row) = self.store.read(|c| rows::project_authorship_row(c, id))? else {
            return Ok(());
        };
        let Some((event, after)) =
            authorship::invalidate_token(Some(&row.authorship), used_set_at, cause_code, now)
        else {
            return Ok(());
        };
        let mut batch = Batch::new(now);
        self.push_event(&mut batch, &event, Some(id), &cause, now);
        batch.project_authorships.push(ProjectAuthorshipRow {
            authorship: after,
            token: row.token,
        });
        self.commit(&batch)?;
        tracing::warn!(
            project_id = %id.0,
            cause = cause_code.as_str(),
            "workspace.project.user_token_invalidated"
        );
        Ok(())
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
