#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use saka_core::authorship::{GitIdentity, UserLink};
use saka_core::model::Task;
use saka_core::rejection::Rejection;
use saka_core::secret::SecretScope;
use saka_core::vocab::{
    AuthorizationState, AuthorshipMode, BlockerCode, BlockerResolution, RefreshReason, Stage,
    StagePhase, UserLinkState, UserTokenKind, WorkItemState,
};
use saka_engine::{AuthorshipView, EngineError, LinkCredential, OPERATOR};
use saka_ports::{Clock, Sealed, SecretStore};
use saka_testkit::{APP_CREDENTIAL, DeviceAnswer, Operator, Sim, SimConfig};
use serde_json::json;

const GITHUB: &str = "github";
const STEPS: usize = 5_000;

fn sim() -> Sim {
    Sim::new(SimConfig {
        operator: Operator {
            start: true,
            retry: vec![],
            ..Operator::default()
        },
        ..SimConfig::default()
    })
}

fn link(sim: &mut Sim) -> UserLink {
    // A relink resumes blockers opened strictly before it; the virtual clock may not have moved.
    sim.clock.advance(1);
    let engine = sim.engine();
    let cause = engine.cause(OPERATOR);
    let started = sim
        .block_on(engine.start_user_link(GITHUB, &cause, None))
        .unwrap();
    sim.world.accounts().answer(DeviceAnswer::Approve);
    let id = started.authorization.id;
    assert!(sim.run_until(500, |s| {
        s.engine()
            .user_link_authorization(GITHUB, OPERATOR, id)
            .unwrap()
            .authorization
            .state
            == AuthorizationState::Completed
    }));
    sim.engine().user_link(GITHUB, OPERATOR).unwrap()
}

fn set_mode(sim: &Sim, mode: AuthorshipMode) -> Result<AuthorshipView, EngineError> {
    // Blockers resume on a strictly later mode change; the virtual clock may not have moved yet.
    sim.clock.advance(1);
    let engine = sim.engine();
    let cause = engine.cause(OPERATOR);
    engine.set_authorship(sim.project_id, mode, &cause, None)
}

fn access_token(sim: &Sim) -> String {
    let link = sim.engine().user_link(GITHUB, OPERATOR).unwrap();
    let row = sim
        .engine()
        .store()
        .read(|c| saka_store::user_link::user_link_by_id(c, link.id))
        .unwrap()
        .unwrap();
    let sealed = Sealed {
        nonce: row.access.nonce,
        ciphertext: row.access.ciphertext,
    };
    let scope = SecretScope::UserLinkToken {
        link_id: link.id,
        kind: UserTokenKind::Access,
    };
    sim.world
        .open(&scope, &sealed)
        .unwrap()
        .expose()
        .to_string()
}

fn octo() -> GitIdentity {
    GitIdentity {
        name: "Octo Cat".into(),
        email: "1+octo@users.noreply.github.com".into(),
    }
}

fn saka() -> GitIdentity {
    GitIdentity {
        name: "Saka Agent".into(),
        email: "agent@saka.invalid".into(),
    }
}

fn blocked(t: &Task) -> Option<(BlockerCode, serde_json::Value)> {
    (t.item.state == WorkItemState::Blocked)
        .then(|| t.open_blocker().map(|b| (b.code, b.params.clone())))
        .flatten()
}

fn opened(sim: &Sim) -> bool {
    sim.task(1).change_request.is_some()
}

fn credentials_of(sim: &Sim, op: &str) -> Vec<String> {
    sim.world
        .scm_calls()
        .into_iter()
        .filter(|c| c.op == op)
        .map(|c| c.credential)
        .collect()
}

fn dev_runs(sim: &Sim) -> Vec<serde_json::Value> {
    sim.events_of("delivery.stage_run.created")
        .into_iter()
        .map(|e| e.payload)
        .filter(|p| p["stage"] == "dev")
        .collect()
}

fn resolution(t: &Task) -> Option<BlockerResolution> {
    t.blocker.as_ref().and_then(|b| b.resolution)
}

fn auto_retries(sim: &Sim) -> u32 {
    sim.task(1).retries.get(&Stage::Dev).map_or(0, |r| r.auto)
}

#[test]
fn user_mode_commits_as_the_linked_account_and_opens_the_pull_request_with_its_token() {
    let mut sim = sim();
    link(&mut sim);
    let view = set_mode(&sim, AuthorshipMode::User).unwrap();
    assert_eq!(view.mode, AuthorshipMode::User);
    assert_eq!(sim.project().authorship_mode, AuthorshipMode::User);
    sim.approve();
    assert!(sim.run_until(STEPS, opened), "{:?}", sim.task(1).item);

    let launches = sim.world.launches();
    assert!(!launches.is_empty());
    for launch in &launches {
        assert_eq!(launch.author, octo());
    }
    let started = sim.events_of("runtime.session.started");
    assert_eq!(
        started[0].payload["author"],
        json!({ "source": "user_link", "login": "octo" })
    );
    let token = access_token(&sim);
    assert!(token.starts_with("ghu_"));
    assert_eq!(
        credentials_of(&sim, "open_change_request"),
        std::slice::from_ref(&token)
    );
    assert_eq!(sim.world.prs()[0].opened_with, token);
    let opened = sim.events_of("scm.change_request.opened");
    assert_eq!(opened[0].payload["opened_with"], "user_link");

    assert!(sim.run_to_shipped(STEPS));
    let calls = sim.world.scm_calls();
    for op in ["push", "change_request", "merge", "delete_branch"] {
        assert!(calls.iter().any(|c| c.op == op), "{op} was never called");
    }
    for call in calls.iter().filter(|c| c.op != "open_change_request") {
        assert_eq!(call.credential, APP_CREDENTIAL, "{}", call.op);
    }
    let metrics = sim.engine().metrics().unwrap();
    assert!(
        metrics
            .change_requests_opened
            .contains(&("user_link".to_string(), 2)),
        "{:?}",
        metrics.change_requests_opened
    );
}

#[test]
fn bot_mode_commits_as_saka_and_opens_the_pull_request_with_the_app() {
    let mut sim = sim();
    link(&mut sim);
    assert_eq!(sim.project().authorship_mode, AuthorshipMode::Bot);
    sim.approve();
    assert!(sim.run_to_shipped(STEPS));
    for launch in sim.world.launches() {
        assert_eq!(launch.author, saka());
    }
    let started = sim.events_of("runtime.session.started");
    assert_eq!(
        started[0].payload["author"],
        json!({ "source": "saka", "login": null })
    );
    for call in sim.world.scm_calls() {
        assert_eq!(call.credential, APP_CREDENTIAL, "{}", call.op);
    }
    let opened = sim.events_of("scm.change_request.opened");
    assert_eq!(opened.len(), 2);
    assert!(opened.iter().all(|e| e.payload["opened_with"] == "app"));
    assert!(sim.world.accounts().calls_of("refresh").is_empty());
    let metrics = sim.engine().metrics().unwrap();
    assert_eq!(metrics.change_requests_opened, vec![("app".to_string(), 2)]);
}

#[test]
fn a_pull_request_near_the_access_expiry_refreshes_once_and_uses_the_new_token() {
    let mut sim = sim();
    sim.world
        .accounts()
        .set_token_lifetimes(Some(4 * 60), Some(30 * 86_400));
    let first = link(&mut sim);
    let before = access_token(&sim);
    set_mode(&sim, AuthorshipMode::User).unwrap();
    sim.approve();
    assert!(sim.run_until(STEPS, opened));
    let refreshes = sim.world.accounts().calls_of("refresh");
    assert_eq!(refreshes.len(), 1);
    let after = access_token(&sim);
    assert_ne!(after, before);
    assert_eq!(credentials_of(&sim, "open_change_request"), [after]);
    let link = sim.engine().user_link(GITHUB, OPERATOR).unwrap();
    assert!(link.refreshed_at.is_some());
    assert!(link.access_expires_at > first.access_expires_at);
}

#[test]
fn a_link_that_needs_relinking_blocks_the_session_before_launch_and_a_relink_resumes_it() {
    let mut sim = sim();
    let first = link(&mut sim);
    set_mode(&sim, AuthorshipMode::User).unwrap();
    sim.world.accounts().revoke_refresh_tokens();
    sim.clock.set(first.access_expires_at.plus_ms(-60_000));
    let engine = sim.engine();
    let relinked = sim.block_on(engine.refresh_user_link(first.id, RefreshReason::Use));
    assert!(matches!(relinked, Ok(LinkCredential::Relink(_))));
    assert_eq!(
        sim.engine().user_link(GITHUB, OPERATOR).unwrap().state,
        UserLinkState::NeedsRelink
    );
    sim.approve();
    assert!(sim.run_until(STEPS, |s| blocked(&s.task(1)).is_some()));
    let (code, params) = blocked(&sim.task(1)).unwrap();
    assert_eq!(code, BlockerCode::UserLinkRequired);
    assert_eq!(
        params,
        json!({ "integration_id": "github", "cause": "refresh_failed", "phase": "running_session" })
    );
    assert!(
        sim.world.launches().is_empty(),
        "no session without an author"
    );
    for _ in 0..200 {
        sim.step();
    }
    assert!(
        blocked(&sim.task(1)).is_some(),
        "nothing resumes it by itself"
    );

    link(&mut sim);
    assert!(sim.run_until(STEPS, opened));
    let t = sim.task(1);
    assert_eq!(resolution(&t), Some(BlockerResolution::Auto));
    let runs = dev_runs(&sim);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1]["trigger"], "auto");
    assert_eq!(runs[1]["reason"], "credential renewed");
    assert_eq!(runs[1]["attempt"], 2);
    assert_eq!(auto_retries(&sim), 0);
    assert_eq!(sim.world.launches()[0].author, octo());
}

#[test]
fn a_revoked_token_blocks_the_pull_request_and_a_relink_opens_it_with_the_new_token() {
    let mut sim = sim();
    link(&mut sim);
    set_mode(&sim, AuthorshipMode::User).unwrap();
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).is_at(Stage::Dev, StagePhase::Pushing)));
    let revoked = access_token(&sim);
    sim.world.accounts().revoke_authorization();
    assert!(sim.run_until(STEPS, |s| blocked(&s.task(1)).is_some()));
    let (code, params) = blocked(&sim.task(1)).unwrap();
    assert_eq!(code, BlockerCode::UserLinkRequired);
    assert_eq!(
        params,
        json!({ "integration_id": "github", "cause": "revoked", "phase": "opening_change" })
    );
    let relink = sim.events_of("scm.user_link.relink_required");
    assert_eq!(relink.len(), 1);
    assert_eq!(relink[0].payload["cause"], "revoked");
    assert_eq!(
        sim.engine().user_link(GITHUB, OPERATOR).unwrap().state,
        UserLinkState::NeedsRelink
    );
    assert!(sim.world.prs().is_empty());
    let budget = sim.task(1).retries.clone();

    link(&mut sim);
    let renewed = access_token(&sim);
    assert_ne!(renewed, revoked);
    assert!(sim.run_until(STEPS, opened));
    assert_eq!(
        credentials_of(&sim, "open_change_request"),
        [revoked, renewed.clone()]
    );
    assert_eq!(sim.world.prs()[0].opened_with, renewed);
    let t = sim.task(1);
    assert_eq!(resolution(&t), Some(BlockerResolution::Auto));
    let resumed = dev_runs(&sim).pop().unwrap();
    assert_eq!(resumed["entry_phase"], "opening_change");
    assert_eq!(resumed["trigger"], "auto");
    assert_eq!(resumed["reason"], "credential renewed");
    assert_eq!(
        t.retries.get(&Stage::Dev),
        budget.get(&Stage::Dev),
        "the resume spends no automatic retry"
    );
    assert_eq!(auto_retries(&sim), 0);
    assert_eq!(sim.world.launches().len(), 1, "no new session for the PR");
    assert!(sim.run_to_shipped(STEPS));
}

#[test]
fn a_denied_pull_request_waits_for_the_operator_and_a_retry_opens_it() {
    let mut sim = sim();
    link(&mut sim);
    set_mode(&sim, AuthorshipMode::User).unwrap();
    sim.world.accounts().deny_user_pulls(Some(403));
    sim.approve();
    assert!(sim.run_until(STEPS, |s| blocked(&s.task(1)).is_some()));
    let (code, params) = blocked(&sim.task(1)).unwrap();
    assert_eq!(code, BlockerCode::UserAccessDenied);
    assert_eq!(
        params,
        json!({ "credential": "user_link", "login": "octo", "status": 403, "phase": "opening_change" })
    );
    let until = sim.clock.now().plus_ms(30 * 60_000);
    while sim.clock.now() < until {
        sim.step();
    }
    assert!(blocked(&sim.task(1)).is_some(), "no automatic resume");
    assert_eq!(auto_retries(&sim), 0);

    sim.world.accounts().deny_user_pulls(None);
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry(sim.task(1).item.id, "access granted", &cause)
        .unwrap();
    assert!(sim.run_until(STEPS, opened));
    assert_eq!(
        sim.events_of("scm.change_request.opened")[0].payload["opened_with"],
        "user_link"
    );
}

#[test]
fn switching_to_bot_mode_resumes_a_blocked_task_and_opens_with_the_app() {
    let mut sim = sim();
    link(&mut sim);
    set_mode(&sim, AuthorshipMode::User).unwrap();
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).is_at(Stage::Dev, StagePhase::Pushing)));
    sim.world.accounts().revoke_authorization();
    assert!(sim.run_until(STEPS, |s| blocked(&s.task(1)).is_some()));
    assert_eq!(
        blocked(&sim.task(1)).unwrap().0,
        BlockerCode::UserLinkRequired
    );

    let engine = sim.engine();
    let cause = engine.cause(OPERATOR);
    let refused = engine.remove_user_link(GITHUB, &cause, None);
    assert!(matches!(
        refused,
        Err(EngineError::Rejected(Rejection::UserLinkInUse { .. }))
    ));
    set_mode(&sim, AuthorshipMode::Bot).unwrap();
    engine.remove_user_link(GITHUB, &cause, None).unwrap();
    assert!(sim.run_until(STEPS, opened));
    let t = sim.task(1);
    assert_eq!(resolution(&t), Some(BlockerResolution::Auto));
    assert_eq!(
        dev_runs(&sim).pop().unwrap()["reason"],
        "authorship mode changed"
    );
    assert_eq!(
        credentials_of(&sim, "open_change_request").last().unwrap(),
        APP_CREDENTIAL
    );
    assert_eq!(
        sim.events_of("scm.change_request.opened")[0].payload["opened_with"],
        "app"
    );
}

#[test]
fn the_mode_is_set_only_with_a_usable_link_and_once_per_change() {
    let mut sim = sim();
    match set_mode(&sim, AuthorshipMode::User) {
        Err(EngineError::Rejected(Rejection::UserLinkMissing { integrations })) => assert_eq!(
            integrations,
            json!([{ "integration_id": "github", "state": "missing" }])
        ),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        set_mode(&sim, AuthorshipMode::UserPat),
        Err(EngineError::Rejected(Rejection::UserTokenMissing))
    ));
    set_mode(&sim, AuthorshipMode::Bot).unwrap();
    assert!(sim.events_of("workspace.project.authorship_set").is_empty());
    link(&mut sim);
    set_mode(&sim, AuthorshipMode::User).unwrap();
    set_mode(&sim, AuthorshipMode::User).unwrap();
    let set = sim.events_of("workspace.project.authorship_set");
    assert_eq!(set.len(), 1);
    assert_eq!(
        set[0].payload,
        json!({ "project_id": sim.project_id, "mode": "user" })
    );

    let mut caps = saka_testkit::FAKE_CAPABILITIES;
    caps.user_authorship = false;
    sim.world.set_capabilities(caps);
    set_mode(&sim, AuthorshipMode::Bot).unwrap();
    assert!(matches!(
        set_mode(&sim, AuthorshipMode::User),
        Err(EngineError::Rejected(
            Rejection::UserAuthorshipUnsupported { .. }
        ))
    ));
}
