use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use saka_core::authorship::{self, NewAuthorization, TokenExpiry, UserLink, UserLinkAuthorization};
use saka_core::event::Event;
use saka_core::ids::{UserLinkAuthorizationId, UserLinkId};
use saka_core::rejection::Rejection;
use saka_core::secret::{SecretScope, SecretText};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    AuthorizationFailure, AuthorizationState, RefreshOutcome, RefreshReason, RelinkCause,
    UserLinkCause, UserTokenKind,
};
use saka_ports::{DevicePoll, IdentityLookup, PortError, Sealed, TokenRefresh, UserTokens};
use saka_store::user_link as rows;
use saka_store::{AuthorizationRow, Batch, ClaimedJob, SealedColumns, UserLinkRow};
use serde_json::json;

use crate::action::{Action, Produced};
use crate::register::Integration;
use crate::worker::{Effect, Execution, field, internal};
use crate::{Cause, Engine, EngineError, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct AuthorizationView {
    pub authorization: UserLinkAuthorization,
    pub link: Option<UserLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkCredential {
    Ready(SecretText),
    Relink(RelinkCause),
}

#[derive(Default)]
pub(crate) struct Accounts {
    link_locks: Mutex<HashMap<UserLinkId, Arc<tokio::sync::Mutex<()>>>>,
    last_polls: Mutex<HashMap<UserLinkAuthorizationId, Timestamp>>,
}

impl Accounts {
    fn link_lock(&self, id: UserLinkId) -> Arc<tokio::sync::Mutex<()>> {
        match self.link_locks.lock() {
            Ok(mut locks) => locks.entry(id).or_default().clone(),
            Err(poisoned) => poisoned.into_inner().entry(id).or_default().clone(),
        }
    }

    // GitHub answers slow_down to a client that polls faster than the interval, so a poll that
    // comes too soon after the last one in this process waits for the next bucket instead.
    fn take_poll(&self, id: UserLinkAuthorizationId, interval_ms: i64, now: Timestamp) -> bool {
        let Ok(mut polls) = self.last_polls.lock() else {
            return true;
        };
        if polls
            .get(&id)
            .is_some_and(|last| now.since(*last) < interval_ms)
        {
            return false;
        }
        polls.insert(id, now);
        true
    }

    fn forget_poll(&self, id: UserLinkAuthorizationId) {
        if let Ok(mut polls) = self.last_polls.lock() {
            polls.remove(&id);
        }
    }
}

fn sealed(columns: &SealedColumns) -> Sealed {
    Sealed {
        nonce: columns.nonce.clone(),
        ciphertext: columns.ciphertext.clone(),
    }
}

fn columns(sealed: Sealed) -> SealedColumns {
    SealedColumns {
        nonce: sealed.nonce,
        ciphertext: sealed.ciphertext,
    }
}

fn job_cause(job: &ClaimedJob) -> Cause {
    Cause {
        actor: format!("system:worker:{}", job.kind.as_str()),
        causation_id: Some(job.id),
        correlation_id: job.correlation_id,
    }
}

fn token_scope(link_id: UserLinkId, kind: UserTokenKind) -> SecretScope<'static> {
    SecretScope::UserLinkToken { link_id, kind }
}

impl Engine {
    fn integration(&self, id: &str) -> Result<&Integration> {
        self.cfg
            .integrations
            .iter()
            .find(|i| i.id == id)
            .ok_or(EngineError::Rejected(Rejection::NotFound))
    }

    fn check_device_flow(&self, integration_id: &str) -> Result<()> {
        self.integration(integration_id)?;
        let supported = self
            .ports
            .scm
            .capabilities(integration_id)
            .is_some_and(|c| c.user_authorship);
        if !supported {
            return Err(Rejection::UserAuthorshipUnsupported {
                integration_id: integration_id.to_string(),
            }
            .into());
        }
        if !self.ports.scm.device_flow_configured(integration_id)? {
            return Err(Rejection::DeviceFlowUnconfigured {
                integration_id: integration_id.to_string(),
            }
            .into());
        }
        Ok(())
    }

    fn view_of(&self, authorization: UserLinkAuthorization) -> Result<AuthorizationView> {
        let link = match authorization.link_id {
            Some(id) => self
                .store
                .read(|c| rows::user_link_by_id(c, id))?
                .map(|r| r.link),
            None => None,
        };
        Ok(AuthorizationView {
            authorization,
            link,
        })
    }

    fn reuse(
        &self,
        pending: UserLinkAuthorization,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<AuthorizationView> {
        let view = self.view_of(pending)?;
        if let Some(action) = action {
            let mut batch = Batch::new(self.now());
            let reply = (action.respond)(Produced::UserLinkAuthorization(&view));
            let id = view.authorization.id.0;
            batch.action = Some(action.record(self, cause, None, Some(id), reply));
            self.commit(&batch)?;
        }
        Ok(view)
    }

    pub async fn start_user_link(
        &self,
        integration_id: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<AuthorizationView> {
        self.check_device_flow(integration_id)?;
        let owner = cause.actor.as_str();
        let reusable = |now: Timestamp| -> Result<Option<UserLinkAuthorization>> {
            Ok(self
                .store
                .read(|c| rows::pending_authorization(c, owner, integration_id))?
                .map(|r| r.authorization)
                .filter(|a| a.reusable(now)))
        };
        if let Some(pending) = reusable(self.now())? {
            let _guard = self.locked()?;
            return self.reuse(pending, cause, action);
        }
        let code = match self.ports.scm.start_device_flow(integration_id).await {
            Err(e) if e.code == "device_flow_unconfigured" => {
                return Err(Rejection::DeviceFlowUnconfigured {
                    integration_id: integration_id.to_string(),
                }
                .into());
            }
            other => other?,
        };
        let _guard = self.locked()?;
        let now = self.now();
        let pending = self
            .store
            .read(|c| rows::pending_authorization(c, owner, integration_id))?
            .map(|r| r.authorization);
        if let Some(current) = pending.as_ref().filter(|a| a.reusable(now)) {
            return self.reuse(current.clone(), cause, action);
        }
        let mut batch = Batch::new(now);
        if let Some(old) = &pending {
            let (event, after) = old.ended(AuthorizationState::Expired, None, None, now);
            self.push_event(&mut batch, &event, None, cause, now);
            batch.user_link_authorizations.push(AuthorizationRow {
                authorization: after,
                device: None,
            });
        }
        let id = UserLinkAuthorizationId(self.new_id());
        let new = NewAuthorization {
            id,
            owner: owner.to_string(),
            integration_id: integration_id.to_string(),
            user_code: code.user_code.clone(),
            verification_uri: code.verification_uri.clone(),
            interval_secs: code.interval_secs,
            expires_in_secs: code.expires_in_secs,
        };
        let (event, authorization) = authorship::start(new, now).ok_or_else(|| {
            PortError::retryable(
                "scm_unavailable",
                json!({ "detail": "device_code_invalid" }),
            )
        })?;
        let device = self.ports.secrets.seal(
            &SecretScope::DeviceCode {
                authorization_id: id,
            },
            &code.device_code,
        )?;
        self.push_event(&mut batch, &event, None, cause, now);
        batch.user_link_authorizations.push(AuthorizationRow {
            authorization: authorization.clone(),
            device: Some(columns(device)),
        });
        let view = AuthorizationView {
            authorization,
            link: None,
        };
        if let Some(action) = action {
            let reply = (action.respond)(Produced::UserLinkAuthorization(&view));
            batch.action = Some(action.record(self, cause, None, Some(id.0), reply));
        }
        self.commit(&batch)?;
        if let Some(old) = &pending {
            self.counters
                .user_link_authorization(integration_id, AuthorizationState::Expired.as_str());
            self.accounts.forget_poll(old.id);
        }
        self.counters
            .user_link_authorization(integration_id, AuthorizationState::Pending.as_str());
        tracing::info!(integration_id, authorization_id = %id, "scm.user_link.authorization_started");
        Ok(view)
    }

    pub fn user_link_authorization(
        &self,
        integration_id: &str,
        owner: &str,
        id: UserLinkAuthorizationId,
    ) -> Result<AuthorizationView> {
        self.integration(integration_id)?;
        let row = self
            .store
            .read(|c| rows::authorization(c, id))?
            .filter(|r| r.authorization.integration_id == integration_id)
            .filter(|r| r.authorization.owner == owner)
            .ok_or(EngineError::Rejected(Rejection::NotFound))?;
        self.view_of(row.authorization)
    }

    pub fn user_link(&self, integration_id: &str, owner: &str) -> Result<UserLink> {
        self.integration(integration_id)?;
        self.store
            .read(|c| rows::user_link(c, owner, integration_id))?
            .map(|r| r.link)
            .ok_or(EngineError::Rejected(Rejection::UserLinkNotFound))
    }

    pub fn remove_user_link(
        &self,
        integration_id: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<()> {
        self.integration(integration_id)?;
        let _guard = self.locked()?;
        let in_use = self.user_mode_projects(integration_id)?;
        if !in_use.is_empty() {
            let projects: Vec<_> = in_use
                .iter()
                .map(|p| json!({ "project_id": p.project_id, "name": p.name }))
                .collect();
            return Err(Rejection::UserLinkInUse {
                projects: json!(projects),
            }
            .into());
        }
        let now = self.now();
        let existing = self
            .store
            .read(|c| rows::user_link(c, &cause.actor, integration_id))?;
        let mut batch = Batch::new(now);
        if let Some(row) = &existing {
            let event = Event::UserLinkRemoved {
                link_id: row.link.id,
            };
            self.push_event(&mut batch, &event, None, cause, now);
            batch.removed_user_links.push(row.link.id);
        }
        if let Some(action) = action {
            let reply = (action.respond)(Produced::Nothing);
            let target = existing.as_ref().map(|r| r.link.id.0);
            batch.action = Some(action.record(self, cause, None, target, reply));
        }
        self.commit(&batch)?;
        if let Some(row) = existing {
            tracing::info!(integration_id, link_id = %row.link.id, login = %row.link.login, "scm.user_link.removed");
        }
        Ok(())
    }

    fn end_authorization(
        &self,
        id: UserLinkAuthorizationId,
        state: AuthorizationState,
        failure: Option<AuthorizationFailure>,
        cause: &Cause,
    ) -> Result<bool> {
        let _guard = self.locked()?;
        let now = self.now();
        let Some(row) = self.store.read(|c| rows::authorization(c, id))? else {
            return Ok(false);
        };
        let current = row.authorization;
        if !current.is_pending() {
            return Ok(false);
        }
        let (event, after) = current.ended(state, failure.map(authorship::failure), None, now);
        let mut batch = Batch::new(now);
        self.push_event(&mut batch, &event, None, cause, now);
        batch.user_link_authorizations.push(AuthorizationRow {
            authorization: after,
            device: None,
        });
        self.commit(&batch)?;
        self.accounts.forget_poll(id);
        self.counters
            .user_link_authorization(&current.integration_id, state.as_str());
        tracing::info!(
            integration_id = %current.integration_id,
            authorization_id = %id,
            state = state.as_str(),
            failure = failure.map(|f| f.as_str()),
            "scm.user_link.authorization_ended"
        );
        Ok(true)
    }

    fn slow_authorization(
        &self,
        id: UserLinkAuthorizationId,
        offered: Option<u64>,
        cause: &Cause,
    ) -> Result<()> {
        let _guard = self.locked()?;
        let now = self.now();
        let Some(row) = self.store.read(|c| rows::authorization(c, id))? else {
            return Ok(());
        };
        if !row.authorization.is_pending() {
            return Ok(());
        }
        let Some((event, after)) = row.authorization.slowed(offered, now) else {
            return Ok(());
        };
        let mut batch = Batch::new(now);
        self.push_event(&mut batch, &event, None, cause, now);
        tracing::info!(authorization_id = %id, interval_secs = after.interval_secs, "scm.user_link.authorization_slowed");
        batch.user_link_authorizations.push(AuthorizationRow {
            authorization: after,
            device: row.device,
        });
        self.commit(&batch)?;
        Ok(())
    }

    pub(crate) async fn user_link_poll_job(&self, job: &ClaimedJob) -> Execution {
        let id: UserLinkAuthorizationId = field(&job.payload, "authorization_id")?;
        let cause = job_cause(job);
        let row = self
            .store
            .read(|c| rows::authorization(c, id))
            .map_err(|e| internal(e.into()))?;
        let now = self.now();
        let Some(AuthorizationRow {
            authorization,
            device: Some(device),
        }) = row.filter(|r| r.authorization.is_pending() && !r.authorization.has_expired(now))
        else {
            return Ok(Effect::Skipped);
        };
        if !self
            .accounts
            .take_poll(id, authorization.interval_ms(), now)
        {
            return Ok(Effect::Skipped);
        }
        let scope = SecretScope::DeviceCode {
            authorization_id: id,
        };
        let code = match self.ports.secrets.open(&scope, &sealed(&device)) {
            Ok(code) => code,
            Err(e) if !e.retryable => {
                self.end_authorization(id, AuthorizationState::Expired, None, &cause)
                    .map_err(internal)?;
                return Ok(Effect::Nothing);
            }
            Err(e) => return Err(e),
        };
        let integration = authorization.integration_id.clone();
        let end = |state, failure| {
            self.end_authorization(id, state, failure, &cause)
                .map(|_| Effect::Nothing)
                .map_err(internal)
        };
        match self.ports.scm.poll_device_flow(&integration, &code).await? {
            DevicePoll::Pending => Ok(Effect::Nothing),
            DevicePoll::SlowDown { interval_secs } => {
                self.slow_authorization(id, interval_secs, &cause)
                    .map_err(internal)?;
                Ok(Effect::Nothing)
            }
            DevicePoll::Expired => end(AuthorizationState::Expired, None),
            DevicePoll::Denied => end(AuthorizationState::Denied, None),
            DevicePoll::Failed { code } => end(AuthorizationState::Failed, Some(code)),
            DevicePoll::Token(tokens) => {
                self.complete_authorization(authorization, tokens, &cause)
                    .await
            }
        }
    }

    async fn complete_authorization(
        &self,
        authorization: UserLinkAuthorization,
        tokens: UserTokens,
        cause: &Cause,
    ) -> Execution {
        let id = authorization.id;
        let failed = |failure| {
            self.end_authorization(id, AuthorizationState::Failed, Some(failure), cause)
                .map(|_| Effect::Nothing)
                .map_err(internal)
        };
        let received = self.now();
        let expiry = TokenExpiry::of(
            received,
            tokens.expires_in_secs,
            tokens.refresh_expires_in_secs,
        );
        let (Some(refresh), Some(expiry)) = (tokens.refresh_token, expiry) else {
            return failed(AuthorizationFailure::TokenNotExpiring);
        };
        // The device code is spent once GitHub hands out a token, so a lookup that fails cannot
        // be retried by polling again; the operator starts a new link instead.
        let identity = match self
            .ports
            .scm
            .user_identity(&authorization.integration_id, &tokens.access_token)
            .await
        {
            Ok(IdentityLookup::Found { identity, .. }) if identity.is_valid() => identity,
            Ok(_) => return failed(AuthorizationFailure::IdentityUnavailable),
            Err(e) => {
                tracing::warn!(authorization_id = %id, code = %e.code, "user identity lookup failed");
                return failed(AuthorizationFailure::IdentityUnavailable);
            }
        };
        let guard = self.locked().map_err(internal)?;
        let now = self.now();
        let current = self
            .store
            .read(|c| rows::authorization(c, id))
            .map_err(|e| internal(e.into()))?
            .filter(|r| r.authorization.is_pending());
        let Some(current) = current else {
            return Ok(Effect::Skipped);
        };
        let existing = self
            .store
            .read(|c| {
                rows::user_link(
                    c,
                    &current.authorization.owner,
                    &current.authorization.integration_id,
                )
            })
            .map_err(|e| internal(e.into()))?;
        let linked = authorship::link(
            &current.authorization,
            existing.as_ref().map(|r| &r.link),
            UserLinkId(self.new_id()),
            identity,
            expiry,
            now,
        );
        let link_id = linked.link.id;
        let access = self.ports.secrets.seal(
            &token_scope(link_id, UserTokenKind::Access),
            &tokens.access_token,
        )?;
        let refresh = self
            .ports
            .secrets
            .seal(&token_scope(link_id, UserTokenKind::Refresh), &refresh)?;
        let mut batch = Batch::new(now);
        for event in &linked.events {
            self.push_event(&mut batch, event, None, cause, now);
        }
        batch.user_link_authorizations.push(AuthorizationRow {
            authorization: linked.authorization,
            device: None,
        });
        batch.user_links.push(UserLinkRow {
            link: linked.link.clone(),
            access: columns(access),
            refresh: columns(refresh),
        });
        self.commit(&batch).map_err(internal)?;
        drop(guard);
        self.accounts.forget_poll(id);
        self.counters.user_link_authorization(
            &linked.link.integration_id,
            AuthorizationState::Completed.as_str(),
        );
        tracing::info!(
            integration_id = %linked.link.integration_id,
            login = %linked.link.login,
            "scm.user_link.linked"
        );
        Ok(Effect::Nothing)
    }

    fn open_token(
        &self,
        link_id: UserLinkId,
        kind: UserTokenKind,
        columns: &SealedColumns,
    ) -> Result<Option<SecretText>> {
        match self
            .ports
            .secrets
            .open(&token_scope(link_id, kind), &sealed(columns))
        {
            Ok(token) => Ok(Some(token)),
            Err(e) if !e.retryable => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn mark_relink(
        &self,
        row: &UserLinkRow,
        cause_code: RelinkCause,
        cause: &Cause,
    ) -> Result<LinkCredential> {
        let _guard = self.locked()?;
        let now = self.now();
        let Some(current) = self
            .store
            .read(|c| rows::user_link_by_id(c, row.link.id))?
            .filter(|r| r.link.linked_at == row.link.linked_at && r.link.is_active())
        else {
            return Ok(LinkCredential::Relink(cause_code));
        };
        let (event, after) = current.link.relink(cause_code, now);
        let mut batch = Batch::new(now);
        self.push_event(&mut batch, &event, None, cause, now);
        batch.user_links.push(UserLinkRow {
            link: after,
            ..current
        });
        self.commit(&batch)?;
        tracing::warn!(
            integration_id = %row.link.integration_id,
            cause = cause_code.as_str(),
            "scm.user_link.relink_required"
        );
        Ok(LinkCredential::Relink(cause_code))
    }

    // The provider refused the token it was given: the link needs a relink, unless it was linked
    // again meanwhile, in which case the job runs again with the new token.
    pub(crate) fn revoke_user_link(
        &self,
        used: &UserLink,
    ) -> std::result::Result<UserLinkCause, PortError> {
        let cause = self.cause("system:user_link:use");
        let row = self
            .store
            .read(|c| rows::user_link_by_id(c, used.id))
            .map_err(|e| internal(e.into()))?;
        let Some(row) = row else {
            return Ok(UserLinkCause::Missing);
        };
        if row.link.linked_at != used.linked_at {
            return Err(PortError::retryable(
                "user_link_changed",
                json!({ "integration_id": used.integration_id }),
            ));
        }
        match self
            .mark_relink(&row, RelinkCause::Revoked, &cause)
            .map_err(internal)?
        {
            LinkCredential::Relink(c) => Ok(c.into()),
            LinkCredential::Ready(_) => Ok(UserLinkCause::Revoked),
        }
    }

    // GitHub refresh tokens are single use and rotate on every refresh, so refreshes of one link
    // never overlap and the new pair is committed before the access token is handed out.
    pub async fn refresh_user_link(
        &self,
        link_id: UserLinkId,
        reason: RefreshReason,
    ) -> Result<LinkCredential> {
        let lock = self.accounts.link_lock(link_id);
        let _held = lock.lock().await;
        let cause = self.cause(&format!("system:user_link:{}", reason.as_str()));
        let row = self
            .store
            .read(|c| rows::user_link_by_id(c, link_id))?
            .ok_or(EngineError::Rejected(Rejection::UserLinkNotFound))?;
        let link = &row.link;
        if !link.is_active() {
            return Ok(LinkCredential::Relink(
                link.relink_cause.unwrap_or(RelinkCause::RefreshFailed),
            ));
        }
        let now = self.now();
        let skip = match reason {
            RefreshReason::Use => link.access_fresh(now),
            RefreshReason::Proactive => !link.proactive_refresh_due(now),
        };
        if skip {
            return match self.open_token(link_id, UserTokenKind::Access, &row.access)? {
                Some(token) => Ok(LinkCredential::Ready(token)),
                None => self.mark_relink(&row, RelinkCause::RefreshFailed, &cause),
            };
        }
        if link.refresh_expired(now) {
            return self.mark_relink(&row, RelinkCause::Expired, &cause);
        }
        let Some(refresh) = self.open_token(link_id, UserTokenKind::Refresh, &row.refresh)? else {
            return self.mark_relink(&row, RelinkCause::RefreshFailed, &cause);
        };
        let integration = link.integration_id.as_str();
        let answer = match self
            .ports
            .scm
            .refresh_user_token(integration, &refresh)
            .await
        {
            Ok(answer) => answer,
            Err(e) => {
                self.counters
                    .user_link_refresh(integration, RefreshOutcome::Failed.as_str());
                tracing::warn!(integration_id = integration, link_id = %link_id, code = %e.code, "user link refresh failed");
                return Err(e.into());
            }
        };
        let tokens = match answer {
            TokenRefresh::Refreshed(tokens) => tokens,
            TokenRefresh::Rejected => {
                self.counters
                    .user_link_refresh(integration, RefreshOutcome::Rejected.as_str());
                return self.mark_relink(&row, RelinkCause::RefreshFailed, &cause);
            }
        };
        let received = self.now();
        let expiry = TokenExpiry::of(
            received,
            tokens.expires_in_secs,
            tokens.refresh_expires_in_secs,
        );
        let (Some(new_refresh), Some(expiry)) = (tokens.refresh_token.as_ref(), expiry) else {
            self.counters
                .user_link_refresh(integration, RefreshOutcome::Rejected.as_str());
            return self.mark_relink(&row, RelinkCause::RefreshFailed, &cause);
        };
        let access = self.ports.secrets.seal(
            &token_scope(link_id, UserTokenKind::Access),
            &tokens.access_token,
        )?;
        let refreshed = self
            .ports
            .secrets
            .seal(&token_scope(link_id, UserTokenKind::Refresh), new_refresh)?;
        let guard = self.locked()?;
        let now = self.now();
        let current = self
            .store
            .read(|c| rows::user_link_by_id(c, link_id))?
            .ok_or(EngineError::Rejected(Rejection::UserLinkNotFound))?;
        if current.link.linked_at != link.linked_at {
            drop(guard);
            return match self.open_token(link_id, UserTokenKind::Access, &current.access)? {
                Some(token) => Ok(LinkCredential::Ready(token)),
                None => Ok(LinkCredential::Relink(RelinkCause::RefreshFailed)),
            };
        }
        let (event, after) = current.link.refreshed(expiry, now);
        let mut batch = Batch::new(now);
        self.push_event(&mut batch, &event, None, &cause, now);
        batch.user_links.push(UserLinkRow {
            link: after,
            access: columns(access),
            refresh: columns(refreshed),
        });
        self.commit(&batch)?;
        drop(guard);
        self.counters
            .user_link_refresh(integration, RefreshOutcome::Refreshed.as_str());
        tracing::info!(integration_id = integration, link_id = %link_id, reason = reason.as_str(), "scm.user_link.refreshed");
        Ok(LinkCredential::Ready(tokens.access_token))
    }

    pub(crate) async fn user_link_refresh_job(&self, job: &ClaimedJob) -> Execution {
        let id: UserLinkId = field(&job.payload, "link_id")?;
        match self.refresh_user_link(id, RefreshReason::Proactive).await {
            Ok(_) => Ok(Effect::Nothing),
            Err(EngineError::Rejected(Rejection::UserLinkNotFound)) => Ok(Effect::Skipped),
            Err(EngineError::Port(e)) => Err(e),
            Err(e) => Err(internal(e)),
        }
    }

    pub(crate) fn reconcile_user_links(
        &self,
        now: Timestamp,
        cause: &Cause,
        jobs: &mut Batch,
    ) -> Result<()> {
        let (pending, links) = self.store.read(|c| {
            Ok((
                rows::pending_authorizations(c)?,
                rows::active_user_links(c)?,
            ))
        })?;
        for authorization in &pending {
            if authorization.has_expired(now) {
                self.end_authorization(authorization.id, AuthorizationState::Expired, None, cause)?;
            } else if let Some(job) = authorization.poll_job(now) {
                self.push_job(jobs, job, None, cause);
            }
        }
        for link in &links {
            if let Some(job) = link.refresh_job(now) {
                self.push_job(jobs, job, None, cause);
            }
        }
        Ok(())
    }
}
