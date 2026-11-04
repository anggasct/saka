#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use saka_core::authorship::GitIdentity;
use saka_core::model::Task;
use saka_core::rejection::Rejection;
use saka_core::secret::SecretText;
use saka_core::time::Timestamp;
use saka_core::vocab::{
    AuthorshipMode, BlockerCode, BlockerResolution, Stage, StagePhase, TokenInvalidCause,
    WorkItemState,
};
use saka_engine::{AuthorshipView, EngineError, OPERATOR};
use saka_ports::Clock;
use saka_testkit::{APP_CREDENTIAL, Operator, Sim, SimConfig};
use serde_json::json;

const STEPS: usize = 5_000;
const HOUR_MS: i64 = 3_600_000;

fn sim() -> Sim {
    let sim = Sim::new(SimConfig {
        operator: Operator {
            retry: vec![],
            ..Operator::default()
        },
        ..SimConfig::default()
    });
    sim.world.accounts().set_user(7, "ann", Some("Ann Lee"));
    sim
}

fn store_token(sim: &Sim, token: &str) -> Result<AuthorshipView, EngineError> {
    // A new token resumes blockers opened strictly before it; the virtual clock may not have moved.
    sim.clock.advance(1);
    let engine = sim.engine();
    let cause = engine.cause(OPERATOR);
    let value = SecretText::new(token.to_string());
    sim.block_on(engine.set_user_token(sim.project_id, &value, &cause, None))
}

fn set_mode(sim: &Sim, mode: AuthorshipMode) -> Result<AuthorshipView, EngineError> {
    sim.clock.advance(1);
    let engine = sim.engine();
    let cause = engine.cause(OPERATOR);
    engine.set_authorship(sim.project_id, mode, &cause, None)
}

fn ann() -> GitIdentity {
    GitIdentity {
        name: "Ann Lee".into(),
        email: "7+ann@users.noreply.github.com".into(),
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

fn last_dev_run(sim: &Sim) -> serde_json::Value {
    sim.events_of("delivery.stage_run.created")
        .into_iter()
        .map(|e| e.payload)
        .rfind(|p| p["stage"] == "dev")
        .unwrap()
}

fn auto_retries(sim: &Sim) -> u32 {
    sim.task(1).retries.get(&Stage::Dev).map_or(0, |r| r.auto)
}

#[test]
fn user_pat_mode_commits_as_the_token_owner_and_opens_the_pull_request_with_the_token() {
    let mut sim = sim();
    let pat = sim.world.accounts().issue_pat(None);
    let stored = store_token(&sim, &pat).unwrap();
    let token = stored.user_token.unwrap();
    assert_eq!(token.token.identity.login, "ann");
    assert_eq!(token.token.expires_at, None);
    assert_eq!(token.problem, None);
    let view = set_mode(&sim, AuthorshipMode::UserPat).unwrap();
    let author = view.author.unwrap();
    assert_eq!(author.identity, ann());
    assert_eq!(author.login.as_deref(), Some("ann"));
    sim.approve();
    assert!(sim.run_until(STEPS, opened), "{:?}", sim.task(1).item);

    let launches = sim.world.launches();
    assert!(!launches.is_empty());
    for launch in &launches {
        assert_eq!(launch.author, ann());
        let env = format!("{:?}{:?}", launch.vars, launch.secrets);
        assert!(!env.contains(&pat), "the token never reaches a session");
        assert!(!env.contains("github_pat_"));
    }
    let started = sim.events_of("runtime.session.started");
    assert_eq!(
        started[0].payload["author"],
        json!({ "source": "user_token", "login": "ann" })
    );
    assert_eq!(
        credentials_of(&sim, "open_change_request"),
        std::slice::from_ref(&pat)
    );
    assert_eq!(
        sim.events_of("scm.change_request.opened")[0].payload["opened_with"],
        "user_token"
    );

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
            .iter()
            .any(|(credential, n)| credential == "user_token" && *n >= 1),
        "{:?}",
        metrics.change_requests_opened
    );
}

#[test]
fn an_expired_token_blocks_the_session_before_launch_and_a_new_token_resumes_it() {
    let mut sim = sim();
    let expires = Timestamp(sim.clock.now().0 + HOUR_MS);
    let first = sim.world.accounts().issue_pat(Some(expires));
    let stored = store_token(&sim, &first).unwrap();
    assert_eq!(stored.user_token.unwrap().token.expires_at, Some(expires));
    set_mode(&sim, AuthorshipMode::UserPat).unwrap();
    sim.clock.set(expires);
    let view = sim.engine().project_authorship(sim.project_id).unwrap();
    assert_eq!(
        view.user_token.unwrap().problem,
        Some(TokenInvalidCause::Expired)
    );
    assert_eq!(view.author, None);
    sim.approve();
    assert!(sim.run_until(STEPS, |s| blocked(&s.task(1)).is_some()));
    let (code, params) = blocked(&sim.task(1)).unwrap();
    assert_eq!(code, BlockerCode::UserTokenInvalid);
    assert_eq!(
        params,
        json!({ "cause": "expired", "phase": "running_session" })
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

    let second = sim.world.accounts().issue_pat(None);
    store_token(&sim, &second).unwrap();
    assert!(sim.run_until(STEPS, opened));
    let t = sim.task(1);
    assert_eq!(
        t.blocker.as_ref().and_then(|b| b.resolution),
        Some(BlockerResolution::Auto)
    );
    let resumed = last_dev_run(&sim);
    assert_eq!(resumed["trigger"], "auto");
    assert_eq!(resumed["reason"], "credential renewed");
    assert_eq!(
        auto_retries(&sim),
        0,
        "the resume spends no automatic retry"
    );
    assert_eq!(sim.world.launches()[0].author, ann());
    assert_eq!(credentials_of(&sim, "open_change_request"), [second]);
}

#[test]
fn a_refused_token_blocks_the_pull_request_marks_it_invalid_and_a_new_token_opens_it() {
    let mut sim = sim();
    let first = sim.world.accounts().issue_pat(None);
    store_token(&sim, &first).unwrap();
    set_mode(&sim, AuthorshipMode::UserPat).unwrap();
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).is_at(Stage::Dev, StagePhase::Pushing)));
    sim.world.accounts().revoke_pat(&first);
    assert!(sim.run_until(STEPS, |s| blocked(&s.task(1)).is_some()));
    let (code, params) = blocked(&sim.task(1)).unwrap();
    assert_eq!(code, BlockerCode::UserTokenInvalid);
    assert_eq!(
        params,
        json!({ "cause": "rejected", "phase": "opening_change" })
    );
    let invalidated = sim.events_of("workspace.project.user_token_invalidated");
    assert_eq!(invalidated.len(), 1);
    assert_eq!(
        invalidated[0].payload,
        json!({ "project_id": sim.project_id, "cause": "rejected" })
    );
    let view = sim.engine().project_authorship(sim.project_id).unwrap();
    assert_eq!(
        view.user_token.unwrap().problem,
        Some(TokenInvalidCause::Rejected)
    );
    assert!(matches!(
        set_mode(&sim, AuthorshipMode::UserPat),
        Err(EngineError::Rejected(Rejection::UserTokenInvalid {
            cause: TokenInvalidCause::Rejected
        }))
    ));
    assert!(sim.world.prs().is_empty());
    let budget = sim.task(1).retries.clone();

    let second = sim.world.accounts().issue_pat(None);
    store_token(&sim, &second).unwrap();
    assert!(sim.run_until(STEPS, opened));
    assert_eq!(
        credentials_of(&sim, "open_change_request"),
        [first, second.clone()]
    );
    assert_eq!(sim.world.prs()[0].opened_with, second);
    let t = sim.task(1);
    assert_eq!(
        t.blocker.as_ref().and_then(|b| b.resolution),
        Some(BlockerResolution::Auto)
    );
    let resumed = last_dev_run(&sim);
    assert_eq!(resumed["entry_phase"], "opening_change");
    assert_eq!(resumed["reason"], "credential renewed");
    assert_eq!(t.retries.get(&Stage::Dev), budget.get(&Stage::Dev));
    assert_eq!(auto_retries(&sim), 0);
    assert_eq!(sim.world.launches().len(), 1, "no new session for the PR");
    assert_eq!(
        sim.events_of("scm.change_request.opened")[0].payload["opened_with"],
        "user_token"
    );
}

#[test]
fn a_token_the_pull_request_cannot_use_waits_for_the_operator() {
    let mut sim = sim();
    let pat = sim.world.accounts().issue_pat(None);
    store_token(&sim, &pat).unwrap();
    set_mode(&sim, AuthorshipMode::UserPat).unwrap();
    sim.world.accounts().deny_user_pulls(Some(403));
    sim.approve();
    assert!(sim.run_until(STEPS, |s| blocked(&s.task(1)).is_some()));
    let (code, params) = blocked(&sim.task(1)).unwrap();
    assert_eq!(code, BlockerCode::UserAccessDenied);
    assert_eq!(
        params,
        json!({ "credential": "user_token", "login": "ann", "status": 403, "phase": "opening_change" })
    );
    assert!(
        sim.events_of("workspace.project.user_token_invalidated")
            .is_empty()
    );
}

#[test]
fn switching_to_bot_mode_resumes_a_token_blocker_and_the_token_can_then_be_removed() {
    let mut sim = sim();
    let pat = sim.world.accounts().issue_pat(None);
    store_token(&sim, &pat).unwrap();
    set_mode(&sim, AuthorshipMode::UserPat).unwrap();
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).is_at(Stage::Dev, StagePhase::Pushing)));
    sim.world.accounts().revoke_pat(&pat);
    assert!(sim.run_until(STEPS, |s| blocked(&s.task(1)).is_some()));

    let engine = sim.engine();
    let cause = engine.cause(OPERATOR);
    assert!(matches!(
        engine.remove_user_token(sim.project_id, &cause, None),
        Err(EngineError::Rejected(Rejection::UserTokenInUse))
    ));
    set_mode(&sim, AuthorshipMode::Bot).unwrap();
    engine
        .remove_user_token(sim.project_id, &cause, None)
        .unwrap();
    engine
        .remove_user_token(sim.project_id, &cause, None)
        .unwrap();
    assert_eq!(
        sim.events_of("workspace.project.user_token_removed").len(),
        1
    );
    assert!(sim.run_until(STEPS, opened));
    assert_eq!(last_dev_run(&sim)["reason"], "authorship mode changed");
    assert_eq!(
        credentials_of(&sim, "open_change_request").last().unwrap(),
        APP_CREDENTIAL
    );
    assert!(matches!(
        set_mode(&sim, AuthorshipMode::UserPat),
        Err(EngineError::Rejected(Rejection::UserTokenMissing))
    ));
}
