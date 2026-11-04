use serde_json::json;

use crate::event::Event;
use crate::ids::{UserLinkAuthorizationId, UserLinkId};
use crate::job::{Job, JobKind};
use crate::model::Coded;
use crate::time::Timestamp;
use crate::vocab::{AuthorizationFailure, AuthorizationState, RelinkCause, UserLinkState};

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
