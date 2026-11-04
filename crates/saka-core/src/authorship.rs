use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::event::Event;
use crate::ids::{ProjectId, UserLinkAuthorizationId, UserLinkId};
use crate::job::{Job, JobKind};
use crate::model::{Blocker, Coded};
use crate::rejection::Rejection;
use crate::time::Timestamp;
use crate::vocab::{
    AuthorSource, AuthorizationFailure, AuthorizationState, AuthorshipMode, BlockerCode,
    ChangeCredentialKind, RelinkCause, ResumeTrigger, StagePhase, TokenInvalidCause, UserLinkCause,
    UserLinkState, UserTokenCause,
};

pub const REUSE_MIN_REMAINING_MS: i64 = 60_000;
pub const ACCESS_REFRESH_MARGIN_MS: i64 = 5 * 60_000;
pub const PROACTIVE_REFRESH_WINDOW_MS: i64 = 7 * DAY_MS;
pub const DAY_MS: i64 = 86_400_000;
pub const SLOW_DOWN_STEP_SECS: u32 = 5;
pub const INTERVAL_MIN_SECS: u32 = 1;
pub const INTERVAL_MAX_SECS: u32 = 300;
pub const USER_CODE_MAX: usize = 32;
pub const LOGIN_MAX: usize = 39;
pub const DISPLAY_NAME_MAX: usize = 255;
pub const USER_TOKEN_PREFIX: &str = "github_pat_";
pub const USER_TOKEN_MIN_BYTES: usize = 40;
pub const USER_TOKEN_MAX_BYTES: usize = 255;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserLink {
    pub id: UserLinkId,
    pub owner: String,
    pub integration_id: String,
    pub provider_user_id: u64,
    pub login: String,
    pub display_name: Option<String>,
    pub email: String,
    pub state: UserLinkState,
    pub relink_cause: Option<RelinkCause>,
    pub access_expires_at: Timestamp,
    pub refresh_expires_at: Timestamp,
    pub linked_at: Timestamp,
    pub refreshed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl UserLink {
    pub fn is_active(&self) -> bool {
        self.state == UserLinkState::Active
    }

    pub fn git_identity(&self) -> GitIdentity {
        GitIdentity {
            name: self
                .display_name
                .clone()
                .unwrap_or_else(|| self.login.clone()),
            email: self.email.clone(),
        }
    }

    // A link that cannot be used says why; one that was never marked keeps the safest cause.
    pub fn problem(&self) -> Option<UserLinkCause> {
        (!self.is_active())
            .then(|| UserLinkCause::from(self.relink_cause.unwrap_or(RelinkCause::RefreshFailed)))
    }

    pub fn access_fresh(&self, now: Timestamp) -> bool {
        self.access_expires_at.0.saturating_sub(now.0) > ACCESS_REFRESH_MARGIN_MS
    }

    pub fn refresh_expired(&self, now: Timestamp) -> bool {
        self.refresh_expires_at <= now
    }

    pub fn proactive_refresh_due(&self, now: Timestamp) -> bool {
        self.is_active()
            && self.refresh_expires_at.0.saturating_sub(now.0) < PROACTIVE_REFRESH_WINDOW_MS
    }

    #[must_use]
    pub fn refresh_job(&self, now: Timestamp) -> Option<Job> {
        self.proactive_refresh_due(now).then(|| {
            Job::new(
                JobKind::ScmUserLinkRefresh,
                format!("{}:{}", self.id, now.0.div_euclid(DAY_MS)),
                json!({ "link_id": self.id }),
                now,
            )
        })
    }

    #[must_use]
    pub fn relink(&self, cause: RelinkCause, now: Timestamp) -> (Event, UserLink) {
        let event = Event::UserLinkRelinkRequired {
            link_id: self.id,
            cause,
        };
        let after = UserLink {
            state: UserLinkState::NeedsRelink,
            relink_cause: Some(cause),
            updated_at: now,
            ..self.clone()
        };
        (event, after)
    }

    #[must_use]
    pub fn refreshed(&self, expiry: TokenExpiry, now: Timestamp) -> (Event, UserLink) {
        let event = Event::UserLinkRefreshed {
            link_id: self.id,
            access_expires_at: expiry.access_expires_at,
            refresh_expires_at: expiry.refresh_expires_at,
        };
        let after = UserLink {
            access_expires_at: expiry.access_expires_at,
            refresh_expires_at: expiry.refresh_expires_at,
            refreshed_at: Some(now),
            updated_at: now,
            ..self.clone()
        };
        (event, after)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenExpiry {
    pub access_expires_at: Timestamp,
    pub refresh_expires_at: Timestamp,
}

impl TokenExpiry {
    // Only short-lived user tokens are accepted: both lifetimes must be present and positive.
    pub fn of(
        now: Timestamp,
        access_secs: Option<u64>,
        refresh_secs: Option<u64>,
    ) -> Option<TokenExpiry> {
        let at = |secs: u64| {
            i64::try_from(secs)
                .ok()
                .filter(|s| *s > 0)
                .map(|s| now.plus_ms(s.saturating_mul(1000)))
        };
        Some(TokenExpiry {
            access_expires_at: at(access_secs?)?,
            refresh_expires_at: at(refresh_secs?)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub provider_user_id: u64,
    pub login: String,
    pub display_name: Option<String>,
    pub email: String,
}

impl Identity {
    pub fn is_valid(&self) -> bool {
        self.provider_user_id >= 1
            && (1..=LOGIN_MAX).contains(&self.login.len())
            && self
                .display_name
                .as_ref()
                .is_none_or(|n| (1..=DISPLAY_NAME_MAX).contains(&n.len()))
            && (3..=254).contains(&self.email.len())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Linked {
    pub events: Vec<Event>,
    pub link: UserLink,
    pub authorization: UserLinkAuthorization,
}

// A relink to the same integration keeps the link id, so anything that points at it stays valid.
#[must_use]
pub fn link(
    authorization: &UserLinkAuthorization,
    existing: Option<&UserLink>,
    new_id: UserLinkId,
    identity: Identity,
    expiry: TokenExpiry,
    now: Timestamp,
) -> Linked {
    let id = existing.map_or(new_id, |l| l.id);
    let replaced_login = existing
        .filter(|l| l.provider_user_id != identity.provider_user_id)
        .map(|l| l.login.clone());
    let link = UserLink {
        id,
        owner: authorization.owner.clone(),
        integration_id: authorization.integration_id.clone(),
        provider_user_id: identity.provider_user_id,
        login: identity.login,
        display_name: identity.display_name,
        email: identity.email,
        state: UserLinkState::Active,
        relink_cause: None,
        access_expires_at: expiry.access_expires_at,
        refresh_expires_at: expiry.refresh_expires_at,
        linked_at: now,
        refreshed_at: None,
        created_at: existing.map_or(now, |l| l.created_at),
        updated_at: now,
    };
    let linked = Event::UserLinked {
        link_id: id,
        integration_id: link.integration_id.clone(),
        authorization_id: authorization.id,
        provider_user_id: link.provider_user_id,
        login: link.login.clone(),
        replaced_login,
    };
    let (ended, authorization) =
        authorization.ended(AuthorizationState::Completed, None, Some(id), now);
    Linked {
        events: vec![linked, ended],
        link,
        authorization,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserLinkAuthorization {
    pub id: UserLinkAuthorizationId,
    pub owner: String,
    pub integration_id: String,
    pub state: AuthorizationState,
    pub user_code: String,
    pub verification_uri: String,
    pub interval_secs: u32,
    pub expires_at: Timestamp,
    pub failure: Option<Coded>,
    pub link_id: Option<UserLinkId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAuthorization {
    pub id: UserLinkAuthorizationId,
    pub owner: String,
    pub integration_id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval_secs: u64,
    pub expires_in_secs: u64,
}

pub fn clamp_interval(secs: u64) -> u32 {
    u32::try_from(secs)
        .unwrap_or(INTERVAL_MAX_SECS)
        .clamp(INTERVAL_MIN_SECS, INTERVAL_MAX_SECS)
}

pub fn failure(code: AuthorizationFailure) -> Coded {
    Coded {
        code: code.as_str().to_string(),
        params: json!({}),
    }
}

#[must_use]
pub fn start(new: NewAuthorization, now: Timestamp) -> Option<(Event, UserLinkAuthorization)> {
    let expires_in_ms = i64::try_from(new.expires_in_secs)
        .ok()
        .filter(|s| *s > 0)?
        .saturating_mul(1000);
    if !(1..=USER_CODE_MAX).contains(&new.user_code.len()) || new.verification_uri.is_empty() {
        return None;
    }
    let authorization = UserLinkAuthorization {
        id: new.id,
        owner: new.owner,
        integration_id: new.integration_id,
        state: AuthorizationState::Pending,
        user_code: new.user_code,
        verification_uri: new.verification_uri,
        interval_secs: clamp_interval(new.interval_secs),
        expires_at: now.plus_ms(expires_in_ms),
        failure: None,
        link_id: None,
        created_at: now,
        updated_at: now,
    };
    let event = Event::UserLinkAuthorizationStarted {
        authorization_id: authorization.id,
        integration_id: authorization.integration_id.clone(),
        expires_at: authorization.expires_at,
    };
    Some((event, authorization))
}

impl UserLinkAuthorization {
    pub fn is_pending(&self) -> bool {
        self.state == AuthorizationState::Pending
    }

    pub fn reusable(&self, now: Timestamp) -> bool {
        self.is_pending() && self.expires_at.0.saturating_sub(now.0) > REUSE_MIN_REMAINING_MS
    }

    pub fn has_expired(&self, now: Timestamp) -> bool {
        self.is_pending() && now >= self.expires_at
    }

    pub fn interval_ms(&self) -> i64 {
        i64::from(self.interval_secs.max(INTERVAL_MIN_SECS)) * 1000
    }

    #[must_use]
    pub fn poll_job(&self, now: Timestamp) -> Option<Job> {
        (self.is_pending() && now < self.expires_at).then(|| {
            Job::new(
                JobKind::ScmUserLinkPoll,
                format!("{}:{}", self.id, now.0.div_euclid(self.interval_ms())),
                json!({ "authorization_id": self.id }),
                now,
            )
        })
    }

    #[must_use]
    pub fn ended(
        &self,
        state: AuthorizationState,
        failure: Option<Coded>,
        link_id: Option<UserLinkId>,
        now: Timestamp,
    ) -> (Event, UserLinkAuthorization) {
        let failure = failure.filter(|_| state == AuthorizationState::Failed);
        let event = Event::UserLinkAuthorizationEnded {
            authorization_id: self.id,
            state,
            failure: failure.clone(),
        };
        let after = UserLinkAuthorization {
            state,
            failure,
            link_id: link_id.filter(|_| state == AuthorizationState::Completed),
            updated_at: now,
            ..self.clone()
        };
        (event, after)
    }

    // GitHub sends the interval to use from now on; without one the documented step is +5 s.
    // A smaller offer never shortens the interval already in use.
    #[must_use]
    pub fn slowed(
        &self,
        offered: Option<u64>,
        now: Timestamp,
    ) -> Option<(Event, UserLinkAuthorization)> {
        let next = match offered {
            Some(secs) => clamp_interval(secs).max(self.interval_secs),
            None => self
                .interval_secs
                .saturating_add(SLOW_DOWN_STEP_SECS)
                .min(INTERVAL_MAX_SECS),
        };
        if next == self.interval_secs {
            return None;
        }
        let event = Event::UserLinkAuthorizationSlowed {
            authorization_id: self.id,
            interval_secs: next,
        };
        let after = UserLinkAuthorization {
            interval_secs: next,
            updated_at: now,
            ..self.clone()
        };
        Some((event, after))
    }
}

impl From<RelinkCause> for UserLinkCause {
    fn from(cause: RelinkCause) -> UserLinkCause {
        match cause {
            RelinkCause::RefreshFailed => UserLinkCause::RefreshFailed,
            RelinkCause::Revoked => UserLinkCause::Revoked,
            RelinkCause::Expired => UserLinkCause::Expired,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAuthor {
    pub source: AuthorSource,
    pub login: Option<String>,
}

impl SessionAuthor {
    pub fn saka() -> SessionAuthor {
        SessionAuthor {
            source: AuthorSource::Saka,
            login: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAuthor {
    pub identity: GitIdentity,
    pub author: SessionAuthor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAuthorship {
    pub project_id: ProjectId,
    pub mode: AuthorshipMode,
    pub mode_updated_at: Timestamp,
    pub token: Option<ProjectUserToken>,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectUserToken {
    pub integration_id: String,
    pub identity: Identity,
    pub expires_at: Option<Timestamp>,
    pub set_at: Timestamp,
    pub invalid_cause: Option<TokenInvalidCause>,
}

impl ProjectUserToken {
    // A token the provider refused stays refused; one past its expiry is expired without a write.
    pub fn problem(&self, now: Timestamp) -> Option<TokenInvalidCause> {
        self.invalid_cause.or_else(|| {
            self.expires_at
                .filter(|at| *at <= now)
                .map(|_| TokenInvalidCause::Expired)
        })
    }

    pub fn git_identity(&self) -> GitIdentity {
        GitIdentity {
            name: self
                .identity
                .display_name
                .clone()
                .unwrap_or_else(|| self.identity.login.clone()),
            email: self.identity.email.clone(),
        }
    }
}

pub fn mode_of(authorship: Option<&ProjectAuthorship>) -> AuthorshipMode {
    authorship.map_or(AuthorshipMode::Bot, |a| a.mode)
}

pub fn token_of(authorship: Option<&ProjectAuthorship>) -> Option<&ProjectUserToken> {
    authorship.and_then(|a| a.token.as_ref())
}

// Setting the mode a project already has changes nothing and records nothing.
#[must_use]
pub fn set_mode(
    project_id: ProjectId,
    current: Option<&ProjectAuthorship>,
    mode: AuthorshipMode,
    now: Timestamp,
) -> Option<(Event, ProjectAuthorship)> {
    if mode_of(current) == mode {
        return None;
    }
    let event = Event::ProjectAuthorshipSet { project_id, mode };
    let after = ProjectAuthorship {
        project_id,
        mode,
        mode_updated_at: now,
        token: current.and_then(|a| a.token.clone()),
        updated_at: now,
    };
    Some((event, after))
}

// The user token mode needs a stored token that is usable right now.
pub fn check_user_pat_mode(
    current: Option<&ProjectAuthorship>,
    now: Timestamp,
) -> Result<(), Rejection> {
    let token = token_of(current).ok_or(Rejection::UserTokenMissing)?;
    match token.problem(now) {
        None => Ok(()),
        Some(cause) => Err(Rejection::UserTokenInvalid { cause }),
    }
}

// Only the shape is checked here; whether the token works is the provider's answer.
pub fn check_user_token_value(value: &str) -> Result<(), Rejection> {
    if !(USER_TOKEN_MIN_BYTES..=USER_TOKEN_MAX_BYTES).contains(&value.len()) {
        return Err(Rejection::Invalid {
            field: "value",
            code: "length",
        });
    }
    if !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Rejection::Invalid {
            field: "value",
            code: "invalid",
        });
    }
    if !value.starts_with(USER_TOKEN_PREFIX) {
        return Err(Rejection::UserTokenNotFineGrained);
    }
    Ok(())
}

// A project that never had a row is in bot mode, so a token stored first records bot mode too.
#[must_use]
pub fn store_token(
    project_id: ProjectId,
    current: Option<&ProjectAuthorship>,
    token: ProjectUserToken,
    now: Timestamp,
) -> (Event, ProjectAuthorship) {
    let event = Event::ProjectUserTokenSet {
        project_id,
        integration_id: token.integration_id.clone(),
        provider_user_id: token.identity.provider_user_id,
        login: token.identity.login.clone(),
        expires_at: token.expires_at,
    };
    let after = ProjectAuthorship {
        project_id,
        mode: mode_of(current),
        mode_updated_at: current.map_or(now, |a| a.mode_updated_at),
        token: Some(token),
        updated_at: now,
    };
    (event, after)
}

#[must_use]
pub fn remove_token(
    current: Option<&ProjectAuthorship>,
    now: Timestamp,
) -> Option<(Event, ProjectAuthorship)> {
    let current = current.filter(|a| a.token.is_some())?;
    let event = Event::ProjectUserTokenRemoved {
        project_id: current.project_id,
    };
    let after = ProjectAuthorship {
        token: None,
        updated_at: now,
        ..current.clone()
    };
    Some((event, after))
}

// Only the token that was used is marked: one stored again meanwhile is left as it is.
#[must_use]
pub fn invalidate_token(
    current: Option<&ProjectAuthorship>,
    used_set_at: Timestamp,
    cause: TokenInvalidCause,
    now: Timestamp,
) -> Option<(Event, ProjectAuthorship)> {
    let current = current?;
    let token = current
        .token
        .as_ref()
        .filter(|t| t.set_at == used_set_at && t.invalid_cause.is_none())?;
    let event = Event::ProjectUserTokenInvalidated {
        project_id: current.project_id,
        cause,
    };
    let after = ProjectAuthorship {
        token: Some(ProjectUserToken {
            invalid_cause: Some(cause),
            ..token.clone()
        }),
        updated_at: now,
        ..current.clone()
    };
    Some((event, after))
}

impl From<TokenInvalidCause> for UserTokenCause {
    fn from(cause: TokenInvalidCause) -> UserTokenCause {
        match cause {
            TokenInvalidCause::Expired => UserTokenCause::Expired,
            TokenInvalidCause::Rejected => UserTokenCause::Rejected,
        }
    }
}

// The token belongs to one integration; a repo on another one never sees it.
pub fn usable_token<'a>(
    current: Option<&'a ProjectAuthorship>,
    integration_id: &str,
    phase: StagePhase,
    now: Timestamp,
) -> Result<&'a ProjectUserToken, CredentialBlock> {
    let blocked = |cause| CredentialBlock::TokenInvalid { cause, phase };
    let token = token_of(current)
        .filter(|t| t.integration_id == integration_id)
        .ok_or(blocked(UserTokenCause::Missing))?;
    match token.problem(now) {
        None => Ok(token),
        Some(cause) => Err(blocked(cause.into())),
    }
}

pub fn user_token_author(
    current: Option<&ProjectAuthorship>,
    integration_id: &str,
    now: Timestamp,
) -> Result<ResolvedAuthor, CredentialBlock> {
    let token = usable_token(current, integration_id, StagePhase::RunningSession, now)?;
    Ok(ResolvedAuthor {
        identity: token.git_identity(),
        author: SessionAuthor {
            source: AuthorSource::UserToken,
            login: Some(token.identity.login.clone()),
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialBlock {
    LinkRequired {
        integration_id: String,
        cause: UserLinkCause,
        phase: StagePhase,
    },
    TokenInvalid {
        cause: UserTokenCause,
        phase: StagePhase,
    },
    AccessDenied {
        credential: ChangeCredentialKind,
        login: String,
        status: u16,
    },
}

impl CredentialBlock {
    pub fn code(&self) -> BlockerCode {
        match self {
            CredentialBlock::LinkRequired { .. } => BlockerCode::UserLinkRequired,
            CredentialBlock::TokenInvalid { .. } => BlockerCode::UserTokenInvalid,
            CredentialBlock::AccessDenied { .. } => BlockerCode::UserAccessDenied,
        }
    }

    pub fn phase(&self) -> StagePhase {
        match self {
            CredentialBlock::LinkRequired { phase, .. }
            | CredentialBlock::TokenInvalid { phase, .. } => *phase,
            CredentialBlock::AccessDenied { .. } => StagePhase::OpeningChange,
        }
    }

    pub fn params(&self) -> Value {
        match self {
            CredentialBlock::LinkRequired {
                integration_id,
                cause,
                phase,
            } => json!({ "integration_id": integration_id, "cause": cause, "phase": phase }),
            CredentialBlock::TokenInvalid { cause, phase } => {
                json!({ "cause": cause, "phase": phase })
            }
            CredentialBlock::AccessDenied {
                credential,
                login,
                status,
            } => json!({
                "credential": credential,
                "login": login,
                "status": status,
                "phase": StagePhase::OpeningChange,
            }),
        }
    }
}

pub fn link_problem(link: Option<&UserLink>) -> Option<UserLinkCause> {
    match link {
        None => Some(UserLinkCause::Missing),
        Some(l) => l.problem(),
    }
}

pub fn link_required(
    integration_id: &str,
    link: Option<&UserLink>,
    phase: StagePhase,
) -> Option<CredentialBlock> {
    link_problem(link).map(|cause| CredentialBlock::LinkRequired {
        integration_id: integration_id.to_string(),
        cause,
        phase,
    })
}

// The author is fixed when a session starts, since a commit cannot change it afterwards.
// The access token's lifetime does not matter here: the session never sees a token.
pub fn user_link_author(
    integration_id: &str,
    link: Option<&UserLink>,
) -> Result<ResolvedAuthor, CredentialBlock> {
    match link.filter(|l| l.is_active()) {
        Some(l) => Ok(ResolvedAuthor {
            identity: l.git_identity(),
            author: SessionAuthor {
                source: AuthorSource::UserLink,
                login: Some(l.login.clone()),
            },
        }),
        None => Err(CredentialBlock::LinkRequired {
            integration_id: integration_id.to_string(),
            cause: link_problem(link).unwrap_or(UserLinkCause::Missing),
            phase: StagePhase::RunningSession,
        }),
    }
}

// An operator's own action ends a credential blocker: a fresh link for its integration, a new
// usable token for the project, or any later change of the project's mode. Nothing else does,
// so it is never retried blindly.
pub fn resume_trigger(
    blocker: &Blocker,
    links: &[UserLink],
    authorship: Option<&ProjectAuthorship>,
    now: Timestamp,
) -> Option<ResumeTrigger> {
    if !matches!(
        blocker.code,
        BlockerCode::UserLinkRequired | BlockerCode::UserTokenInvalid
    ) {
        return None;
    }
    if authorship.is_some_and(|a| a.mode_updated_at > blocker.opened_at) {
        return Some(ResumeTrigger::AuthorshipModeChanged);
    }
    let renewed = if blocker.code == BlockerCode::UserTokenInvalid {
        token_of(authorship)
            .is_some_and(|t| t.set_at > blocker.opened_at && t.problem(now).is_none())
    } else {
        let integration = blocker.params.get("integration_id")?.as_str()?;
        links.iter().any(|l| {
            l.integration_id == integration && l.is_active() && l.linked_at > blocker.opened_at
        })
    };
    renewed.then_some(ResumeTrigger::CredentialRenewed)
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    const NOW: Timestamp = Timestamp(1_767_225_600_000);

    fn pending() -> UserLinkAuthorization {
        let new = NewAuthorization {
            id: UserLinkAuthorizationId(Uuid::from_u128(1)),
            owner: "operator:local".into(),
            integration_id: "github".into(),
            user_code: "WDJB-MJHT".into(),
            verification_uri: "https://github.com/login/device".into(),
            interval_secs: 5,
            expires_in_secs: 900,
        };
        start(new, NOW).unwrap().1
    }

    #[test]
    fn a_pending_authorization_is_reused_only_with_more_than_a_minute_left() {
        let a = pending();
        assert!(a.reusable(NOW));
        assert!(a.reusable(a.expires_at.plus_ms(-60_001)));
        assert!(!a.reusable(a.expires_at.plus_ms(-60_000)));
        assert!(!a.has_expired(a.expires_at.plus_ms(-1)));
        assert!(a.has_expired(a.expires_at));
    }

    #[test]
    fn polls_are_keyed_per_interval_and_stop_at_expiry() {
        let a = pending();
        let first = a.poll_job(NOW).unwrap();
        assert_eq!(first.kind, JobKind::ScmUserLinkPoll);
        assert_eq!(
            a.poll_job(NOW.plus_ms(4_999)).unwrap().idempotency_key,
            first.idempotency_key
        );
        assert_ne!(
            a.poll_job(NOW.plus_ms(5_000)).unwrap().idempotency_key,
            first.idempotency_key
        );
        assert!(a.poll_job(a.expires_at).is_none());
        assert_eq!(first.payload, json!({ "authorization_id": a.id }));
    }

    #[test]
    fn slow_down_takes_the_offered_interval_or_adds_five_seconds() {
        let a = pending();
        let (event, after) = a.slowed(None, NOW).unwrap();
        assert_eq!(after.interval_secs, 10);
        assert_eq!(event.kind(), "scm.user_link.authorization_slowed");
        assert_eq!(a.slowed(Some(15), NOW).unwrap().1.interval_secs, 15);
        assert_eq!(a.slowed(Some(2), NOW), None);
        assert_eq!(a.slowed(Some(9_999), NOW).unwrap().1.interval_secs, 300);
    }

    #[test]
    fn only_a_failed_end_keeps_its_failure_and_only_completed_keeps_the_link() {
        let a = pending();
        let code = failure(AuthorizationFailure::DeviceFlowDisabled);
        let (_, denied) = a.ended(AuthorizationState::Denied, Some(code.clone()), None, NOW);
        assert_eq!(denied.failure, None);
        let (event, failed) = a.ended(AuthorizationState::Failed, Some(code.clone()), None, NOW);
        assert_eq!(failed.failure, Some(code));
        assert_eq!(
            event.payload()["failure"]["code"],
            json!("device_flow_disabled")
        );
    }

    fn identity(id: u64, login: &str) -> Identity {
        Identity {
            provider_user_id: id,
            login: login.into(),
            display_name: None,
            email: format!("{id}+{login}@users.noreply.github.com"),
        }
    }

    #[test]
    fn a_relink_keeps_the_id_and_names_the_account_it_replaced() {
        let a = pending();
        let expiry = TokenExpiry::of(NOW, Some(28_800), Some(15_897_600)).unwrap();
        let new_id = UserLinkId(Uuid::from_u128(9));
        let first = link(&a, None, new_id, identity(1, "octo"), expiry, NOW);
        assert_eq!(first.link.id, new_id);
        assert_eq!(first.authorization.state, AuthorizationState::Completed);
        assert_eq!(first.authorization.link_id, Some(new_id));
        assert_eq!(first.events[0].payload()["replaced_login"], json!(null));
        let again = link(
            &a,
            Some(&first.link),
            UserLinkId(Uuid::from_u128(10)),
            identity(2, "other"),
            expiry,
            NOW,
        );
        assert_eq!(again.link.id, new_id);
        assert_eq!(again.events[0].payload()["replaced_login"], json!("octo"));
        assert_eq!(
            again.link.access_expires_at,
            NOW.plus_ms(28_800_000),
            "{:?}",
            again.link
        );
    }

    fn linked_at(at: Timestamp, state: UserLinkState) -> UserLink {
        let expiry = TokenExpiry::of(at, Some(28_800), Some(86_400)).unwrap();
        let mut l = link(
            &pending(),
            None,
            UserLinkId(Uuid::from_u128(9)),
            Identity {
                display_name: Some("Octo Cat".into()),
                ..identity(1, "octo")
            },
            expiry,
            at,
        )
        .link;
        if state == UserLinkState::NeedsRelink {
            l = l.relink(RelinkCause::Revoked, at).1;
        }
        l
    }

    #[test]
    fn a_session_authors_as_the_active_link_or_says_why_it_cannot() {
        let active = linked_at(NOW, UserLinkState::Active);
        let resolved = user_link_author("github", Some(&active)).unwrap();
        assert_eq!(
            resolved.identity,
            GitIdentity {
                name: "Octo Cat".into(),
                email: "1+octo@users.noreply.github.com".into()
            }
        );
        assert_eq!(resolved.author.login.as_deref(), Some("octo"));
        let nameless = UserLink {
            display_name: None,
            ..active.clone()
        };
        assert_eq!(nameless.git_identity().name, "octo");
        for (link, cause) in [
            (None, "missing"),
            (Some(linked_at(NOW, UserLinkState::NeedsRelink)), "revoked"),
        ] {
            let block = user_link_author("github", link.as_ref()).unwrap_err();
            assert_eq!(block.code(), BlockerCode::UserLinkRequired);
            assert_eq!(
                block.params(),
                json!({ "integration_id": "github", "cause": cause, "phase": "running_session" })
            );
        }
        let expired_access = UserLink {
            access_expires_at: NOW.plus_ms(-1),
            ..active
        };
        assert!(
            user_link_author("github", Some(&expired_access)).is_ok(),
            "the access token is not needed to author"
        );
    }

    #[test]
    fn only_a_later_link_or_mode_change_resumes_a_link_blocker() {
        let blocker = |code, opened_at| Blocker {
            id: crate::ids::BlockerId(Uuid::from_u128(1)),
            work_item_id: crate::ids::WorkItemId(Uuid::from_u128(2)),
            stage_run_id: None,
            code,
            params: json!({ "integration_id": "github", "cause": "revoked", "phase": "opening_change" }),
            resolution: None,
            opened_at,
            resolved_at: None,
        };
        let b = blocker(BlockerCode::UserLinkRequired, NOW);
        let later = linked_at(NOW.plus_ms(1), UserLinkState::Active);
        let same = linked_at(NOW, UserLinkState::Active);
        let broken = linked_at(NOW.plus_ms(1), UserLinkState::NeedsRelink);
        let other = UserLink {
            integration_id: "ghes".into(),
            ..later.clone()
        };
        assert_eq!(
            resume_trigger(&b, std::slice::from_ref(&later), None, NOW),
            Some(ResumeTrigger::CredentialRenewed)
        );
        assert_eq!(resume_trigger(&b, &[same, broken, other], None, NOW), None);
        let mode = |at| ProjectAuthorship {
            project_id: ProjectId(Uuid::from_u128(3)),
            mode: AuthorshipMode::Bot,
            mode_updated_at: at,
            token: None,
            updated_at: at,
        };
        assert_eq!(
            resume_trigger(&b, &[], Some(&mode(NOW.plus_ms(1))), NOW),
            Some(ResumeTrigger::AuthorshipModeChanged)
        );
        assert_eq!(resume_trigger(&b, &[], Some(&mode(NOW)), NOW), None);
        let denied = blocker(BlockerCode::UserAccessDenied, NOW);
        assert_eq!(
            resume_trigger(&denied, &[later], Some(&mode(NOW.plus_ms(1))), NOW),
            None
        );
    }

    fn with_token(at: Timestamp, expires_at: Option<Timestamp>) -> ProjectAuthorship {
        let token = ProjectUserToken {
            integration_id: "github".into(),
            identity: Identity {
                display_name: Some("Ann Lee".into()),
                ..identity(7, "ann")
            },
            expires_at,
            set_at: at,
            invalid_cause: None,
        };
        store_token(ProjectId(Uuid::from_u128(3)), None, token, at).1
    }

    #[test]
    fn only_a_fine_grained_token_of_the_right_size_is_accepted() {
        let ok = format!("{USER_TOKEN_PREFIX}{}", "a".repeat(29));
        assert_eq!(ok.len(), 40);
        assert_eq!(check_user_token_value(&ok), Ok(()));
        let classic = format!("ghp_{}", "a".repeat(36));
        assert_eq!(
            check_user_token_value(&classic),
            Err(Rejection::UserTokenNotFineGrained)
        );
        for (value, code) in [
            (ok[..39].to_string(), "length"),
            (format!("{ok}{}", "a".repeat(216)), "length"),
            (ok.replacen('a', " ", 1), "invalid"),
            (ok.replacen('a', "é", 1), "invalid"),
        ] {
            assert_eq!(
                check_user_token_value(&value),
                Err(Rejection::Invalid {
                    field: "value",
                    code
                }),
                "{value}"
            );
        }
    }

    #[test]
    fn a_token_authors_until_it_expires_or_is_refused_and_only_on_its_integration() {
        let expires = NOW.plus_ms(3_600_000);
        let a = with_token(NOW, Some(expires));
        assert_eq!(a.mode, AuthorshipMode::Bot);
        assert_eq!(a.mode_updated_at, NOW);
        let resolved = user_token_author(Some(&a), "github", NOW).unwrap();
        assert_eq!(resolved.identity.name, "Ann Lee");
        assert_eq!(resolved.author.source, AuthorSource::UserToken);
        assert_eq!(resolved.author.login.as_deref(), Some("ann"));
        let block = |r: Result<ResolvedAuthor, CredentialBlock>| r.unwrap_err().params();
        assert_eq!(
            block(user_token_author(Some(&a), "github", expires)),
            json!({ "cause": "expired", "phase": "running_session" })
        );
        assert_eq!(
            block(user_token_author(Some(&a), "ghes", NOW)),
            json!({ "cause": "missing", "phase": "running_session" })
        );
        assert_eq!(
            block(user_token_author(None, "github", NOW)),
            json!({ "cause": "missing", "phase": "running_session" })
        );
        let (event, refused) =
            invalidate_token(Some(&a), NOW, TokenInvalidCause::Rejected, NOW).unwrap();
        assert_eq!(event.kind(), "workspace.project.user_token_invalidated");
        assert_eq!(
            block(user_token_author(Some(&refused), "github", NOW)),
            json!({ "cause": "rejected", "phase": "running_session" })
        );
        assert_eq!(
            check_user_pat_mode(Some(&refused), NOW),
            Err(Rejection::UserTokenInvalid {
                cause: TokenInvalidCause::Rejected
            })
        );
        assert_eq!(
            check_user_pat_mode(None, NOW),
            Err(Rejection::UserTokenMissing)
        );
        assert_eq!(check_user_pat_mode(Some(&a), NOW), Ok(()));
        assert!(
            invalidate_token(Some(&a), NOW.plus_ms(1), TokenInvalidCause::Rejected, NOW).is_none(),
            "a token stored again meanwhile is not the one refused"
        );
        assert!(invalidate_token(Some(&refused), NOW, TokenInvalidCause::Rejected, NOW).is_none());
    }

    #[test]
    fn a_stored_token_event_and_its_removal_never_carry_the_value() {
        let a = with_token(NOW, None);
        let (event, _) = store_token(
            a.project_id,
            Some(&a),
            a.token.clone().unwrap(),
            NOW.plus_ms(1),
        );
        assert_eq!(event.kind(), "workspace.project.user_token_set");
        assert_eq!(
            event.payload(),
            json!({
                "project_id": a.project_id,
                "integration_id": "github",
                "provider_user_id": 7,
                "login": "ann",
                "expires_at": null,
            })
        );
        let (removed, after) = remove_token(Some(&a), NOW).unwrap();
        assert_eq!(removed.kind(), "workspace.project.user_token_removed");
        assert_eq!(after.token, None);
        assert!(remove_token(Some(&after), NOW).is_none());
        let (_, user_pat) = set_mode(a.project_id, Some(&a), AuthorshipMode::UserPat, NOW).unwrap();
        assert_eq!(user_pat.token, a.token, "a mode change keeps the token");
    }

    #[test]
    fn only_a_later_usable_token_or_mode_change_resumes_a_token_blocker() {
        let opened_at = NOW;
        let blocker = Blocker {
            id: crate::ids::BlockerId(Uuid::from_u128(1)),
            work_item_id: crate::ids::WorkItemId(Uuid::from_u128(2)),
            stage_run_id: None,
            code: BlockerCode::UserTokenInvalid,
            params: json!({ "cause": "rejected", "phase": "opening_change" }),
            resolution: None,
            opened_at,
            resolved_at: None,
        };
        let earlier = ProjectAuthorship {
            mode: AuthorshipMode::UserPat,
            ..with_token(NOW.plus_ms(-10), None)
        };
        assert_eq!(resume_trigger(&blocker, &[], Some(&earlier), NOW), None);
        let renewed = ProjectAuthorship {
            mode_updated_at: NOW.plus_ms(-10),
            ..with_token(NOW.plus_ms(1), None)
        };
        assert_eq!(
            resume_trigger(&blocker, &[], Some(&renewed), NOW.plus_ms(2)),
            Some(ResumeTrigger::CredentialRenewed)
        );
        let short = ProjectAuthorship {
            mode_updated_at: NOW.plus_ms(-10),
            ..with_token(NOW.plus_ms(1), Some(NOW.plus_ms(5)))
        };
        assert_eq!(
            resume_trigger(&blocker, &[], Some(&short), NOW.plus_ms(5)),
            None,
            "a new token already expired does not resume"
        );
        let switched = ProjectAuthorship {
            mode_updated_at: NOW.plus_ms(1),
            ..earlier
        };
        assert_eq!(
            resume_trigger(&blocker, &[], Some(&switched), NOW),
            Some(ResumeTrigger::AuthorshipModeChanged)
        );
    }

    #[test]
    fn setting_the_current_mode_records_nothing() {
        let project = ProjectId(Uuid::from_u128(3));
        assert!(set_mode(project, None, AuthorshipMode::Bot, NOW).is_none());
        let (event, after) = set_mode(project, None, AuthorshipMode::User, NOW).unwrap();
        assert_eq!(event.kind(), "workspace.project.authorship_set");
        assert_eq!(
            event.payload(),
            json!({ "project_id": project, "mode": "user" })
        );
        assert!(set_mode(project, Some(&after), AuthorshipMode::User, NOW).is_none());
        assert_eq!(mode_of(Some(&after)), AuthorshipMode::User);
        assert_eq!(mode_of(None), AuthorshipMode::Bot);
    }

    #[test]
    fn tokens_without_both_lifetimes_are_not_accepted() {
        assert!(TokenExpiry::of(NOW, Some(28_800), None).is_none());
        assert!(TokenExpiry::of(NOW, None, Some(1)).is_none());
        assert!(TokenExpiry::of(NOW, Some(0), Some(1)).is_none());
    }

    #[test]
    fn a_proactive_refresh_is_due_once_a_day_inside_the_last_week() {
        let a = pending();
        let expiry = TokenExpiry::of(NOW, Some(28_800), Some(8 * 86_400)).unwrap();
        let l = link(
            &a,
            None,
            UserLinkId(Uuid::from_u128(9)),
            identity(1, "octo"),
            expiry,
            NOW,
        )
        .link;
        assert!(l.refresh_job(NOW).is_none());
        let inside = NOW.plus_ms(DAY_MS + 1);
        let job = l.refresh_job(inside).unwrap();
        assert_eq!(
            l.refresh_job(inside.plus_ms(3_600_000))
                .unwrap()
                .idempotency_key,
            job.idempotency_key
        );
        assert_ne!(
            l.refresh_job(inside.plus_ms(DAY_MS))
                .unwrap()
                .idempotency_key,
            job.idempotency_key
        );
        let (_, relinked) = l.relink(RelinkCause::RefreshFailed, inside);
        assert!(relinked.refresh_job(inside).is_none());
        assert!(l.access_fresh(NOW.plus_ms(28_800_000 - 300_001)));
        assert!(!l.access_fresh(NOW.plus_ms(28_800_000 - 300_000)));
    }
}
