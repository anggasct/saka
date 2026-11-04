#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use saka_core::authorship::{DAY_MS, UserLink};
use saka_core::ids::UserLinkAuthorizationId;
use saka_core::rejection::Rejection;
use saka_core::secret::SecretScope;
use saka_core::time::Timestamp;
use saka_core::vocab::{
    AuthorizationFailure, AuthorizationState, RefreshReason, RelinkCause, UserLinkState,
    UserTokenKind,
};
use saka_engine::{AuthorizationView, EngineError, LinkCredential, OPERATOR};
use saka_ports::{Clock, Sealed, SecretStore};
use saka_testkit::{ACCESS_TOKEN_SECS, DeviceAnswer, FakeClock, Sim, SimConfig, Step};
use serde_json::json;

const GITHUB: &str = "github";

fn start(sim: &Sim) -> AuthorizationView {
    let engine = sim.engine();
    let cause = engine.cause(OPERATOR);
    sim.block_on(engine.start_user_link(GITHUB, &cause, None))
        .unwrap()
}

fn authorization(sim: &Sim, id: UserLinkAuthorizationId) -> AuthorizationView {
    sim.engine()
        .user_link_authorization(GITHUB, OPERATOR, id)
        .unwrap()
}

fn link(sim: &Sim) -> Option<UserLink> {
    sim.engine().user_link(GITHUB, OPERATOR).ok()
}

fn kinds(sim: &Sim) -> Vec<String> {
    let mut out = Vec::new();
    let mut after = 0;
    loop {
        let page = sim.engine().events_after(after, 500).unwrap();
        let Some(last) = page.last() else {
            return out;
        };
        after = last.seq;
        out.extend(
            page.into_iter()
                .filter(|e| e.kind.starts_with("scm.user_link."))
                .map(|e| e.kind),
        );
    }
}

fn settle(sim: &mut Sim, steps: usize) {
    for _ in 0..steps {
        if matches!(sim.step(), Step::Idle) {
            return;
        }
    }
}

fn run_for(sim: &mut Sim, ms: i64) {
    let until = sim.clock.now().plus_ms(ms);
    while sim.clock.now() < until {
        sim.step();
    }
}

fn linked(sim: &mut Sim) -> UserLink {
    let started = start(sim);
    sim.world.accounts().answer(DeviceAnswer::Approve);
    assert!(sim.run_until(200, |s| {
        authorization(s, started.authorization.id)
            .authorization
            .state
            == AuthorizationState::Completed
    }));
    link(sim).unwrap()
}

fn stored_token(sim: &Sim, kind: UserTokenKind) -> String {
    let link = link(sim).unwrap();
    let row = sim
        .engine()
        .store()
        .read(|c| saka_store::user_link::user_link_by_id(c, link.id))
        .unwrap()
        .unwrap();
    let columns = match kind {
        UserTokenKind::Access => row.access,
        UserTokenKind::Refresh => row.refresh,
    };
    let sealed = Sealed {
        nonce: columns.nonce,
        ciphertext: columns.ciphertext,
    };
    sim.world
        .open(
            &SecretScope::UserLinkToken {
                link_id: link.id,
                kind,
            },
            &sealed,
        )
        .unwrap()
        .expose()
        .to_string()
}

#[test]
fn the_device_flow_polls_once_per_interval_until_the_account_is_linked() {
    let mut sim = Sim::new(SimConfig::default());
    let started = start(&sim);
    let a = &started.authorization;
    assert_eq!(a.state, AuthorizationState::Pending);
    assert_eq!(a.interval_secs, 5);
    assert_eq!(a.user_code, "FAKE-0001");
    assert_eq!(
        a.expires_at,
        FakeClock::START.plus_ms(900_000),
        "now + expires_in"
    );

    run_for(&mut sim, 60_000);
    let polls = sim.world.accounts().calls_of("device_poll");
    assert!(
        (6..=13).contains(&polls.len()),
        "about one poll per 5 s: {}",
        polls.len()
    );
    for pair in polls.windows(2) {
        assert!(
            pair[1].at.since(pair[0].at) >= 5_000,
            "never faster than interval_secs: {:?}",
            pair
        );
    }
    let code = sim.world.accounts().device_codes()[0].clone();
    assert!(polls.iter().all(|c| c.credential == code));

    sim.world.accounts().answer(DeviceAnswer::Approve);
    assert!(sim.run_until(100, |s| link(s).is_some()));
    let l = link(&sim).unwrap();
    assert_eq!(l.state, UserLinkState::Active);
    assert_eq!((l.provider_user_id, l.login.as_str()), (1, "octo"));
    assert_eq!(l.display_name.as_deref(), Some("Octo Cat"));
    assert_eq!(l.email, "1+octo@users.noreply.github.com");
    assert_eq!(
        l.access_expires_at.since(l.linked_at),
        i64::try_from(ACCESS_TOKEN_SECS).unwrap() * 1000
    );
    let done = authorization(&sim, a.id);
    assert_eq!(done.authorization.state, AuthorizationState::Completed);
    assert_eq!(done.authorization.link_id, Some(l.id));
    assert_eq!(done.link.as_ref().map(|l| l.login.as_str()), Some("octo"));
    assert_eq!(
        kinds(&sim),
        [
            "scm.user_link.authorization_started",
            "scm.user_link.linked",
            "scm.user_link.authorization_ended",
        ]
    );
    let linked = &sim.events_of("scm.user_link.linked")[0];
    assert_eq!(linked.stream_type, "user_link");
    assert_eq!(
        linked.payload,
        json!({
            "link_id": l.id, "integration_id": "github", "authorization_id": a.id,
            "provider_user_id": 1, "login": "octo", "replaced_login": null,
        })
    );
    assert_eq!(
        sim.events_of("scm.user_link.authorization_started")[0].stream_type,
        "user_link_authorization"
    );
    let calls = sim.world.accounts().calls_of("user");
    assert_eq!(calls.len(), 1);
    assert!(calls[0].credential.starts_with("ghu_"));
    assert_eq!(
        stored_token(&sim, UserTokenKind::Access),
        calls[0].credential
    );

    let polls_before = sim.world.accounts().calls_of("device_poll").len();
    run_for(&mut sim, 30_000);
    assert_eq!(
        sim.world.accounts().calls_of("device_poll").len(),
        polls_before,
        "a completed authorization is not polled"
    );
}

#[test]
fn slow_down_raises_the_interval_and_the_polls_follow_it() {
    let mut sim = Sim::new(SimConfig::default());
    let started = start(&sim);
    run_for(&mut sim, 6_000);
    sim.world.accounts().answer(DeviceAnswer::SlowDown(None));
    assert!(sim.run_until(100, |s| {
        authorization(s, started.authorization.id)
            .authorization
            .interval_secs
            == 10
    }));
    sim.world
        .accounts()
        .answer(DeviceAnswer::SlowDown(Some(20)));
    assert!(sim.run_until(400, |s| {
        authorization(s, started.authorization.id)
            .authorization
            .interval_secs
            == 20
    }));
    let slowed = sim.events_of("scm.user_link.authorization_slowed");
    assert_eq!(
        slowed
            .iter()
            .map(|e| e.payload["interval_secs"].clone())
            .collect::<Vec<_>>(),
        [json!(10), json!(20)]
    );
    let from = sim.clock.now();
    run_for(&mut sim, 120_000);
    let late: Vec<_> = sim
        .world
        .accounts()
        .calls_of("device_poll")
        .into_iter()
        .filter(|c| c.at >= from)
        .collect();
    assert!((4..=7).contains(&late.len()), "{}", late.len());
    for pair in late.windows(2) {
        assert!(pair[1].at.since(pair[0].at) >= 20_000, "{pair:?}");
    }
}

#[test]
fn a_second_start_returns_the_pending_authorization_until_its_last_minute() {
    let mut sim = Sim::new(SimConfig::default());
    let first = start(&sim);
    let again = start(&sim);
    assert_eq!(again.authorization.id, first.authorization.id);
    assert_eq!(sim.world.accounts().calls_of("device_code").len(), 1);

    sim.clock
        .set(first.authorization.expires_at.plus_ms(-60_000));
    let fresh = start(&sim);
    assert_ne!(fresh.authorization.id, first.authorization.id);
    assert_eq!(
        authorization(&sim, first.authorization.id)
            .authorization
            .state,
        AuthorizationState::Expired
    );
    let ended = sim.events_of("scm.user_link.authorization_ended");
    assert_eq!(ended.len(), 1);
    assert_eq!(
        ended[0].payload,
        json!({ "authorization_id": first.authorization.id, "state": "expired", "failure": null })
    );
    settle(&mut sim, 20);
}

#[test]
fn an_authorization_past_its_expiry_is_closed_by_the_reconciler() {
    let mut sim = Sim::new(SimConfig::default());
    sim.world.accounts().set_device_timing(5, 120);
    let started = start(&sim);
    run_for(&mut sim, 125_000);
    let a = authorization(&sim, started.authorization.id).authorization;
    assert_eq!(a.state, AuthorizationState::Expired);
    assert!(link(&sim).is_none());
    let device = sim
        .engine()
        .store()
        .read(|c| saka_store::user_link::authorization(c, a.id))
        .unwrap()
        .unwrap()
        .device;
    assert_eq!(device, None, "the device code is gone once it ends");
    let after = sim.world.accounts().calls_of("device_poll").len();
    run_for(&mut sim, 30_000);
    assert_eq!(sim.world.accounts().calls_of("device_poll").len(), after);
}

#[test]
fn denied_failed_and_non_expiring_answers_end_the_authorization() {
    let cases = [
        (DeviceAnswer::Deny, AuthorizationState::Denied, None),
        (DeviceAnswer::Expire, AuthorizationState::Expired, None),
        (
            DeviceAnswer::Fail(AuthorizationFailure::DeviceFlowDisabled),
            AuthorizationState::Failed,
            Some("device_flow_disabled"),
        ),
    ];
    for (answer, state, code) in cases {
        let mut sim = Sim::new(SimConfig::default());
        let started = start(&sim);
        sim.world.accounts().answer(answer);
        assert!(sim.run_until(100, |s| {
            authorization(s, started.authorization.id)
                .authorization
                .state
                == state
        }));
        let a = authorization(&sim, started.authorization.id).authorization;
        assert_eq!(a.failure.map(|f| f.code), code.map(String::from));
        assert!(link(&sim).is_none());
    }

    let mut sim = Sim::new(SimConfig::default());
    sim.world.accounts().set_token_lifetimes(None, None);
    let started = start(&sim);
    sim.world.accounts().answer(DeviceAnswer::Approve);
    assert!(sim.run_until(100, |s| {
        authorization(s, started.authorization.id)
            .authorization
            .state
            == AuthorizationState::Failed
    }));
    let a = authorization(&sim, started.authorization.id).authorization;
    assert_eq!(a.failure.unwrap().code, "token_not_expiring");
    assert!(link(&sim).is_none());
    assert!(sim.world.accounts().calls_of("user").is_empty());

    let mut sim = Sim::new(SimConfig::default());
    sim.world.accounts().reject_identity(true);
    let started = start(&sim);
    sim.world.accounts().answer(DeviceAnswer::Approve);
    assert!(sim.run_until(100, |s| {
        authorization(s, started.authorization.id)
            .authorization
            .state
            == AuthorizationState::Failed
    }));
    let a = authorization(&sim, started.authorization.id).authorization;
    assert_eq!(a.failure.unwrap().code, "identity_unavailable");
}

#[test]
fn a_restart_mid_flow_resumes_polling_from_the_database() {
    let mut sim = Sim::new(SimConfig::default());
    let started = start(&sim);
    run_for(&mut sim, 12_000);
    let before = sim.world.accounts().calls_of("device_poll").len();
    assert!(before >= 1);
    sim.restart();
    sim.world.accounts().answer(DeviceAnswer::Pending);
    sim.world.accounts().answer(DeviceAnswer::Approve);
    assert!(sim.run_until(200, |s| link(s).is_some()));
    assert!(sim.world.accounts().calls_of("device_poll").len() > before);
    assert_eq!(
        authorization(&sim, started.authorization.id)
            .authorization
            .state,
        AuthorizationState::Completed
    );
}

#[test]
fn linking_another_account_keeps_the_link_id_and_names_the_replaced_login() {
    let mut sim = Sim::new(SimConfig::default());
    let first = linked(&mut sim);
    sim.world.accounts().set_user(583231, "monalisa", None);
    let second = linked(&mut sim);
    assert_eq!(second.id, first.id);
    assert_eq!(second.login, "monalisa");
    assert_eq!(second.display_name, None);
    assert_eq!(second.email, "583231+monalisa@users.noreply.github.com");
    let events = sim.events_of("scm.user_link.linked");
    assert_eq!(events[1].payload["replaced_login"], "octo");
}

#[test]
fn unlinking_removes_the_row_once_and_is_quiet_without_a_link() {
    let mut sim = Sim::new(SimConfig::default());
    let l = linked(&mut sim);
    let engine = sim.engine();
    let cause = engine.cause(OPERATOR);
    engine.remove_user_link(GITHUB, &cause, None).unwrap();
    engine.remove_user_link(GITHUB, &cause, None).unwrap();
    assert!(link(&sim).is_none());
    let removed = sim.events_of("scm.user_link.removed");
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].payload, json!({ "link_id": l.id }));
    assert!(matches!(
        engine.user_link(GITHUB, OPERATOR),
        Err(EngineError::Rejected(Rejection::UserLinkNotFound))
    ));
    assert!(matches!(
        engine.user_link("gitlab", OPERATOR),
        Err(EngineError::Rejected(Rejection::NotFound))
    ));
}

fn refresh(sim: &Sim, reason: RefreshReason) -> LinkCredential {
    let engine = sim.engine();
    let id = link(sim).unwrap().id;
    sim.block_on(engine.refresh_user_link(id, reason)).unwrap()
}

#[test]
fn a_refresh_inside_the_last_five_minutes_rotates_the_pair_and_commits_it_first() {
    let mut sim = Sim::new(SimConfig::default());
    let l = linked(&mut sim);
    let first_access = stored_token(&sim, UserTokenKind::Access);
    let first_refresh = stored_token(&sim, UserTokenKind::Refresh);

    sim.clock.set(l.access_expires_at.plus_ms(-300_001));
    assert_eq!(
        refresh(&sim, RefreshReason::Use),
        LinkCredential::Ready(saka_core::secret::SecretText::new(first_access.clone()))
    );
    assert!(sim.world.accounts().calls_of("refresh").is_empty());

    sim.clock.set(l.access_expires_at.plus_ms(-240_000));
    let LinkCredential::Ready(token) = refresh(&sim, RefreshReason::Use) else {
        panic!("a refreshable link gives a token");
    };
    let calls = sim.world.accounts().calls_of("refresh");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].credential, first_refresh);
    assert_ne!(token.expose(), first_access);
    assert_eq!(stored_token(&sim, UserTokenKind::Access), token.expose());
    assert_ne!(stored_token(&sim, UserTokenKind::Refresh), first_refresh);
    let after = link(&sim).unwrap();
    assert_eq!(after.refreshed_at, Some(sim.clock.now()));
    assert!(after.access_fresh(sim.clock.now()));
    let refreshed = sim.events_of("scm.user_link.refreshed");
    assert_eq!(
        refreshed[0].payload,
        json!({
            "link_id": l.id,
            "access_expires_at": after.access_expires_at,
            "refresh_expires_at": after.refresh_expires_at,
        })
    );
    assert_eq!(
        refresh(&sim, RefreshReason::Use),
        LinkCredential::Ready(token)
    );
    assert_eq!(sim.world.accounts().calls_of("refresh").len(), 1);
}

#[test]
fn two_refreshes_at_once_make_one_call_to_github() {
    let mut sim = Sim::new(SimConfig::default());
    let l = linked(&mut sim);
    sim.clock.set(l.access_expires_at.plus_ms(-60_000));
    let engine = sim.engine();
    let (a, b) = sim.block_on(async {
        tokio::join!(
            engine.refresh_user_link(l.id, RefreshReason::Use),
            engine.refresh_user_link(l.id, RefreshReason::Use)
        )
    });
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(sim.world.accounts().calls_of("refresh").len(), 1);
    assert_eq!(sim.events_of("scm.user_link.refreshed").len(), 1);
}

#[test]
fn a_rejected_or_expired_refresh_token_needs_a_relink() {
    let mut sim = Sim::new(SimConfig::default());
    let l = linked(&mut sim);
    sim.clock.set(l.access_expires_at);
    sim.world.accounts().revoke_refresh_tokens();
    assert_eq!(
        refresh(&sim, RefreshReason::Use),
        LinkCredential::Relink(RelinkCause::RefreshFailed)
    );
    let after = link(&sim).unwrap();
    assert_eq!(after.state, UserLinkState::NeedsRelink);
    assert_eq!(after.relink_cause, Some(RelinkCause::RefreshFailed));
    assert_eq!(
        sim.events_of("scm.user_link.relink_required")[0].payload,
        json!({ "link_id": l.id, "cause": "refresh_failed" })
    );
    assert_eq!(
        refresh(&sim, RefreshReason::Use),
        LinkCredential::Relink(RelinkCause::RefreshFailed)
    );
    assert_eq!(sim.world.accounts().calls_of("refresh").len(), 1);

    let mut sim = Sim::new(SimConfig::default());
    let l = linked(&mut sim);
    sim.clock.set(l.refresh_expires_at);
    assert_eq!(
        refresh(&sim, RefreshReason::Use),
        LinkCredential::Relink(RelinkCause::Expired)
    );
    assert!(sim.world.accounts().calls_of("refresh").is_empty());
    assert_eq!(link(&sim).unwrap().relink_cause, Some(RelinkCause::Expired));

    let mut sim = Sim::new(SimConfig::default());
    let l = linked(&mut sim);
    sim.clock.set(l.access_expires_at);
    sim.world.accounts().fail_refreshes(1);
    let engine = sim.engine();
    let e = sim
        .block_on(engine.refresh_user_link(l.id, RefreshReason::Use))
        .unwrap_err();
    assert!(
        matches!(e, EngineError::Port(ref p) if p.retryable),
        "{e:?}"
    );
    assert_eq!(link(&sim).unwrap().state, UserLinkState::Active);
}

#[test]
fn the_proactive_refresh_runs_once_a_day_in_the_last_week_of_the_refresh_token() {
    let mut sim = Sim::new(SimConfig::default());
    let l = linked(&mut sim);
    run_for(&mut sim, 3 * 3_600_000);
    assert!(
        sim.world.accounts().calls_of("refresh").is_empty(),
        "a refresh token with months left is left alone"
    );
    assert_eq!(l.refresh_expires_at.since(l.linked_at), 184 * DAY_MS);

    let mut sim = Sim::new(SimConfig::default());
    sim.world
        .accounts()
        .set_token_lifetimes(Some(ACCESS_TOKEN_SECS), Some(3 * 86_400));
    linked(&mut sim);
    settle(&mut sim, 50);
    assert_eq!(sim.world.accounts().calls_of("refresh").len(), 1);
    for day in 1..=2 {
        run_for(&mut sim, 6 * 3_600_000);
        assert_eq!(
            sim.world.accounts().calls_of("refresh").len(),
            day,
            "one refresh per day"
        );
        let next_day = (sim.clock.now().0.div_euclid(DAY_MS) + 1) * DAY_MS;
        sim.clock.set(Timestamp(next_day));
        settle(&mut sim, 50);
        assert_eq!(sim.world.accounts().calls_of("refresh").len(), day + 1);
    }
    let refreshed = sim.events_of("scm.user_link.refreshed");
    assert_eq!(refreshed.len(), 3);
    let jobs: i64 = sim
        .engine()
        .store()
        .read(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM job WHERE kind = 'scm.user_link_refresh'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(jobs, 3);
}
