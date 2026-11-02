#![allow(clippy::unwrap_used, reason = "tests fail loudly on a broken setup")]

use proptest::prelude::*;
use saka_core::admission::Waiting;
use saka_core::ids::SpecDraftId;
use saka_core::model::Draft;
use saka_core::vocab::{
    EnvironmentState, LintOutcome, PrimaryAction, SessionState, SpecDraftState, SpecOrigin,
    SpecState, WorkItemState,
};
use saka_engine::SessionCommand;
use saka_ports::{DocWrite, LaunchProfile};
use saka_store::{StoredEvent, read};
use saka_testkit::{
    AgentOutcome, AgentScript, DraftTurn, Operator, Sim, SimConfig, TurnUsage, one_step_spec,
};
use serde_json::{Value, json};

const STEPS: usize = 4_000;
const PATH: &str = "docs/specs/export.md";
const IDEA: &str = "Let people export their task list as a CSV file.";

fn bad() -> Vec<u8> {
    b"---
saka: spec
---
# Export tasks

## Acceptance criteria

- **AC-01**: `tasks export` writes a CSV file.

## Steps

| Step | Work | AC | Done when |
|------|------|----|-----------|
| S1 | Add the export command | AC-02 | `make test` |
"
    .to_vec()
}

fn good(open_decision: bool) -> Vec<u8> {
    let decision = if open_decision {
        "
## Decisions

### Q1: Which delimiter does the file use?

- [ ] **Comma** (recommended) — the CSV default.
- [ ] **Semicolon** — common where the comma is the decimal mark.
"
    } else {
        ""
    };
    format!(
        "---
saka: spec
---
# Export tasks

## Acceptance criteria

- **AC-01**: `tasks export` writes a CSV file.

## Done when

- `make test`
{decision}"
    )
    .into_bytes()
}

fn write(bytes: Vec<u8>) -> DraftTurn {
    DraftTurn::Write {
        path: PATH.into(),
        bytes,
    }
}

fn sim(tune: impl FnOnce(&mut SimConfig)) -> Sim {
    let mut cfg = SimConfig {
        operator: Operator {
            merge: true,
            retry: vec![],
            answer: None,
        },
        ..SimConfig::default()
    };
    tune(&mut cfg);
    Sim::new(cfg)
}

fn drafted(sim: &Sim, turns: Vec<DraftTurn>) -> SpecDraftId {
    sim.world.script_draft(turns);
    sim.create_draft(PATH, IDEA, &[]).unwrap().draft.id
}

fn run_to(sim: &mut Sim, id: SpecDraftId, done: impl Fn(&Draft) -> bool) -> Draft {
    assert!(
        sim.run_until(STEPS, |s| done(&s.draft(id))),
        "draft never reached the state: {:?}",
        sim.draft(id).draft
    );
    sim.draft(id)
}

fn finished(sim: &mut Sim, id: SpecDraftId) -> Draft {
    let d = run_to(sim, id, |d| d.draft.state.is_finished());
    assert!(
        sim.run_until(STEPS, |s| s.world.running_sessions().is_empty()),
        "runner left behind"
    );
    d
}

fn failure(d: &Draft) -> (String, Value) {
    let f = d.draft.failure.clone().unwrap();
    (f.code, f.params)
}

fn events(sim: &Sim, kind: &str) -> Vec<StoredEvent> {
    sim.engine()
        .store()
        .read(|c| read::events_after(c, 0, 100_000))
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

fn base_commits(sim: &Sim) -> usize {
    events(sim, "workspace.doc.committed").len()
}

#[test]
fn a_draft_fixed_after_one_lint_round_commits_once_as_an_architect_spec() {
    let mut sim = sim(|_| {});
    let fixed = good(true);
    let id = drafted(&sim, vec![write(bad()), write(fixed.clone())]);
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded, "{:?}", d.draft);

    let rounds = sim.draft_rounds(id);
    let outcomes: Vec<LintOutcome> = rounds.iter().map(|(_, r)| r.outcome).collect();
    assert_eq!(outcomes, [LintOutcome::Failed, LintOutcome::Passed]);
    let codes: Vec<&str> = rounds[0]
        .1
        .issues
        .iter()
        .filter_map(|i| i["code"].as_str())
        .collect();
    assert!(codes.contains(&"step_ac_unknown"), "{codes:?}");
    assert_eq!(rounds[1].1.decisions_open, 1);

    let prompts = sim.world.prompts();
    assert!(prompts[0].contains("You are the architect for project \"tasks\""));
    assert!(prompts[0].contains(&format!("Target file: {PATH}")));
    assert!(prompts[0].contains(IDEA));
    assert!(
        prompts[1].starts_with(&format!(
            "Saka checked {PATH} after your last turn (check 1 of 4). It does not pass yet:\n- line "
        )),
        "{}",
        prompts[1]
    );
    assert!(prompts[1].contains("step_ac_unknown"));

    let commits = sim.world.doc_commits();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].path, PATH);
    assert_eq!(commits[0].bytes, fixed);
    assert_eq!(commits[0].expect, saka_ports::DocExpect::Absent);
    assert_eq!(sim.world.doc(PATH).unwrap(), fixed);
    assert_eq!(base_commits(&sim), 1);
    let committed = &events(&sim, "workspace.doc.committed")[0];
    assert_eq!(committed.actor, "system:architect");
    assert_eq!(committed.payload["created"], json!(true));

    let spec = sim.spec_of(d.draft.spec_id.unwrap());
    assert_eq!(spec.origin, SpecOrigin::Architect);
    assert_eq!(spec.state, SpecState::Draft);
    let revision = sim
        .engine()
        .store()
        .read(|c| read::load_revision(c, spec.latest_revision_id))
        .unwrap()
        .unwrap();
    assert_eq!(Some(revision.content_hash), rounds[1].1.content_hash);
    assert_eq!(
        d.draft.commit_sha.as_deref(),
        Some(sim.world.base_head().as_str())
    );

    let sessions = sim.draft_sessions(id);
    assert_eq!(sessions.len(), 1);
    let ended = sessions[0].end_reason.clone().unwrap();
    assert_eq!(
        (ended.code.as_str(), ended.params),
        ("lint_passed", json!({ "round": 2 }))
    );
    assert_eq!(sessions[0].state, SessionState::Ended);
    let d = run_to(&mut sim, id, |d| {
        d.environment
            .as_ref()
            .is_some_and(|e| e.state == EnvironmentState::TornDown)
    });
    assert!(!sim.world.worktree_live(d.environment.unwrap().id));

    let launch = sim.world.launches().pop().unwrap();
    assert_eq!(
        launch.profile,
        LaunchProfile::Architect { cwd: "docs".into() }
    );
    assert!(!launch.vars.contains_key("SAKA_TASK_KEY"));
    assert!(!launch.vars.keys().any(|k| k.starts_with("SAKA_PORT")));
    assert!(launch.secrets.is_empty());
}

#[test]
fn a_spec_without_open_decisions_is_registered_ready() {
    let mut sim = sim(|cfg| cfg.architect_model = Some("opus".into()));
    let id = drafted(&sim, vec![write(good(false))]);
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    assert_eq!(sim.draft_rounds(id).len(), 1);
    let spec = sim.spec_of(d.draft.spec_id.unwrap());
    assert_eq!(spec.state, SpecState::Ready);
    assert_eq!(
        sim.world.launches().pop().unwrap().model.as_deref(),
        Some("opus")
    );
}

#[test]
fn a_draft_that_never_passes_fails_after_its_last_round_without_a_commit() {
    let mut sim = sim(|_| {});
    let id = drafted(&sim, vec![write(bad())]);
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Failed);
    assert_eq!(
        failure(&d),
        ("lint_rounds_exhausted".into(), json!({ "rounds": 4 }))
    );
    assert_eq!(
        saka_core::draft::primary_action(&d),
        Some(PrimaryAction::OpenWritten)
    );
    assert_eq!(sim.draft_rounds(id).len(), 4);
    assert!(sim.world.doc_commits().is_empty());
    assert_eq!(sim.world.doc(PATH), None);
    let d = run_to(&mut sim, id, |d| {
        d.environment
            .as_ref()
            .is_some_and(|e| e.state == EnvironmentState::TornDown)
    });
    assert_eq!(d.draft.state, SpecDraftState::Failed);
}

#[test]
fn a_missing_file_counts_as_a_round_and_asks_for_the_whole_spec() {
    let mut sim = sim(|_| {});
    let id = drafted(&sim, vec![DraftTurn::Nothing, write(good(true))]);
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    let rounds = sim.draft_rounds(id);
    assert_eq!(rounds[0].1.outcome, LintOutcome::Missing);
    assert_eq!(rounds[0].1.bytes, None);
    assert_eq!(
        sim.world.prompts()[1],
        format!(
            "Saka found no file at {PATH} after your last turn (check 1 of 4). Write the complete spec to {PATH}, then end your turn."
        )
    );
}

#[test]
fn a_retry_seeds_the_file_and_tells_the_architect_why_it_stopped() {
    let mut sim = sim(|_| {});
    let id = drafted(&sim, vec![write(bad())]);
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Failed);
    let first = d.attempt.clone().unwrap();
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry_spec_draft(
            id,
            "try with the note",
            Some("Use a semicolon.".into()),
            &cause,
            None,
        )
        .unwrap();
    assert_eq!(sim.draft(id).draft.state, SpecDraftState::Queued);
    sim.world.script_draft(vec![write(good(true))]);
    let d = run_to(&mut sim, id, |d| {
        d.draft.attempt == 2 && d.draft.state == SpecDraftState::Drafting
    });
    let attempt = d.attempt.clone().unwrap();
    assert_ne!(attempt.id, first.id);
    assert_eq!(attempt.trigger, saka_core::vocab::Trigger::Manual);
    let env = saka_core::ids::EnvironmentId(attempt.id.0);
    assert_eq!(sim.world.worktree_file(env, PATH), Some(bad()));
    let packet = sim
        .engine()
        .store()
        .read(|c| saka_store::draft::packet(c, attempt.id))
        .unwrap()
        .unwrap();
    assert!(
        packet.contains(&format!(
            "Previous attempt 1 stopped: lint_rounds_exhausted. {PATH} already contains what it wrote. Its last check found:\n- line "
        )),
        "{packet}"
    );
    assert!(packet.contains("Operator note:\nUse a semicolon.\n"));
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    assert_eq!(d.draft.retries, 1);
}

#[test]
fn a_server_restart_reattaches_a_running_draft_session() {
    let mut sim = sim(|cfg| {
        cfg.agent = AgentScript {
            outcome: AgentOutcome::Hang,
            ..AgentScript::default()
        }
    });
    let id = drafted(&sim, vec![write(good(true))]);
    run_to(&mut sim, id, |d| {
        d.session
            .as_ref()
            .is_some_and(|s| s.state == SessionState::Running && s.cli_session_id.is_some())
    });
    sim.restart();
    assert!(sim.run_until(200, |s| s.stats().reattaches > 0));
    let d = sim.draft(id);
    assert_eq!(d.draft.state, SpecDraftState::Drafting);
    assert_eq!(sim.draft_sessions(id).len(), 1);
    assert!(
        sim.draft_sessions(id)
            .iter()
            .all(|s| s.state != SessionState::Lost)
    );
}

#[test]
fn a_killed_runner_resumes_once_in_the_same_place_then_fails_the_draft() {
    let mut sim = sim(|cfg| {
        cfg.agent = AgentScript {
            outcome: AgentOutcome::Hang,
            ..AgentScript::default()
        }
    });
    let id = drafted(&sim, vec![write(good(true))]);
    let running = |d: &Draft| {
        d.session
            .as_ref()
            .is_some_and(|s| s.state == SessionState::Running && s.cli_session_id.is_some())
    };
    let first = run_to(&mut sim, id, running).session.unwrap();
    sim.world.kill_runner(first.id);
    let second = run_to(&mut sim, id, |d| {
        running(d) && d.session.as_ref().is_some_and(|s| s.id != first.id)
    })
    .session
    .unwrap();
    assert_eq!(second.resumed_from_session_id, Some(first.id));
    assert_eq!(second.environment_id, first.environment_id);
    let launches = sim.world.launches();
    assert_eq!(launches.len(), 2);
    assert_eq!(launches[0].profile, launches[1].profile);
    sim.world.kill_runner(second.id);
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Failed);
    assert_eq!(failure(&d).0, "session_lost");
}

#[test]
fn a_crash_after_the_push_converges_on_one_commit_and_one_spec() {
    let mut sim = sim(|_| {});
    let id = drafted(&sim, vec![write(good(true))]);
    run_to(&mut sim, id, |d| {
        d.draft.state == SpecDraftState::Committing
    });
    for _ in 0..20 {
        if !sim.world.doc_commits().is_empty() {
            break;
        }
        sim.crash_mid_job();
    }
    assert_eq!(sim.world.doc_commits().len(), 1, "the commit ran once");
    assert_eq!(sim.draft(id).draft.state, SpecDraftState::Committing);
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    assert_eq!(sim.world.doc_commits().len(), 2, "the commit job ran again");
    assert_eq!(events(&sim, "spec.spec.registered").len(), 2);
    assert_eq!(base_commits(&sim), 0, "the repeat found the file unchanged");
    assert_eq!(sim.world.doc(PATH).unwrap(), good(true));
    let spec = sim.spec_of(d.draft.spec_id.unwrap());
    assert_eq!(spec.path, PATH);
    assert_eq!(sim.spec_by_slug("export"), spec.id);
}

#[test]
fn a_crash_between_a_turn_and_its_round_records_the_round_once() {
    let mut sim = sim(|_| {});
    let id = drafted(&sim, vec![write(bad()), write(good(true))]);
    run_to(&mut sim, id, |d| {
        d.session
            .as_ref()
            .is_some_and(|s| s.state == SessionState::Idle && s.turns == 1)
            && d.last_round.is_none()
    });
    sim.restart();
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    let rounds = sim.draft_rounds(id);
    assert_eq!(rounds.len(), 2);
    assert_eq!(rounds[0].1.turn, 1);
    assert_eq!(rounds[1].1.turn, 2);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn crashes_at_any_point_never_double_a_draft_effect(points in proptest::collection::vec(0usize..60, 1..4)) {
        let mut sim = sim(|_| {});
        let id = drafted(&sim, vec![write(bad()), write(good(true))]);
        let mut points = points;
        points.sort_unstable();
        let mut step = 0;
        for point in points {
            while step < point && !sim.draft(id).draft.state.is_finished() {
                sim.operate();
                sim.step();
                step += 1;
            }
            sim.crash_mid_job();
        }
        let d = finished(&mut sim, id);
        prop_assert_eq!(d.draft.state, SpecDraftState::Succeeded);
        let rounds = sim.draft_rounds(id);
        prop_assert_eq!(rounds.last().map(|(_, r)| r.outcome), Some(LintOutcome::Passed));
        let mut turns: Vec<_> = rounds.iter().map(|(_, r)| (r.session_id, r.turn)).collect();
        turns.dedup();
        prop_assert_eq!(turns.len(), rounds.len());
        prop_assert!(base_commits(&sim) <= 1);
        prop_assert_eq!(sim.world.doc(PATH), Some(good(true)));
        let live: Vec<_> = sim.draft_sessions(id).into_iter().filter(|s| s.state.is_live()).collect();
        prop_assert!(live.is_empty());
    }
}

fn retried(sim: &Sim, id: SpecDraftId) {
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .retry_spec_draft(id, "try again", None, &cause, None)
        .unwrap();
}

fn torn_down(sim: &Sim, env: saka_core::ids::EnvironmentId) -> bool {
    events(sim, "runtime.environment.torn_down")
        .iter()
        .any(|e| e.payload["environment_id"] == json!(env))
}

#[test]
fn an_older_attempt_whose_teardown_failed_is_still_torn_down_hourly() {
    let mut sim = sim(|cfg| cfg.draft.lint_rounds = 1);
    let id = drafted(&sim, vec![write(bad())]);
    sim.world.refuse_teardowns(true);
    let d = run_to(&mut sim, id, |d| {
        d.environment
            .as_ref()
            .is_some_and(|e| e.state == EnvironmentState::TeardownFailed)
    });
    assert_eq!(d.draft.state, SpecDraftState::Failed);
    let first = d.environment.unwrap().id;
    retried(&sim, id);
    sim.world.script_draft(vec![write(good(true))]);
    let d = run_to(&mut sim, id, |d| {
        d.draft.attempt == 2 && d.draft.state == SpecDraftState::Drafting
    });
    assert_ne!(d.environment.as_ref().unwrap().id, first);
    assert_eq!(d.retired_environments.len(), 1);
    assert!(sim.world.worktree_live(first));
    sim.world.refuse_teardowns(false);
    assert!(
        sim.run_until(STEPS, |s| torn_down(s, first)),
        "the first attempt's worktree is never removed"
    );
    assert!(!sim.world.worktree_live(first));
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    assert!(d.retired_environments.is_empty());
}

#[test]
fn a_retry_whose_chosen_context_is_gone_fails_as_context_unavailable() {
    let mut sim = sim(|cfg| cfg.draft.lint_rounds = 1);
    sim.world.put_doc("docs/prd.md", b"# PRD\n");
    sim.world.script_draft(vec![write(bad())]);
    let id = sim
        .create_draft(PATH, IDEA, &["docs/prd.md"])
        .unwrap()
        .draft
        .id;
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Failed);
    sim.world.remove_doc("docs/prd.md");
    retried(&sim, id);
    let d = finished(&mut sim, id);
    assert_eq!(
        failure(&d),
        (
            "context_unavailable".into(),
            json!({ "path": "docs/prd.md" })
        )
    );
    assert_eq!(d.draft.attempt, 2);
    let env = saka_core::ids::EnvironmentId(d.attempt.as_ref().unwrap().id.0);
    assert!(
        !sim.world.worktree_live(env),
        "the prepared worktree is removed"
    );
    assert!(saka_core::draft::can_retry(&d));
}

#[test]
fn a_file_that_appears_on_the_branch_first_fails_as_target_exists() {
    let mut sim = sim(|_| {});
    let id = drafted(&sim, vec![write(good(true))]);
    run_to(&mut sim, id, |d| d.draft.state == SpecDraftState::Drafting);
    sim.world.put_doc(PATH, b"# Someone else\n");
    let blob = saka_testkit::git_blob(b"# Someone else\n");
    let d = finished(&mut sim, id);
    let (code, params) = failure(&d);
    assert_eq!(code, "target_exists");
    assert_eq!(params["blob"], json!(blob));
    assert_eq!(sim.world.doc(PATH).unwrap(), b"# Someone else\n");
    assert_eq!(
        saka_core::draft::primary_action(&d),
        Some(PrimaryAction::OpenWritten)
    );
    assert!(!saka_core::draft::can_retry(&d));
}

#[test]
fn commit_refusals_map_to_their_draft_failures() {
    for (scripted, code, params) in [
        (
            DocWrite::PathConflict {
                path: "docs/specs".into(),
            },
            "target_path_conflict",
            json!({ "path": "docs/specs" }),
        ),
        (
            DocWrite::Rejected,
            "push_rejected",
            json!({ "branch": "main" }),
        ),
    ] {
        let mut sim = sim(|_| {});
        let id = drafted(&sim, vec![write(good(true))]);
        sim.world.script_doc_write(scripted);
        let d = finished(&mut sim, id);
        assert_eq!(failure(&d), (code.to_string(), params));
        assert_eq!(
            saka_core::draft::primary_action(&d),
            Some(PrimaryAction::OpenWritten)
        );
    }
}

#[test]
fn a_slug_taken_at_registration_keeps_the_commit_and_offers_the_file() {
    let mut sim = sim(|_| {});
    let id = drafted(&sim, vec![write(good(true))]);
    sim.register("docs/archive/export.md", &one_step_spec("Old export", ""));
    let d = finished(&mut sim, id);
    let (code, params) = failure(&d);
    assert_eq!(code, "register_failed");
    assert_eq!(params["code"], json!("spec_slug_conflict"));
    assert_eq!(
        d.draft.commit_sha.as_deref(),
        Some(sim.world.base_head().as_str())
    );
    assert_eq!(
        saka_core::draft::primary_action(&d),
        Some(PrimaryAction::OpenFile)
    );
}

type Tune = Box<dyn FnOnce(&mut SimConfig)>;

fn exhausted_dimension(tune: impl FnOnce(&mut SimConfig), turns: Vec<DraftTurn>) -> (Sim, Draft) {
    let mut sim = sim(tune);
    let id = drafted(&sim, turns);
    let d = finished(&mut sim, id);
    (sim, d)
}

#[test]
fn every_budget_dimension_fails_the_draft_after_one_warning() {
    let heavy = TurnUsage {
        cost_usd_micros: 450_000,
        input_tokens: 450,
        output_tokens: 0,
    };
    let cases: Vec<(&str, Tune, Vec<DraftTurn>)> = vec![
        (
            "turns",
            Box::new(|cfg: &mut SimConfig| {
                cfg.draft.budget.turns = 5;
                cfg.draft.lint_rounds = 10;
            }),
            vec![write(bad())],
        ),
        (
            "running_minutes",
            Box::new(|cfg: &mut SimConfig| {
                cfg.draft.budget.running_minutes = 1;
                cfg.agent.outcome = AgentOutcome::Hang;
            }),
            vec![write(bad())],
        ),
        (
            "tokens",
            Box::new(move |cfg: &mut SimConfig| {
                cfg.draft.budget.tokens = 1_000;
                cfg.agent.usage = heavy;
            }),
            vec![write(bad())],
        ),
        (
            "cost_usd",
            Box::new(move |cfg: &mut SimConfig| {
                cfg.draft.budget.cost_usd_micros = 1_000_000;
                cfg.agent.usage = heavy;
            }),
            vec![write(bad())],
        ),
    ];
    for (dimension, tune, turns) in cases {
        let (sim, d) = exhausted_dimension(tune, turns);
        let (code, params) = failure(&d);
        assert_eq!(code, "budget_exhausted", "{dimension}: {params}");
        assert_eq!(params["dimension"], json!(dimension));
        let warned: Vec<_> = events(&sim, "runtime.budget.warned")
            .into_iter()
            .filter(|e| e.payload["dimension"] == json!(dimension))
            .collect();
        // Turns are counted before each turn is sent, so that limit stops without a warning.
        let expected = usize::from(dimension != "turns");
        assert_eq!(warned.len(), expected, "{dimension}");
        assert_eq!(d.attempt.unwrap().budget_warnings.len(), expected);
    }
}

fn rate_limited(top: &str, utilization: f64, now: i64) -> AgentScript {
    let resets_at = (now + 3 * 3_600_000) / 1000;
    AgentScript {
        rate_limit: Some(json!({
            "status": top, "rateLimitType": "five_hour", "resetsAt": resets_at,
            "unifiedWindows": {
                "five_hour": { "utilization": utilization, "status": top, "resetsAt": resets_at },
            },
        })),
        ..AgentScript::default()
    }
}

#[test]
fn a_refused_rate_limit_fails_the_draft_as_quota_exhausted() {
    let mut sim = sim(|_| {});
    let now = sim.engine().now().0;
    sim.world.set_script(rate_limited("rejected", 1.0, now));
    let id = drafted(&sim, vec![write(good(true))]);
    let d = finished(&mut sim, id);
    let (code, params) = failure(&d);
    assert_eq!(code, "quota_exhausted");
    assert_eq!(params["account"], json!("claude:default"));
}

#[test]
fn a_busy_account_holds_queued_drafts_until_the_window_resets() {
    let mut sim = sim(|_| {});
    let now = sim.engine().now().0;
    sim.world
        .set_script(rate_limited("allowed_warning", 0.95, now));
    let first = drafted(&sim, vec![write(good(true))]);
    let d = finished(&mut sim, first);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    sim.world.set_script(AgentScript::default());
    let second = sim
        .create_draft("docs/specs/import.md", IDEA, &[])
        .unwrap()
        .draft
        .id;
    assert!(!sim.run_until(100, |s| s.draft(second).draft.state
        != SpecDraftState::Queued));
    let view = sim.engine().spec_draft(second).unwrap();
    assert!(
        matches!(&view.waiting_on[..], [Waiting::Quota { window, .. }] if window == "five_hour"),
        "{:?}",
        view.waiting_on
    );
    sim.world.script_draft(vec![DraftTurn::Write {
        path: "docs/specs/import.md".into(),
        bytes: good(true),
    }]);
    let d = finished(&mut sim, second);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    assert!(sim.engine().now().0 >= now + 3 * 3_600_000);
}

#[test]
fn one_architect_slot_queues_the_second_draft_without_taking_task_slots() {
    let mut sim = sim(|cfg| {
        cfg.max_active_tasks = 1;
        cfg.agent.outcome = AgentOutcome::Hang;
    });
    let first = drafted(&sim, vec![write(good(true))]);
    let second = sim
        .create_draft("docs/specs/import.md", IDEA, &[])
        .unwrap()
        .draft
        .id;
    run_to(&mut sim, first, |d| {
        d.draft.state == SpecDraftState::Drafting
    });
    sim.approve();
    assert!(sim.run_until(STEPS, |s| s.task(1).item.state == WorkItemState::InStage));
    assert_eq!(sim.draft(second).draft.state, SpecDraftState::Queued);
    let view = sim.engine().spec_draft(second).unwrap();
    assert_eq!(view.waiting_on, vec![Waiting::ArchitectSlot]);

    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    engine
        .cancel_spec_draft(first, "not needed", &cause, None)
        .unwrap();
    run_to(&mut sim, second, |d| {
        d.draft.state == SpecDraftState::Drafting
    });
    assert_eq!(sim.draft(first).draft.state, SpecDraftState::Cancelled);
}

#[test]
fn an_operator_stop_fails_the_draft_and_architect_permissions_never_ask() {
    let mut sim = sim(|cfg| {
        cfg.agent = AgentScript {
            outcome: AgentOutcome::Hang,
            ask: Some(("Bash".into(), json!({ "command": "ls" }))),
            ..AgentScript::default()
        }
    });
    let id = drafted(&sim, vec![write(good(true))]);
    let d = run_to(&mut sim, id, |d| {
        d.session
            .as_ref()
            .is_some_and(|s| s.state == SessionState::Running)
    });
    assert!(sim.run_until(STEPS, |s| s.stats().answers_to_cli == 1));
    let requests = sim
        .engine()
        .permission_requests(
            &saka_engine::PermissionFilter {
                spec_draft: Some(id),
                ..Default::default()
            },
            &saka_engine::Page {
                after: None,
                limit: 10,
            },
        )
        .unwrap()
        .items;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].request.state,
        saka_core::vocab::PermissionState::Denied
    );
    assert_eq!(
        requests[0].request.answer_source.as_deref(),
        Some("policy:architect:deny")
    );
    let engine = sim.engine();
    let cause = engine.cause("human:operator:local");
    let session = d.session.unwrap().id;
    engine
        .session_command(
            session,
            SessionCommand::Stop {
                reason: "wrong idea".into(),
            },
            &cause,
            None,
        )
        .unwrap();
    let d = finished(&mut sim, id);
    let (code, params) = failure(&d);
    assert_eq!(code, "session_failed");
    assert_eq!(params["cause"], json!("operator_stop"));
}

#[test]
fn writing_the_target_through_a_relative_path_is_allowed() {
    let mut sim = sim(|cfg| {
        cfg.agent.ask = Some(("Write".into(), json!({ "file_path": "specs/export.md" })));
    });
    let id = drafted(&sim, vec![write(good(true))]);
    let d = finished(&mut sim, id);
    assert_eq!(d.draft.state, SpecDraftState::Succeeded);
    let requests = sim
        .engine()
        .permission_requests(
            &saka_engine::PermissionFilter {
                spec_draft: Some(id),
                ..Default::default()
            },
            &saka_engine::Page {
                after: None,
                limit: 10,
            },
        )
        .unwrap()
        .items;
    assert_eq!(
        requests[0].request.answer_source.as_deref(),
        Some("policy:architect:target")
    );
}

#[test]
fn creating_a_draft_refuses_a_taken_target_and_bad_context() {
    let sim = sim(|_| {});
    let taken = |r: Result<Draft, saka_engine::EngineError>| match r {
        Err(saka_engine::EngineError::Rejected(rejection)) => {
            (rejection.code().to_string(), rejection.params())
        }
        other => panic!("expected a rejection, got {other:?}"),
    };
    sim.world.put_doc("docs/specs/there.md", b"# There\n");
    let blob = saka_testkit::git_blob(b"# There\n");
    let (code, params) = taken(sim.create_draft("docs/specs/there.md", IDEA, &[]));
    assert_eq!(code, "spec_draft_target_taken");
    assert_eq!(params["reason"], json!("file_exists"));
    assert_eq!(params["blob"], json!(blob));
    let (_, params) = taken(sim.create_draft("docs/specs/tasks-done.md", IDEA, &[]));
    assert_eq!(params["reason"], json!("spec_registered"));
    sim.register("docs/archive/taken.md", &one_step_spec("Taken", ""));
    let (_, params) = taken(sim.create_draft("docs/specs/taken.md", IDEA, &[]));
    assert_eq!(params["reason"], json!("slug_taken"));
    let live = sim.create_draft(PATH, IDEA, &[]).unwrap().draft.id;
    let (_, params) = taken(sim.create_draft(PATH, IDEA, &[]));
    assert_eq!(params["reason"], json!("draft_active"));
    assert_eq!(params["spec_draft_id"], json!(live));

    let invalid = |r: Result<Draft, saka_engine::EngineError>| match r {
        Err(saka_engine::EngineError::Invalid { field, code }) => (field.to_string(), code),
        Err(saka_engine::EngineError::InvalidItem { field, code }) => (field, code),
        other => panic!("expected a validation error, got {other:?}"),
    };
    let path = "docs/specs/new.md";
    assert_eq!(
        invalid(sim.create_draft(path, " ", &[])),
        ("idea".into(), "required".into())
    );
    assert_eq!(
        invalid(sim.create_draft(path, &"x".repeat(8001), &[])),
        ("idea".into(), "too_long".into())
    );
    assert_eq!(
        invalid(sim.create_draft("docs/notes.txt", IDEA, &[])).0,
        "path"
    );
    let many: Vec<String> = (0..17).map(|i| format!("docs/c{i}.md")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(
        invalid(sim.create_draft(path, IDEA, &many)),
        ("context_paths".into(), "too_many".into())
    );
    sim.world.put_doc("docs/prd.md", b"# PRD\n");
    assert_eq!(
        invalid(sim.create_draft(path, IDEA, &["docs/prd.md", "docs/prd.md"])),
        ("context_paths".into(), "duplicate".into())
    );
    assert_eq!(
        invalid(sim.create_draft(path, IDEA, &[path])),
        ("context_paths".into(), "same_as_path".into())
    );
    assert_eq!(
        invalid(sim.create_draft(path, IDEA, &["docs/prd.md", "docs/gone.md"])),
        ("context_paths[1]".into(), "not_found".into())
    );
    sim.world.put_doc("docs/big.md", &vec![b'a'; 200 * 1024]);
    sim.world.put_doc("docs/big2.md", &vec![b'b'; 100 * 1024]);
    assert_eq!(
        invalid(sim.create_draft(path, IDEA, &["docs/big.md", "docs/big2.md"])),
        ("context_paths".into(), "too_large".into())
    );
}

#[test]
fn draft_decisions_are_logged_without_the_idea_or_the_content() {
    saka_testkit::logs::check_callsites_per_thread();
    let buffer = Buffer::default();
    let out = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || out.clone())
        .finish();
    let id = tracing::subscriber::with_default(subscriber, || {
        let mut sim = sim(|_| {});
        let id = drafted(&sim, vec![write(bad()), write(good(true))]);
        let d = finished(&mut sim, id);
        assert_eq!(d.draft.state, SpecDraftState::Succeeded);
        let failing = drafted_at(&sim, "docs/specs/failing.md", bad());
        let d = finished(&mut sim, failing);
        assert_eq!(d.draft.state, SpecDraftState::Failed);
        let engine = sim.engine();
        let cause = engine.cause("human:operator:local");
        engine
            .cancel_spec_draft(failing, "giving up", &cause, None)
            .unwrap();
        let metrics = engine.metrics().unwrap();
        assert_eq!(metrics.spec_drafts_active, 0);
        let finished: std::collections::BTreeMap<_, _> =
            metrics.spec_drafts_finished.into_iter().collect();
        assert_eq!(finished.get("succeeded"), Some(&1));
        assert_eq!(finished.get("failed"), Some(&1));
        assert_eq!(finished.get("cancelled"), Some(&1));
        let rounds: std::collections::BTreeMap<_, _> =
            metrics.spec_draft_lint_rounds.into_iter().collect();
        assert_eq!(rounds.get("passed"), Some(&1));
        assert_eq!(rounds.get("failed"), Some(&5));
        id
    });
    let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let message = |l: &Value| l["fields"]["message"].as_str().unwrap_or("").to_string();
    for wanted in [
        "spec_draft.requested",
        "spec_draft.admitted",
        "spec_draft.linted",
        "spec_draft.committed",
        "spec_draft.succeeded",
        "spec_draft.failed",
        "spec_draft.cancelled",
    ] {
        let line = lines
            .iter()
            .find(|l| message(l) == wanted)
            .unwrap_or_else(|| panic!("no {wanted} log"));
        assert!(line["fields"]["spec_draft_id"].is_string(), "{line}");
        assert!(line["fields"].get("attempt").is_some(), "{line}");
    }
    let linted = lines
        .iter()
        .find(|l| {
            message(l) == "spec_draft.linted"
                && l["fields"]["spec_draft_id"] == json!(id.to_string())
        })
        .unwrap();
    assert_eq!(linted["fields"]["round"], json!(1));
    let failed = lines
        .iter()
        .find(|l| message(l) == "spec_draft.failed")
        .unwrap();
    assert_eq!(failed["fields"]["code"], json!("lint_rounds_exhausted"));
    assert!(!text.contains(IDEA));
    assert!(!text.contains("Export tasks"));
    assert!(!text.contains("You are the architect"));
}

fn drafted_at(sim: &Sim, path: &str, bytes: Vec<u8>) -> SpecDraftId {
    sim.world.script_draft(vec![DraftTurn::Write {
        path: path.into(),
        bytes,
    }]);
    sim.create_draft(path, IDEA, &[]).unwrap().draft.id
}

#[derive(Clone, Default)]
struct Buffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
