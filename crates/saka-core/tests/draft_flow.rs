#![allow(clippy::unwrap_used, reason = "test assertions")]

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use saka_core::Ctx;
use saka_core::draft::{self, DraftConfig, DraftInput, NewDraft, PacketFacts};
use saka_core::event::{Event, Prompt};
use saka_core::ids::{
    EnvironmentId, IdSource, ProjectId, RepoId, SessionId, SpecDraftAttemptId, SpecDraftId, SpecId,
    SpecRevisionId,
};
use saka_core::job::{Job, JobKind};
use saka_core::model::{AccountQuota, Budget, Coded, Draft, LintRound};
use saka_core::rejection::Rejection;
use saka_core::session::{SessionConfig, SessionInput};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    DraftFailureCode, DraftLocale, LintOutcome, PrimaryAction, SessionState, SpecDraftState,
};
use serde_json::json;
use uuid::Uuid;

const T0: Timestamp = Timestamp(1_700_000_000_000);

struct Seq(u128);

impl IdSource for Seq {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

fn cfg() -> DraftConfig {
    DraftConfig {
        max_active: 1,
        lint_rounds: 4,
        budget: Budget {
            running_minutes: 30,
            turns: 6,
            tokens: 0,
            cost_usd_micros: 0,
        },
        session: SessionConfig {
            permission_wait_ms: 24 * 3_600_000,
            decision_wait_ms: 72 * 3_600_000,
            idle_ms: 30 * 60_000,
            budget_warn_percent: 80,
            rate_limit_pause: 0.9,
            account: "claude:default".into(),
        },
    }
}

struct World {
    d: Draft,
    cfg: DraftConfig,
    now: Timestamp,
    ids: Seq,
    jobs: Vec<Job>,
    events: Vec<Event>,
}

impl World {
    fn new() -> World {
        World::with(cfg())
    }

    fn with(cfg: DraftConfig) -> World {
        let event = draft::request(NewDraft {
            spec_draft_id: SpecDraftId(Uuid::from_u128(1)),
            project_id: ProjectId(Uuid::from_u128(2)),
            path: "docs/specs/avatar.md".into(),
            slug: "avatar".into(),
            idea: "Let users upload an avatar".into(),
            locale: DraftLocale::En,
            context_paths: vec![],
        });
        let d = draft::evolve(None, &event, T0).unwrap();
        World {
            d,
            cfg,
            now: T0,
            ids: Seq(1000),
            jobs: Vec::new(),
            events: vec![event],
        }
    }

    fn go(&mut self, input: DraftInput) -> Result<Vec<Event>, Rejection> {
        let mut ctx = Ctx {
            now: self.now,
            ids: &mut self.ids,
        };
        let events = draft::decide(&self.d, input, &self.cfg, &mut ctx)?;
        for e in &events {
            self.d = draft::evolve(Some(self.d.clone()), e, self.now).unwrap();
            self.jobs.extend(draft::react(&self.d, e, self.now));
        }
        self.events.extend(events.iter().cloned());
        Ok(events)
    }

    fn ok(&mut self, input: DraftInput) -> Vec<Event> {
        self.go(input).unwrap()
    }

    fn state(&self) -> SpecDraftState {
        self.d.draft.state
    }

    fn take_jobs(&mut self) -> Vec<JobKind> {
        std::mem::take(&mut self.jobs)
            .into_iter()
            .map(|j| j.kind)
            .collect()
    }

    fn admit(&mut self) -> Vec<Event> {
        let attempt_id = SpecDraftAttemptId(Uuid::from_u128(self.ids.0 + 7_000));
        self.ids.0 += 1;
        let budget = self.cfg.budget;
        self.ok(DraftInput::Admit { attempt_id, budget })
    }

    fn attempt_id(&self) -> SpecDraftAttemptId {
        self.d.attempt.as_ref().unwrap().id
    }

    fn prepare(&mut self) -> Vec<Event> {
        let id = EnvironmentId(self.attempt_id().0);
        self.ok(DraftInput::EnvironmentPrepared {
            environment_id: id,
            repo_id: RepoId(Uuid::from_u128(3)),
            root_path: format!("/envs/{id}"),
            worktree_path: format!("/envs/{id}/worktree"),
            base_sha: "a".repeat(40),
            packet: PacketFacts {
                text: "<documents>".into(),
                sha256: "b".repeat(64),
                documents: vec![],
            },
        })
    }

    fn launch(&mut self) -> Vec<Event> {
        let session_id = SessionId(Uuid::from_u128(self.ids.0 + 9_000));
        self.ids.0 += 1;
        self.ok(DraftInput::Session(SessionInput::Launched {
            session_id,
            cli: "claude".into(),
            cli_version: None,
            model: None,
            account: "default".into(),
            runner_unit: "unit".into(),
            socket_path: "/s".into(),
            log_path: "/l".into(),
            author: saka_core::authorship::SessionAuthor::saka(),
        }))
    }

    fn finish_turn(&mut self) -> Vec<Event> {
        let turn = self.d.session.as_ref().unwrap().turns;
        self.ok(DraftInput::Session(SessionInput::TurnAccepted { turn }));
        self.now = self.now.plus_ms(1_000);
        self.ok(DraftInput::Session(SessionInput::TurnEnded {
            turn,
            status: "success".into(),
            cost_usd_micros: Some(10),
            input_tokens: Some(10),
            output_tokens: Some(10),
        }))
    }

    fn round(&self, outcome: LintOutcome) -> LintRound {
        let s = self.d.session.as_ref().unwrap();
        let content = (outcome != LintOutcome::Missing).then(|| b"---\nsaka: spec\n---\n".to_vec());
        LintRound {
            spec_draft_id: self.d.draft.id,
            attempt_id: self.attempt_id(),
            round: 0,
            session_id: s.id,
            turn: s.turns,
            outcome,
            bytes: content.as_ref().map(|c| c.len() as u64),
            content_hash: content
                .as_ref()
                .map(|_| format!("sha256:{}", "c".repeat(64))),
            content,
            parser_version: 4,
            issues: match outcome {
                LintOutcome::Failed => vec![
                    json!({ "code": "ac_missing", "severity": "error", "line": 1, "params": {} }),
                ],
                _ => vec![],
            },
            decisions_open: 0,
            created_at: self.now,
        }
    }

    fn lint(&mut self, outcome: LintOutcome) -> Vec<Event> {
        let round = self.round(outcome);
        self.ok(DraftInput::Linted(round))
    }

    fn drafting(&mut self) {
        self.admit();
        self.prepare();
        self.launch();
        self.finish_turn();
    }

    fn committing(&mut self) {
        self.drafting();
        self.lint(LintOutcome::Passed);
    }
}

fn kinds(events: &[Event]) -> Vec<String> {
    events.iter().map(Event::kind).collect()
}

#[test]
fn a_draft_walks_from_queued_to_succeeded() {
    let mut w = World::new();
    assert_eq!(w.state(), SpecDraftState::Queued);
    assert_eq!(kinds(&w.admit()), ["spec.draft.attempt_started"]);
    assert_eq!(w.state(), SpecDraftState::Preparing);
    assert_eq!(w.take_jobs(), [JobKind::EnvPrepare]);
    assert_eq!(
        kinds(&w.prepare()),
        ["runtime.environment.prepared", "spec.draft.packet_rendered"]
    );
    assert_eq!(w.state(), SpecDraftState::Drafting);
    assert_eq!(w.take_jobs(), [JobKind::SessionStart]);
    let started = w.launch();
    assert!(started.iter().any(|e| matches!(
        e,
        Event::SessionTurnRequested {
            turn: 1,
            prompt: Prompt::DraftPacket,
            ..
        }
    )));
    w.take_jobs();
    w.finish_turn();
    assert!(w.take_jobs().contains(&JobKind::DraftLint));
    let linted = w.lint(LintOutcome::Failed);
    assert!(linted.iter().any(|e| matches!(
        e,
        Event::SessionTurnRequested {
            turn: 2,
            prompt: Prompt::DraftLint {
                round: 1,
                max_rounds: 4
            },
            ..
        }
    )));
    w.take_jobs();
    w.finish_turn();
    let passed = w.lint(LintOutcome::Passed);
    assert!(kinds(&passed).contains(&"spec.draft.committing".to_string()));
    assert!(passed.iter().any(|e| matches!(
        e,
        Event::SessionEnded { end_reason, .. } if end_reason.code == "lint_passed" && end_reason.params == json!({ "round": 2 })
    )));
    assert_eq!(w.state(), SpecDraftState::Committing);
    let jobs = w.take_jobs();
    assert!(jobs.contains(&JobKind::DraftCommit) && jobs.contains(&JobKind::SessionStop));
    let done = w.ok(DraftInput::Committed {
        commit: "d".repeat(40),
        spec_id: SpecId(Uuid::from_u128(77)),
        revision_id: SpecRevisionId(Uuid::from_u128(78)),
    });
    assert_eq!(kinds(&done), ["spec.draft.succeeded"]);
    assert_eq!(w.state(), SpecDraftState::Succeeded);
    assert!(w.d.draft.finished_at.is_some());
    assert_eq!(w.take_jobs(), [JobKind::EnvTeardown]);
}

#[test]
fn a_missing_file_asks_for_the_whole_spec() {
    let mut w = World::new();
    w.drafting();
    let events = w.lint(LintOutcome::Missing);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::SessionTurnRequested {
            prompt: Prompt::DraftMissing {
                round: 1,
                max_rounds: 4
            },
            ..
        }
    )));
}

#[test]
fn the_last_failed_round_fails_the_draft_with_its_evidence() {
    let mut w = World::new();
    w.drafting();
    for _ in 0..3 {
        w.lint(LintOutcome::Failed);
        w.finish_turn();
    }
    let events = w.lint(LintOutcome::Failed);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::SpecDraftFailed { code: DraftFailureCode::LintRoundsExhausted, params, .. } if *params == json!({ "rounds": 4 })
    )));
    assert_eq!(w.state(), SpecDraftState::Failed);
    assert_eq!(w.d.session.as_ref().unwrap().state, SessionState::Ended);
    assert_eq!(
        draft::primary_action(&w.d),
        Some(PrimaryAction::OpenWritten)
    );
    assert_eq!(draft::available_actions(&w.d), ["retry", "cancel"]);
}

#[test]
fn a_round_is_recorded_once_per_turn() {
    let mut w = World::new();
    w.drafting();
    let round = w.round(LintOutcome::Failed);
    assert!(!w.ok(DraftInput::Linted(round.clone())).is_empty());
    assert!(w.ok(DraftInput::Linted(round)).is_empty());
}

#[test]
fn a_round_for_a_turn_that_is_no_longer_the_last_is_ignored() {
    let mut w = World::new();
    w.drafting();
    let stale = w.round(LintOutcome::Passed);
    let sid = w.d.session.as_ref().unwrap().id;
    w.ok(DraftInput::Session(SessionInput::Steer {
        session_id: sid,
        text: "add a decision".into(),
    }));
    assert!(w.ok(DraftInput::Linted(stale)).is_empty());
    assert_eq!(w.state(), SpecDraftState::Drafting);
}

#[test]
fn committing_cannot_be_cancelled_and_steering_is_refused() {
    let mut w = World::new();
    w.committing();
    assert_eq!(
        w.go(DraftInput::Cancel { reason: "x".into() }),
        Err(Rejection::InvalidState {
            state: "committing".into()
        })
    );
    let sid = w.d.session.as_ref().unwrap().id;
    assert!(
        w.go(DraftInput::Session(SessionInput::Steer {
            session_id: sid,
            text: "more".into()
        }))
        .is_err()
    );
}

#[test]
fn commit_failures_pick_the_primary_action() {
    let mut w = World::new();
    w.committing();
    w.ok(DraftInput::CommitFailed {
        code: DraftFailureCode::TargetExists,
        params: json!({ "blob": "x", "commit": "y" }),
        commit: None,
    });
    assert_eq!(
        draft::primary_action(&w.d),
        Some(PrimaryAction::OpenWritten)
    );
    assert_eq!(draft::available_actions(&w.d), ["cancel"]);
    assert_eq!(
        w.go(DraftInput::Retry {
            reason: "again".into(),
            note: None
        }),
        Err(Rejection::InvalidState {
            state: "failed".into()
        })
    );

    let mut r = World::new();
    r.committing();
    r.ok(DraftInput::CommitFailed {
        code: DraftFailureCode::RegisterFailed,
        params: json!({ "code": "spec_slug_conflict", "params": {} }),
        commit: Some("e".repeat(40)),
    });
    assert_eq!(draft::primary_action(&r.d), Some(PrimaryAction::OpenFile));
    assert_eq!(r.d.draft.commit_sha, Some("e".repeat(40)));
}

#[test]
fn the_fourth_retry_is_refused_after_three_failed_retries() {
    let mut w = World::new();
    w.admit();
    for _ in 0..3 {
        w.ok(DraftInput::EnvironmentFailed {
            code: DraftFailureCode::EnvSetupFailed,
            params: json!({}),
        });
        w.ok(DraftInput::Retry {
            reason: "again".into(),
            note: None,
        });
        w.admit();
    }
    assert_eq!(w.d.draft.attempt, 4);
    w.ok(DraftInput::EnvironmentFailed {
        code: DraftFailureCode::EnvSetupFailed,
        params: json!({}),
    });
    assert_eq!(
        w.go(DraftInput::Retry {
            reason: "again".into(),
            note: None
        }),
        Err(Rejection::RetryLimitReached {
            stage: None,
            limit: 3
        })
    );
    assert!(w.admit().is_empty());
}

#[test]
fn cancel_ends_the_live_session_and_tears_down() {
    let mut w = World::new();
    w.drafting();
    w.take_jobs();
    let events = w.ok(DraftInput::Cancel {
        reason: "not needed".into(),
    });
    assert!(kinds(&events).contains(&"runtime.session.ended".to_string()));
    assert_eq!(w.state(), SpecDraftState::Cancelled);
    let jobs = w.take_jobs();
    assert!(jobs.contains(&JobKind::SessionStop) && jobs.contains(&JobKind::EnvTeardown));
    assert!(w.go(DraftInput::Cancel { reason: "x".into() }).is_err());
}

#[test]
fn remembering_a_draft_permission_is_not_applicable() {
    let mut w = World::new();
    w.drafting();
    let refused = w.go(DraftInput::Session(SessionInput::AnswerPermission {
        permission_request_id: saka_core::ids::PermissionRequestId(Uuid::from_u128(5)),
        allow: true,
        message: None,
        actor: "operator:local".into(),
        remember: true,
        active_rules: vec![],
        allowed_parts: Vec::new(),
    }));
    assert_eq!(
        refused,
        Err(Rejection::Invalid {
            field: "remember",
            code: "not_applicable"
        })
    );
}

#[test]
fn an_operator_stop_fails_the_draft_with_session_failed() {
    let mut w = World::new();
    w.drafting();
    let sid = w.d.session.as_ref().unwrap().id;
    w.ok(DraftInput::Session(SessionInput::Stop {
        session_id: sid,
        reason: "wrong idea".into(),
    }));
    assert_eq!(w.state(), SpecDraftState::Failed);
    assert_eq!(
        draft::failure_code(&w.d),
        Some(DraftFailureCode::SessionFailed)
    );
    assert_eq!(
        w.d.draft.failure.as_ref().unwrap().params["cause"],
        "operator_stop"
    );
}

#[test]
fn a_lost_session_resumes_once_then_fails() {
    let mut w = World::new();
    w.drafting();
    w.take_jobs();
    w.ok(DraftInput::Session(SessionInput::Gone {
        cause: "runner_exited".into(),
    }));
    assert_eq!(w.state(), SpecDraftState::Drafting);
    assert_eq!(
        draft::phase_jobs(&w.d, w.now)
            .iter()
            .filter(|j| j.kind == JobKind::SessionStart)
            .count(),
        1
    );
    let resumed = w.launch();
    assert!(resumed.iter().any(|e| matches!(
        e,
        Event::SessionStarted {
            resume_cause: Some(saka_core::vocab::ResumeCause::Lost),
            ..
        }
    )));
    w.ok(DraftInput::Session(SessionInput::Gone {
        cause: "runner_exited".into(),
    }));
    assert_eq!(w.state(), SpecDraftState::Failed);
    assert_eq!(
        draft::failure_code(&w.d),
        Some(DraftFailureCode::SessionLost)
    );
}

#[test]
fn the_turn_budget_fails_the_draft_before_a_feedback_turn() {
    let mut c = cfg();
    c.budget.turns = 1;
    let mut w = World::with(c);
    w.drafting();
    let events = w.lint(LintOutcome::Failed);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::SpecDraftFailed {
            code: DraftFailureCode::BudgetExhausted,
            ..
        }
    )));
}

#[test]
fn a_passing_round_wins_over_an_exhausted_turn_budget() {
    let mut c = cfg();
    c.budget.turns = 1;
    let mut w = World::with(c);
    w.drafting();
    w.lint(LintOutcome::Passed);
    assert_eq!(w.state(), SpecDraftState::Committing);
}

#[test]
fn job_exhaustion_maps_to_draft_failures() {
    let mut w = World::new();
    w.drafting();
    w.ok(DraftInput::JobExhausted {
        kind: JobKind::DraftLint,
        error: Coded {
            code: "worktree_missing".into(),
            params: json!({}),
        },
    });
    assert_eq!(
        draft::failure_code(&w.d),
        Some(DraftFailureCode::WorktreeUnreadable)
    );

    let mut c = World::new();
    c.committing();
    c.ok(DraftInput::JobExhausted {
        kind: JobKind::DraftCommit,
        error: Coded {
            code: "scm_unavailable".into(),
            params: json!({}),
        },
    });
    assert_eq!(
        c.d.draft.failure.as_ref().unwrap().params,
        json!({ "phase": "committing" })
    );
}

#[test]
fn admission_respects_the_slot_and_the_account_pause() {
    let one = World::new();
    let mut two = World::new();
    two.d.draft.id = SpecDraftId(Uuid::from_u128(99));
    two.d.draft.queued_at = T0.plus_ms(1);
    let c = cfg();
    let admits = |drafts: &[Draft], quotas: &[AccountQuota]| {
        draft::reconcile(drafts, quotas, &c, T0)
            .into_iter()
            .filter_map(|a| match a {
                draft::DraftAction::Admit(id) => Some(id),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let drafts = vec![one.d.clone(), two.d.clone()];
    assert_eq!(admits(&drafts, &[]), [one.d.draft.id]);
    let waits = draft::waiting(&drafts, &[], &c, T0);
    assert_eq!(waits.len(), 1);
    assert_eq!(waits[0].1[0].kind(), "architect_slot");
    let paused = AccountQuota {
        account: "claude:default".into(),
        window: "five_hour".into(),
        utilization: 0.95,
        status: "allowed_warning".into(),
        resets_at: Some(T0.plus_ms(60_000)),
        observed_at: T0,
    };
    assert!(admits(&drafts, std::slice::from_ref(&paused)).is_empty());
    assert!(
        draft::waiting(&drafts, &[paused], &c, T0)
            .iter()
            .all(|(_, w)| w.iter().any(|x| x.kind() == "quota"))
    );
}

#[derive(Debug, Clone)]
enum Op {
    Admit,
    Prepare,
    PrepareFails,
    Launch,
    Accept,
    EndTurn(bool),
    Lint(u8, bool),
    Committed,
    CommitFails(u8),
    Retry,
    Cancel,
    Steer,
    Stop,
    Gone,
    Exhaust(u8),
    Tick(u16),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => Just(Op::Admit),
        3 => Just(Op::Prepare),
        1 => Just(Op::PrepareFails),
        3 => Just(Op::Launch),
        3 => Just(Op::Accept),
        3 => any::<bool>().prop_map(Op::EndTurn),
        4 => (0u8..3, any::<bool>()).prop_map(|(o, s)| Op::Lint(o, s)),
        2 => Just(Op::Committed),
        1 => (0u8..4).prop_map(Op::CommitFails),
        2 => Just(Op::Retry),
        1 => Just(Op::Cancel),
        1 => Just(Op::Steer),
        1 => Just(Op::Stop),
        1 => Just(Op::Gone),
        1 => (0u8..4).prop_map(Op::Exhaust),
        1 => (0u16..200).prop_map(Op::Tick),
    ]
}

fn input(w: &mut World, op: &Op) -> Option<DraftInput> {
    let session = w.d.session.clone();
    let sid = session.as_ref().map(|s| s.id);
    Some(match op {
        Op::Admit => {
            let attempt_id = SpecDraftAttemptId(Uuid::from_u128(w.ids.0 + 7_000));
            w.ids.0 += 1;
            DraftInput::Admit {
                attempt_id,
                budget: w.cfg.budget,
            }
        }
        Op::Prepare => {
            let attempt = w.d.attempt.as_ref()?.id;
            DraftInput::EnvironmentPrepared {
                environment_id: EnvironmentId(attempt.0),
                repo_id: RepoId(Uuid::from_u128(3)),
                root_path: "/r".into(),
                worktree_path: "/r/w".into(),
                base_sha: "a".repeat(40),
                packet: PacketFacts {
                    text: "p".into(),
                    sha256: "b".repeat(64),
                    documents: vec![],
                },
            }
        }
        Op::PrepareFails => DraftInput::EnvironmentFailed {
            code: DraftFailureCode::ContextUnavailable,
            params: json!({ "path": "docs/x.md" }),
        },
        Op::Launch => {
            let session_id = SessionId(Uuid::from_u128(w.ids.0 + 9_000));
            w.ids.0 += 1;
            DraftInput::Session(SessionInput::Launched {
                session_id,
                cli: "claude".into(),
                cli_version: None,
                model: None,
                account: "default".into(),
                runner_unit: "u".into(),
                socket_path: "/s".into(),
                log_path: "/l".into(),
                author: saka_core::authorship::SessionAuthor::saka(),
            })
        }
        Op::Accept => DraftInput::Session(SessionInput::TurnAccepted {
            turn: session?.turns,
        }),
        Op::EndTurn(ok) => DraftInput::Session(SessionInput::TurnEnded {
            turn: session?.turns,
            status: if *ok { "success" } else { "error_max_turns" }.into(),
            cost_usd_micros: Some(5),
            input_tokens: Some(5),
            output_tokens: Some(5),
        }),
        Op::Lint(o, stale) => {
            let session = session?;
            let outcome = [
                LintOutcome::Passed,
                LintOutcome::Failed,
                LintOutcome::Missing,
            ][*o as usize];
            let mut round = LintRound {
                spec_draft_id: w.d.draft.id,
                attempt_id: w.d.attempt.as_ref()?.id,
                round: 0,
                session_id: session.id,
                turn: session.turns,
                outcome,
                bytes: (outcome != LintOutcome::Missing).then_some(3),
                content: (outcome != LintOutcome::Missing).then(|| b"abc".to_vec()),
                content_hash: (outcome != LintOutcome::Missing).then(|| "h".into()),
                parser_version: 4,
                issues: vec![],
                decisions_open: 0,
                created_at: w.now,
            };
            if *stale {
                round.turn = round.turn.saturating_sub(1);
            }
            DraftInput::Linted(round)
        }
        Op::Committed => DraftInput::Committed {
            commit: "c".repeat(40),
            spec_id: SpecId(Uuid::from_u128(50)),
            revision_id: SpecRevisionId(Uuid::from_u128(51)),
        },
        Op::CommitFails(n) => DraftInput::CommitFailed {
            code: [
                DraftFailureCode::TargetExists,
                DraftFailureCode::TargetPathConflict,
                DraftFailureCode::PushRejected,
                DraftFailureCode::RegisterFailed,
            ][*n as usize],
            params: json!({}),
            commit: None,
        },
        Op::Retry => DraftInput::Retry {
            reason: "again".into(),
            note: None,
        },
        Op::Cancel => DraftInput::Cancel {
            reason: "stop".into(),
        },
        Op::Steer => DraftInput::Session(SessionInput::Steer {
            session_id: sid?,
            text: "more".into(),
        }),
        Op::Stop => DraftInput::Session(SessionInput::Stop {
            session_id: sid?,
            reason: "enough".into(),
        }),
        Op::Gone => DraftInput::Session(SessionInput::Gone {
            cause: "runner_exited".into(),
        }),
        Op::Exhaust(n) => DraftInput::JobExhausted {
            kind: [
                JobKind::DraftLint,
                JobKind::DraftCommit,
                JobKind::EnvPrepare,
                JobKind::SessionSendTurn,
            ][*n as usize],
            error: Coded {
                code: "x".into(),
                params: json!({}),
            },
        },
        Op::Tick(minutes) => {
            w.now = w.now.plus_ms(i64::from(*minutes) * 60_000);
            DraftInput::Tick
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn random_inputs_keep_one_attempt_one_session_and_proven_success(ops in prop::collection::vec(op(), 1..80)) {
        let mut w = World::new();
        let mut attempts: BTreeMap<SpecDraftAttemptId, bool> = BTreeMap::new();
        let mut live: BTreeSet<SessionId> = BTreeSet::new();
        let mut passed: BTreeSet<SpecDraftAttemptId> = BTreeSet::new();
        for op in &ops {
            let Some(input) = input(&mut w, op) else { continue };
            let before = w.state();
            let Ok(events) = w.go(input) else {
                prop_assert_eq!(w.state(), before);
                continue;
            };
            for e in &events {
                match e {
                    Event::SpecDraftAttemptStarted { attempt_id, .. } => {
                        prop_assert!(attempts.values().all(|running| !running), "two running attempts");
                        attempts.insert(*attempt_id, true);
                    }
                    Event::SpecDraftSucceeded { attempt_id, .. } => {
                        prop_assert!(passed.contains(attempt_id), "succeeded without a passed round");
                        attempts.insert(*attempt_id, false);
                    }
                    Event::SpecDraftFailed { attempt_id, .. } => {
                        attempts.insert(*attempt_id, false);
                    }
                    Event::SpecDraftCancelled { .. } => {
                        for running in attempts.values_mut() {
                            *running = false;
                        }
                    }
                    Event::SpecDraftLinted { attempt_id, outcome: LintOutcome::Passed, .. } => {
                        passed.insert(*attempt_id);
                    }
                    Event::SessionStarted { session_id, .. } => {
                        prop_assert!(live.is_empty(), "two live sessions");
                        live.insert(*session_id);
                    }
                    Event::SessionStateChanged { session_id, state, .. } if !state.is_live() => {
                        live.remove(session_id);
                    }
                    _ => {}
                }
            }
            let state = w.state();
            if matches!(state, SpecDraftState::Succeeded | SpecDraftState::Cancelled | SpecDraftState::Failed | SpecDraftState::Queued) {
                prop_assert!(w.d.live_session().is_none(), "a finished or queued draft keeps a live session");
            }
            if before == SpecDraftState::Committing {
                prop_assert!(matches!(state, SpecDraftState::Committing | SpecDraftState::Succeeded | SpecDraftState::Failed));
            }
            if matches!(before, SpecDraftState::Succeeded | SpecDraftState::Cancelled) {
                prop_assert_eq!(state, before);
            }
            prop_assert!(w.d.draft.retries <= draft::MANUAL_RETRY_LIMIT);
            prop_assert!(w.d.draft.attempt <= draft::MANUAL_RETRY_LIMIT + 1);
        }
    }
}
