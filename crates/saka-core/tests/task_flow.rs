#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail loudly on a broken setup"
)]

use saka_core::Ctx;
use saka_core::event::{Event, Prompt};
use saka_core::gate::Check;
use saka_core::ids::{
    ChangeRequestId, EnvironmentId, IdSource, PermissionRequestId, ProjectId, RepoId, SessionId,
    SpecRevisionId,
};
use saka_core::job::{Job, JobKind};
use saka_core::model::{
    Coded, ProjectRepo, RevisionView, Spec, StepPlan, Task, WorkItemDependency,
};
use saka_core::policy::{Decision, DecisionSource, Effect};
use saka_core::react::react;
use saka_core::reconcile::{Action, Snapshot, reconcile};
use saka_core::rejection::Rejection;
use saka_core::session::SessionInput;
use saka_core::spec::{self, NewRevision};
use saka_core::task::{self, LoopConfig, TaskInput};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    BlockerCode, ChangeState, CheckConclusion, CheckKind, CheckStatus, EnvironmentState,
    InterruptBy, MergeMethod, PermissionState, SessionState, SpecState, Stage, StagePhase,
    StageRunState, Trigger, WorkItemState,
};
use serde_json::json;
use uuid::Uuid;

struct Counter(u128);

impl IdSource for Counter {
    fn next_id(&mut self) -> Uuid {
        self.0 += 1;
        Uuid::from_u128(self.0)
    }
}

fn cfg() -> LoopConfig {
    LoopConfig {
        max_followup_turns: 1,
        permission_wait_ms: 24 * 3_600_000,
        idle_ms: 30 * 60_000,
        budgets: std::collections::BTreeMap::from([(
            saka_core::vocab::Stage::Dev,
            saka_core::model::Budget {
                running_minutes: 60,
                turns: 10,
                tokens: 0,
                cost_usd_micros: 0,
            },
        )]),
        budget_warn_percent: 80,
        rate_limit_pause: 0.9,
        account: "claude:default".into(),
        ci_appear_timeout_ms: 600_000,
        poll_interval_ms: 15_000,
        merge_method: MergeMethod::Squash,
        delete_branch_after_merge: true,
        commit_status: true,
    }
}

struct World {
    ids: Counter,
    now: Timestamp,
    spec: Spec,
    tasks: Vec<Task>,
    deps: Vec<WorkItemDependency>,
    jobs: Vec<Job>,
}

fn bindings(repo: RepoId) -> &'static [ProjectRepo] {
    Box::leak(Box::new([ProjectRepo {
        repo_id: repo,
        name: "app".into(),
        is_default: true,
        base_branch: "main".into(),
        path_scope: vec!["**".into()],
    }]))
}

fn approval(if_match: SpecRevisionId, repo: RepoId) -> spec::Approval<'static> {
    spec::Approval {
        if_match,
        reason: None,
        repos: bindings(repo),
        parser_version: 2,
        depends_on_line: 1,
        project_specs: &[],
        project_graph: &[],
        approved: None,
        tasks: &[],
    }
}

impl World {
    fn new() -> World {
        let mut ids = Counter(0);
        let now = Timestamp(1_000_000);
        let mut ctx = Ctx { now, ids: &mut ids };
        let revision = NewRevision {
            origin: saka_core::vocab::SpecOrigin::Manual,
            source_commit: "a".repeat(40),
            content_hash: "sha256:x".into(),
            context_hash: "sha256:none".into(),
            context: Vec::new(),
            lint_ok: true,
            issue_codes: vec![],
            title: "Mark tasks as done".into(),
            parser_version: 2,
            priority: 0,
        };
        let events = spec::record_revision(
            None,
            ProjectId(Uuid::from_u128(900)),
            "docs/specs/tasks-done.md",
            "tasks-done",
            revision,
            None,
            &mut ctx,
        );
        let spec = events
            .iter()
            .fold(None, |s, e| spec::evolve(s, e, now))
            .unwrap();
        World {
            ids,
            now,
            spec,
            tasks: vec![],
            deps: vec![],
            jobs: vec![],
        }
    }

    fn approve(&mut self) {
        let latest = RevisionView {
            id: self.spec.latest_revision_id,
            lint_ok: true,
            issues: json!([]),
            steps: vec![
                StepPlan {
                    id: "S1".into(),
                    index: 1,
                    work: "Add done".into(),
                    ac: vec![],
                    commands: vec![],
                    after: vec![],
                    repo: None,
                    line: 0,
                },
                StepPlan {
                    id: "S2".into(),
                    index: 2,
                    work: "Show done".into(),
                    ac: vec![],
                    commands: vec![],
                    after: vec!["S1".into()],
                    repo: None,
                    line: 0,
                },
            ],
            parser_version: 2,
            priority: 0,
            depends_on: vec![],
        };
        let mut ctx = Ctx {
            now: self.now,
            ids: &mut self.ids,
        };
        let events = spec::approve(
            &self.spec,
            &latest,
            approval(self.spec.latest_revision_id, RepoId(Uuid::from_u128(901))),
            &mut ctx,
        )
        .unwrap();
        self.deps = spec::approval_graph(&self.spec, &latest, &events, &[]).1;
        for e in &events {
            self.spec = spec::evolve(Some(self.spec.clone()), e, self.now).unwrap();
            if let Some(t) = task::evolve(None, e, self.now) {
                self.tasks.push(t);
            }
        }
    }

    fn task(&self, step: u32) -> &Task {
        self.tasks
            .iter()
            .find(|t| t.item.step_index == Some(step))
            .unwrap()
    }

    fn try_decide(&mut self, step: u32, input: TaskInput) -> Result<Vec<Event>, Rejection> {
        let i = self
            .tasks
            .iter()
            .position(|t| t.item.step_index == Some(step))
            .unwrap();
        let mut ctx = Ctx {
            now: self.now,
            ids: &mut self.ids,
        };
        let events = task::decide(&self.tasks[i], input, &cfg(), &mut ctx)?;
        let mut t = self.tasks[i].clone();
        for e in &events {
            t = task::evolve(Some(t), e, self.now).unwrap();
            self.jobs.extend(react(&t, e, &cfg(), self.now));
        }
        t.retired_runs.clear();
        self.tasks[i] = t;
        Ok(events)
    }

    fn decide(&mut self, step: u32, input: TaskInput) -> Vec<Event> {
        self.try_decide(step, input).unwrap()
    }

    fn take_jobs(&mut self) -> Vec<JobKind> {
        self.jobs.drain(..).map(|j| j.kind).collect()
    }

    fn reconcile(&self) -> Vec<Action> {
        reconcile(
            &Snapshot {
                quotas: Vec::new(),
                port_blocks: None,
                specs: vec![self.spec.clone()],
                tasks: self.tasks.clone(),
                work_item_dependencies: self.deps.clone(),
                spec_dependencies: vec![],
                ..Snapshot::default()
            },
            &cfg(),
            2,
            self.now,
        )
    }

    fn admits(&self) -> Vec<u32> {
        self.reconcile()
            .into_iter()
            .filter_map(|a| match a {
                Action::Decide(id, TaskInput::Admit) => {
                    self.tasks.iter().find(|t| t.item.id == id)?.item.step_index
                }
                _ => None,
            })
            .collect()
    }

    fn place(&self, step: u32) -> (WorkItemState, Option<Stage>, Option<StagePhase>) {
        let i = &self.task(step).item;
        (i.state, i.stage, i.stage_phase)
    }

    fn run_to_awaiting_ci(&mut self, step: u32) {
        self.decide(step, TaskInput::Admit);
        self.decide(step, prepared(step));
        self.decide(step, launched(step));
        self.decide(
            step,
            TaskInput::Session(SessionInput::TurnAccepted { turn: 1 }),
        );
        self.decide(step, turn_ended(1, "success"));
        self.decide(step, inspected(1, false, 1));
        self.decide(step, TaskInput::BranchPushed { sha: sha('b') });
        self.decide(step, opened(step));
        self.jobs.clear();
    }

    fn run_to_tearing_down(&mut self, step: u32) {
        self.run_to_awaiting_ci(step);
        self.decide(
            step,
            observed('b', vec![check("test", Some(CheckConclusion::Success))]),
        );
        let version = self.task(step).item.version;
        self.decide(
            step,
            TaskInput::RequestMerge {
                expected_version: version,
                reason: None,
            },
        );
        self.decide(
            step,
            TaskInput::MergeSucceeded {
                merge_commit_sha: sha('c'),
                merged_at: None,
            },
        );
        self.decide(
            step,
            TaskInput::MergeVerified {
                found: true,
                base_tip: None,
            },
        );
        self.jobs.clear();
    }

    fn environment_state(&self, step: u32) -> EnvironmentState {
        self.task(step).environment.as_ref().unwrap().state
    }

    fn teardown_jobs(&self) -> Vec<Job> {
        self.reconcile()
            .into_iter()
            .filter_map(|a| match a {
                Action::Enqueue(j) if j.kind == JobKind::EnvTeardown => Some(j),
                _ => None,
            })
            .collect()
    }
}

fn teardown_exhausted() -> TaskInput {
    TaskInput::JobExhausted {
        kind: JobKind::EnvTeardown,
        error: Coded {
            code: "env_io_failed".into(),
            params: json!({ "op": "remove_env_root", "error": "Permission denied (os error 13)" }),
        },
    }
}

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

fn prepared(step: u32) -> TaskInput {
    prepared_with(step, serde_json::json!({ "gate": { "source": "scm" } }))
}

fn prepared_with(step: u32, recipe: serde_json::Value) -> TaskInput {
    TaskInput::EnvironmentPrepared {
        environment_id: EnvironmentId(Uuid::from_u128(500 + u128::from(step))),
        root_path: format!("/envs/{step}"),
        worktree_path: format!("/envs/{step}/worktree"),
        base_sha: sha('a'),
        setup: Box::new(saka_core::model::EnvironmentSetup {
            recipe: Some(recipe),
            ..Default::default()
        }),
    }
}

fn launched(step: u32) -> TaskInput {
    TaskInput::Session(SessionInput::Launched {
        session_id: SessionId(Uuid::from_u128(600 + u128::from(step))),
        cli: "claude".into(),
        cli_version: Some("2.0.0".into()),
        model: None,
        account: "default".into(),
        runner_unit: format!("saka-session-{step}"),
        socket_path: "/run/s.sock".into(),
        log_path: "/data/s/events.ndjson".into(),
        author: saka_core::authorship::SessionAuthor::saka(),
    })
}

fn launched_again(step: u32) -> TaskInput {
    match launched(step) {
        TaskInput::Session(SessionInput::Launched {
            session_id,
            cli,
            cli_version,
            model,
            account,
            runner_unit,
            socket_path,
            log_path,
            author,
        }) => TaskInput::Session(SessionInput::Launched {
            session_id: SessionId(Uuid::from_u128(session_id.0.as_u128() + 50)),
            cli,
            cli_version,
            model,
            account,
            runner_unit,
            socket_path,
            log_path,
            author,
        }),
        other => other,
    }
}

fn turn_ended(turn: u32, status: &str) -> TaskInput {
    TaskInput::Session(SessionInput::TurnEnded {
        turn,
        status: status.into(),
        cost_usd_micros: Some(10),
        input_tokens: Some(1),
        output_tokens: Some(2),
    })
}

fn inspected(turn: u32, dirty: bool, commits_ahead: u32) -> TaskInput {
    TaskInput::WorktreeInspected {
        turn,
        dirty,
        commits_ahead,
        head_sha: sha('b'),
        outside: None,
    }
}

fn opened(step: u32) -> TaskInput {
    TaskInput::ChangeOpened {
        change_request_id: ChangeRequestId(Uuid::from_u128(700 + u128::from(step))),
        number: 10 + u64::from(step),
        url: "https://example.test/pr".into(),
        head_sha: sha('b'),
        base_branch: "main".into(),
        opened_with: saka_core::vocab::ChangeCredentialKind::App,
    }
}

fn check(name: &str, conclusion: Option<CheckConclusion>) -> Check {
    Check {
        name: name.into(),
        kind: CheckKind::CheckRun,
        status: if conclusion.is_some() {
            CheckStatus::Completed
        } else {
            CheckStatus::InProgress
        },
        conclusion,
        url: None,
    }
}

fn observed(head: char, checks: Vec<Check>) -> TaskInput {
    TaskInput::ChangeObserved {
        state: ChangeState::Open,
        head_sha: sha(head),
        merge_commit_sha: None,
        merged_at: None,
        checks,
    }
}

fn ask(cli_request_id: &str, id: u128) -> TaskInput {
    TaskInput::Session(SessionInput::PermissionRequested {
        permission_request_id: PermissionRequestId(Uuid::from_u128(id)),
        cli_request_id: cli_request_id.into(),
        tool: "Bash".into(),
        summary: "npm install".into(),
        input: json!({ "command": "npm install" }),
        decision: Decision {
            effect: Effect::Ask,
            source: DecisionSource::Default,
            message: None,
        },
    })
}

#[test]
fn two_steps_run_in_order_from_approve_to_shipped() {
    let mut w = World::new();
    assert_eq!(w.spec.state, SpecState::Ready);
    w.approve();
    assert_eq!(w.spec.state, SpecState::Approved);
    assert_eq!(w.tasks.len(), 2);
    assert_eq!(w.task(1).item.branch, "saka/tasks-done/s1");
    assert_eq!(w.task(2).item.task_key, "tasks-done/s2");

    assert_eq!(w.admits(), vec![1]);
    w.decide(1, TaskInput::Admit);
    assert_eq!(w.place(1), (WorkItemState::Admitted, None, None));
    assert_eq!(w.take_jobs(), vec![JobKind::EnvPrepare]);
    assert_eq!(w.admits(), Vec::<u32>::new());

    w.decide(1, prepared(1));
    assert_eq!(
        w.place(1),
        (
            WorkItemState::InStage,
            Some(Stage::Dev),
            Some(StagePhase::RunningSession)
        )
    );
    assert_eq!(w.take_jobs(), vec![JobKind::SessionStart]);

    let events = w.decide(1, launched(1));
    assert!(matches!(
        &events[1],
        Event::SessionTurnRequested {
            turn: 1,
            prompt: Prompt::WorkPacket { retry_of: None },
            ..
        }
    ));
    assert_eq!(w.take_jobs(), vec![JobKind::SessionSendTurn]);
    w.decide(
        1,
        TaskInput::Session(SessionInput::TurnAccepted { turn: 1 }),
    );
    assert_eq!(
        w.task(1).session.as_ref().unwrap().state,
        SessionState::Running
    );

    w.decide(1, ask("req-1", 800));
    assert_eq!(
        w.task(1).session.as_ref().unwrap().state,
        SessionState::Waiting
    );
    assert_eq!(w.task(1).permissions[0].state, PermissionState::Pending);
    assert!(w.take_jobs().is_empty());
    let id = PermissionRequestId(Uuid::from_u128(800));
    w.decide(
        1,
        TaskInput::Session(SessionInput::AnswerPermission {
            permission_request_id: id,
            allow: true,
            message: None,
            actor: "operator:local".into(),
            remember: false,
            active_rules: Vec::new(),
        }),
    );
    assert_eq!(
        w.task(1).session.as_ref().unwrap().state,
        SessionState::Running
    );
    assert_eq!(w.take_jobs(), vec![JobKind::SessionDeliverPermissionAnswer]);
    assert_eq!(
        w.try_decide(
            1,
            TaskInput::Session(SessionInput::AnswerPermission {
                permission_request_id: id,
                allow: false,
                message: None,
                actor: "operator:local".into(),
                remember: false,
                active_rules: Vec::new(),
            })
        ),
        Err(Rejection::PermissionAlreadyAnswered)
    );
    w.decide(
        1,
        TaskInput::Session(SessionInput::PermissionDelivered {
            permission_request_id: id,
        }),
    );
    assert!(w.task(1).permissions[0].delivered_at.is_some());
    assert!(w.decide(1, ask("req-1", 801)).is_empty());

    w.decide(1, turn_ended(1, "success"));
    assert_eq!(w.take_jobs(), vec![JobKind::EnvInspectWorktree]);
    let events = w.decide(1, inspected(1, true, 0));
    assert!(matches!(
        &events[0],
        Event::SessionTurnRequested {
            turn: 2,
            prompt: Prompt::Uncommitted {
                dirty: true,
                commits_ahead: 0
            },
            ..
        }
    ));
    w.decide(
        1,
        TaskInput::Session(SessionInput::TurnAccepted { turn: 2 }),
    );
    w.decide(1, turn_ended(2, "success"));
    w.jobs.clear();
    w.decide(1, inspected(2, false, 1));
    assert_eq!(w.place(1).2, Some(StagePhase::Pushing));
    assert_eq!(
        w.task(1).session.as_ref().unwrap().state,
        SessionState::Ended
    );
    assert_eq!(
        w.take_jobs(),
        vec![JobKind::SessionStop, JobKind::ScmPushBranch]
    );

    w.decide(1, TaskInput::BranchPushed { sha: sha('b') });
    assert_eq!(w.take_jobs(), vec![JobKind::ScmOpenChangeRequest]);
    w.decide(1, opened(1));
    assert_eq!(
        w.place(1),
        (
            WorkItemState::InStage,
            Some(Stage::Merge),
            Some(StagePhase::AwaitingCi)
        )
    );
    assert_eq!(w.task(1).attempts.get(&Stage::Merge), Some(&1));
    assert_eq!(w.take_jobs(), vec![JobKind::ScmPollChangeRequest]);

    let version = w.task(1).item.version;
    assert!(matches!(
        w.try_decide(
            1,
            TaskInput::RequestMerge {
                expected_version: version,
                reason: None
            }
        ),
        Err(Rejection::MergeNotReady { .. })
    ));
    w.decide(1, observed('b', vec![check("test", None)]));
    assert_eq!(w.place(1).2, Some(StagePhase::AwaitingCi));
    w.decide(
        1,
        observed('b', vec![check("test", Some(CheckConclusion::Success))]),
    );
    assert_eq!(w.place(1).2, Some(StagePhase::AwaitingMerge));
    assert!(
        w.decide(
            1,
            observed('b', vec![check("test", Some(CheckConclusion::Success))])
        )
        .is_empty()
    );

    let version = w.task(1).item.version;
    assert_eq!(
        w.try_decide(
            1,
            TaskInput::RequestMerge {
                expected_version: version - 1,
                reason: None
            }
        ),
        Err(Rejection::VersionStale)
    );
    w.decide(
        1,
        TaskInput::RequestMerge {
            expected_version: version,
            reason: None,
        },
    );
    assert_eq!(w.place(1).2, Some(StagePhase::Merging));
    let merge = w.jobs.iter().find(|j| j.kind == JobKind::ScmMerge).unwrap();
    assert_eq!(merge.payload["head_sha"], json!(sha('b')));
    w.jobs.clear();

    w.decide(
        1,
        TaskInput::MergeSucceeded {
            merge_commit_sha: sha('c'),
            merged_at: None,
        },
    );
    assert_eq!(
        w.place(1),
        (
            WorkItemState::InStage,
            Some(Stage::Sync),
            Some(StagePhase::Verifying)
        )
    );
    assert_eq!(w.take_jobs(), vec![JobKind::EnvVerifyMerged]);
    w.decide(
        1,
        TaskInput::MergeVerified {
            found: true,
            base_tip: None,
        },
    );
    assert_eq!(
        w.take_jobs(),
        vec![JobKind::EnvTeardown, JobKind::ScmDeleteBranch]
    );
    assert_eq!(w.admits(), Vec::<u32>::new(), "S2 waits until S1 is done");
    w.decide(
        1,
        TaskInput::EnvironmentTornDown {
            teardown_failures: Vec::new(),
        },
    );
    assert_eq!(w.place(1).0, WorkItemState::Done);

    assert_eq!(w.admits(), vec![2]);
    w.run_to_awaiting_ci(2);
    w.decide(
        2,
        TaskInput::ChangeObserved {
            state: ChangeState::Merged,
            head_sha: sha('b'),
            merge_commit_sha: Some(sha('d')),
            merged_at: None,
            checks: vec![],
        },
    );
    assert_eq!(
        w.place(2).2,
        Some(StagePhase::Verifying),
        "merged outside Saka goes to sync"
    );
    w.decide(
        2,
        TaskInput::MergeVerified {
            found: true,
            base_tip: Some(sha('d')),
        },
    );
    let run = w.task(2).stage_run.clone().unwrap();
    assert_eq!(
        (run.stage, run.head_sha.as_deref()),
        (Stage::Sync, Some(sha('d').as_str())),
        "a merge without proof is verified at its merge commit"
    );
    assert!(
        w.take_jobs().contains(&JobKind::ScmPollCommit),
        "the repo's gate is CI, so CI is read at the merge commit"
    );
    w.decide(
        2,
        TaskInput::CommitChecksObserved {
            head_sha: sha('d'),
            checks: vec![check("test", Some(CheckConclusion::Success))],
        },
    );
    assert_eq!(w.place(2).2, Some(StagePhase::TearingDown));
    w.decide(
        2,
        TaskInput::EnvironmentTornDown {
            teardown_failures: Vec::new(),
        },
    );
    assert_eq!(w.place(2).0, WorkItemState::Done);
    let ship: Vec<Action> = w
        .reconcile()
        .into_iter()
        .filter(|a| matches!(a, Action::ShipSpec(_)))
        .collect();
    assert_eq!(ship, vec![Action::ShipSpec(w.spec.id)]);
    let events = spec::ship(&w.spec, true).unwrap();
    w.spec = spec::evolve(Some(w.spec.clone()), &events[0], w.now).unwrap();
    assert_eq!(w.spec.state, SpecState::Shipped);
}

#[test]
fn approval_checks_revision_lint_and_state() {
    let mut w = World::new();
    let latest = RevisionView {
        id: w.spec.latest_revision_id,
        lint_ok: true,
        issues: json!([]),
        steps: vec![],
        parser_version: 2,
        priority: 0,
        depends_on: vec![],
    };
    let mut ctx = Ctx {
        now: w.now,
        ids: &mut w.ids,
    };
    let repo = RepoId(Uuid::nil());
    assert_eq!(
        spec::approve(
            &w.spec,
            &latest,
            approval(SpecRevisionId(Uuid::nil()), repo),
            &mut ctx
        ),
        Err(Rejection::RevisionStale)
    );
    let old_parser = RevisionView {
        parser_version: 1,
        ..latest.clone()
    };
    assert_eq!(
        spec::approve(
            &w.spec,
            &old_parser,
            approval(w.spec.latest_revision_id, repo),
            &mut ctx
        ),
        Err(Rejection::SpecReparseRequired {
            parser_version: 1,
            current_parser_version: 2
        })
    );
    let failing = RevisionView {
        lint_ok: false,
        issues: json!([{ "code": "steps_missing" }]),
        ..latest.clone()
    };
    assert!(matches!(
        spec::approve(
            &w.spec,
            &failing,
            approval(w.spec.latest_revision_id, repo),
            &mut ctx
        ),
        Err(Rejection::SpecNotReady { .. })
    ));
    let mut approved = w.spec.clone();
    approved.state = SpecState::Approved;
    approved.approved_revision_id = Some(w.spec.latest_revision_id);
    assert_eq!(
        spec::approve(
            &approved,
            &latest,
            approval(w.spec.latest_revision_id, repo),
            &mut ctx
        ),
        Err(Rejection::SpecAlreadyApproved)
    );
}

#[test]
fn same_content_records_nothing_and_old_content_reuses_its_revision() {
    let w = World::new();
    let mut ids = Counter(100);
    let mut ctx = Ctx {
        now: w.now,
        ids: &mut ids,
    };
    let revision = || NewRevision {
        origin: saka_core::vocab::SpecOrigin::Manual,
        source_commit: "b".repeat(40),
        content_hash: "h".into(),
        context_hash: "sha256:none".into(),
        context: Vec::new(),
        lint_ok: false,
        issue_codes: vec!["steps_missing".into()],
        title: "T".into(),
        parser_version: 2,
        priority: 0,
    };
    let unchanged = spec::record_revision(
        Some(&w.spec),
        w.spec.project_id,
        &w.spec.path,
        &w.spec.slug,
        revision(),
        Some(w.spec.latest_revision_id),
        &mut ctx,
    );
    assert!(unchanged.is_empty());
    let earlier = SpecRevisionId(Uuid::from_u128(42));
    let events = spec::record_revision(
        Some(&w.spec),
        w.spec.project_id,
        &w.spec.path,
        &w.spec.slug,
        revision(),
        Some(earlier),
        &mut ctx,
    );
    assert!(
        matches!(&events[0], Event::SpecRevisionRecorded { revision_id, reused: true, .. } if *revision_id == earlier)
    );
    let s = spec::evolve(Some(w.spec.clone()), &events[0], w.now).unwrap();
    assert_eq!((s.latest_revision_id, s.state), (earlier, SpecState::Draft));
}

#[test]
fn red_ci_blocks_with_failing_checks_and_retry_returns_to_the_session() {
    let mut w = World::new();
    w.approve();
    w.run_to_awaiting_ci(1);
    w.decide(
        1,
        observed(
            'b',
            vec![
                check("lint", Some(CheckConclusion::Success)),
                check("test", Some(CheckConclusion::Failure)),
            ],
        ),
    );
    let t = w.task(1);
    assert_eq!(t.item.state, WorkItemState::Blocked);
    let blocker = t.open_blocker().unwrap();
    assert_eq!(blocker.code, BlockerCode::CiRed);
    assert_eq!(blocker.params["checks"], json!(["test"]));
    assert_eq!(t.stage_run.as_ref().unwrap().state, StageRunState::Blocked);

    w.decide(
        1,
        TaskInput::Retry {
            reason: "fix the test".into(),
        },
    );
    let t = w.task(1);
    assert!(t.open_blocker().is_none());
    assert_eq!(
        w.place(1),
        (
            WorkItemState::InStage,
            Some(Stage::Dev),
            Some(StagePhase::RunningSession)
        )
    );
    let run = t.stage_run.as_ref().unwrap();
    assert_eq!(
        (run.stage, run.attempt, run.trigger),
        (Stage::Dev, 2, Trigger::Manual)
    );
    assert_eq!(
        w.take_jobs(),
        vec![JobKind::ScmPollChangeRequest, JobKind::SessionStart]
    );
    let launch = TaskInput::Session(SessionInput::Launched {
        session_id: SessionId(Uuid::from_u128(650)),
        cli: "claude".into(),
        cli_version: None,
        model: None,
        account: "default".into(),
        runner_unit: "u".into(),
        socket_path: "s".into(),
        log_path: "l".into(),
        author: saka_core::authorship::SessionAuthor::saka(),
    });
    let events = w.decide(1, launch);
    assert!(
        matches!(&events[1], Event::SessionTurnRequested { prompt: Prompt::WorkPacket { retry_of: Some(Coded { code, .. }) }, .. } if code == "ci_red")
    );

    w.decide(
        1,
        TaskInput::Session(SessionInput::TurnAccepted { turn: 1 }),
    );
    w.decide(1, turn_ended(1, "success"));
    w.decide(
        1,
        TaskInput::WorktreeInspected {
            turn: 1,
            dirty: false,
            commits_ahead: 2,
            head_sha: sha('e'),
            outside: None,
        },
    );
    w.jobs.clear();
    let events = w.decide(1, TaskInput::BranchPushed { sha: sha('e') });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::ChangeRequestUpdated { .. }))
    );
    assert!(
        !w.jobs
            .iter()
            .any(|j| j.kind == JobKind::ScmOpenChangeRequest)
    );
    assert_eq!(w.place(1).2, Some(StagePhase::AwaitingCi));
    assert_eq!(w.task(1).attempts.get(&Stage::Merge), Some(&2));
}

#[test]
fn no_checks_within_the_timeout_blocks_as_ci_red() {
    let mut w = World::new();
    w.approve();
    w.run_to_awaiting_ci(1);
    w.now = w.now.plus_ms(cfg().ci_appear_timeout_ms + 1);
    assert!(
        w.reconcile()
            .iter()
            .any(|a| matches!(a, Action::Decide(_, TaskInput::Tick)))
    );
    w.decide(1, TaskInput::Tick);
    let b = w.task(1).open_blocker().unwrap();
    assert_eq!(
        (b.code, b.params["reason"].clone()),
        (BlockerCode::CiRed, json!("no_checks"))
    );
}

#[test]
fn running_time_excludes_waiting_for_a_person() {
    let mut w = World::new();
    w.approve();
    w.decide(1, TaskInput::Admit);
    w.decide(1, prepared(1));
    w.decide(1, launched(1));
    w.decide(
        1,
        TaskInput::Session(SessionInput::TurnAccepted { turn: 1 }),
    );
    w.now = w.now.plus_ms(10 * 60_000);
    w.decide(1, ask("r", 801));
    w.now = w.now.plus_ms(5 * 60 * 60_000);
    assert!(w.decide(1, TaskInput::Tick).is_empty());
    let id = PermissionRequestId(Uuid::from_u128(801));
    w.decide(
        1,
        TaskInput::Session(SessionInput::AnswerPermission {
            permission_request_id: id,
            allow: false,
            message: None,
            actor: "operator:local".into(),
            remember: false,
            active_rules: Vec::new(),
        }),
    );
    assert_eq!(
        w.task(1).permissions[0].message.as_deref(),
        Some("Denied by the operator.")
    );
    w.now = w.now.plus_ms(50 * 60_000);
    w.jobs.clear();
    w.decide(1, TaskInput::Tick);
    let t = w.task(1);
    assert_eq!(
        t.open_blocker().map(|b| (b.code, b.params.clone())),
        Some((
            BlockerCode::BudgetExhausted,
            json!({ "dimension": "running_minutes", "used": 60, "limit": 60 })
        ))
    );
    assert_eq!(t.session.as_ref().unwrap().state, SessionState::Failed);
    assert_eq!(
        t.session.as_ref().unwrap().interrupt_requested,
        Some(InterruptBy::Saka)
    );
    assert_eq!(
        w.take_jobs(),
        vec![JobKind::SessionInterrupt, JobKind::SessionStop]
    );
}

#[test]
fn server_restart_loses_the_session_expires_its_requests_and_resumes_it() {
    let mut w = World::new();
    w.approve();
    w.decide(1, TaskInput::Admit);
    w.decide(1, prepared(1));
    w.decide(1, launched(1));
    w.decide(
        1,
        TaskInput::Session(SessionInput::TurnAccepted { turn: 1 }),
    );
    w.decide(1, ask("r", 802));
    w.jobs.clear();
    w.decide(1, TaskInput::Session(SessionInput::ServerRestarted));
    let t = w.task(1);
    assert_eq!(t.session.as_ref().unwrap().state, SessionState::Lost);
    assert_eq!(t.permissions[0].state, PermissionState::Expired);
    assert_eq!(
        t.permissions[0].answer_source.as_deref(),
        Some("system:session_ended")
    );
    assert!(t.open_blocker().is_none());
    assert_eq!(w.take_jobs(), vec![JobKind::SessionStop]);
    let start: Vec<Job> = w
        .reconcile()
        .into_iter()
        .filter_map(|a| match a {
            Action::Enqueue(j) if j.kind == JobKind::SessionStart => Some(j),
            _ => None,
        })
        .collect();
    let t = w.task(1);
    let run = t.stage_run.as_ref().unwrap().id;
    let lost = t.session.as_ref().unwrap().id;
    assert_eq!(start.len(), 1);
    assert_eq!(
        start[0].idempotency_key,
        format!("session.start:{run}:{lost}")
    );
    assert_eq!(start[0].payload["resume"]["cause"], json!("lost"));
    assert_eq!(
        w.try_decide(
            1,
            TaskInput::Session(SessionInput::AnswerPermission {
                permission_request_id: PermissionRequestId(Uuid::from_u128(802)),
                allow: true,
                message: None,
                actor: "x".into(),
                remember: false,
                active_rules: Vec::new(),
            })
        ),
        Err(Rejection::PermissionAlreadyAnswered)
    );
}

#[test]
fn failed_turns_runner_exits_and_empty_work_block_with_their_codes() {
    let cases: Vec<(Vec<TaskInput>, BlockerCode)> = vec![
        (
            vec![turn_ended(1, "error_during_execution")],
            BlockerCode::SessionFailed,
        ),
        (
            vec![
                TaskInput::Session(SessionInput::CliExited {
                    code: Some(1),
                    signal: None,
                }),
                launched_again(1),
                TaskInput::Session(SessionInput::CliExited {
                    code: Some(1),
                    signal: None,
                }),
            ],
            BlockerCode::SessionLost,
        ),
        (
            vec![
                turn_ended(1, "success"),
                inspected(1, false, 0),
                TaskInput::Session(SessionInput::TurnAccepted { turn: 2 }),
                turn_ended(2, "success"),
                inspected(2, true, 0),
            ],
            BlockerCode::NoChanges,
        ),
    ];
    for (inputs, code) in cases {
        let mut w = World::new();
        w.approve();
        w.decide(1, TaskInput::Admit);
        w.decide(1, prepared(1));
        w.decide(1, launched(1));
        w.decide(
            1,
            TaskInput::Session(SessionInput::TurnAccepted { turn: 1 }),
        );
        for input in inputs {
            w.decide(1, input);
        }
        assert_eq!(w.task(1).open_blocker().map(|b| b.code), Some(code));
        assert!(w.task(1).live_session().is_none());
    }
}

#[test]
fn push_rejection_merge_conflict_closed_change_and_missing_merge() {
    let mut w = World::new();
    w.approve();
    w.decide(1, TaskInput::Admit);
    w.decide(1, prepared(1));
    w.decide(1, launched(1));
    w.decide(
        1,
        TaskInput::Session(SessionInput::TurnAccepted { turn: 1 }),
    );
    w.decide(1, turn_ended(1, "success"));
    w.decide(1, inspected(1, false, 1));
    w.decide(
        1,
        TaskInput::PushRejected {
            remote_sha: sha('f'),
            local_sha: sha('b'),
        },
    );
    assert_eq!(
        w.task(1).open_blocker().map(|b| b.code),
        Some(BlockerCode::PushRejected)
    );
    w.decide(
        1,
        TaskInput::Retry {
            reason: "remote branch removed".into(),
        },
    );
    assert_eq!(w.place(1).2, Some(StagePhase::Pushing));

    let mut w = World::new();
    w.approve();
    w.run_to_awaiting_ci(1);
    w.decide(
        1,
        observed('b', vec![check("t", Some(CheckConclusion::Success))]),
    );
    let version = w.task(1).item.version;
    w.decide(
        1,
        TaskInput::RequestMerge {
            expected_version: version,
            reason: None,
        },
    );
    w.decide(
        1,
        TaskInput::MergeRejected {
            reason: "head_changed".into(),
        },
    );
    let b = w.task(1).open_blocker().unwrap().clone();
    assert_eq!(
        (b.code, b.params["reason"].clone()),
        (BlockerCode::MergeConflict, json!("head_changed"))
    );
    w.decide(
        1,
        TaskInput::Retry {
            reason: "re-check".into(),
        },
    );
    assert_eq!(
        w.place(1),
        (
            WorkItemState::InStage,
            Some(Stage::Merge),
            Some(StagePhase::AwaitingCi)
        )
    );

    w.decide(
        1,
        TaskInput::ChangeObserved {
            state: ChangeState::Closed,
            head_sha: sha('b'),
            merge_commit_sha: None,
            merged_at: None,
            checks: vec![],
        },
    );
    assert_eq!(
        w.task(1).open_blocker().map(|b| b.code),
        Some(BlockerCode::ChangeClosed)
    );
    assert!(matches!(
        w.try_decide(1, TaskInput::Retry { reason: "x".into() }),
        Err(Rejection::InvalidState { .. })
    ));
    w.decide(
        1,
        TaskInput::Cancel {
            reason: "closed on purpose".into(),
        },
    );
    assert_eq!(w.place(1).0, WorkItemState::Cancelled);
    assert!(w.take_jobs().contains(&JobKind::EnvTeardown));
    assert!(matches!(
        w.try_decide(
            1,
            TaskInput::Cancel {
                reason: "again".into()
            }
        ),
        Err(Rejection::InvalidState { .. })
    ));
    assert_eq!(w.admits(), Vec::<u32>::new());

    let mut w = World::new();
    w.approve();
    w.run_to_awaiting_ci(1);
    w.decide(
        1,
        TaskInput::ChangeObserved {
            state: ChangeState::Merged,
            head_sha: sha('b'),
            merge_commit_sha: Some(sha('c')),
            merged_at: None,
            checks: vec![],
        },
    );
    w.decide(
        1,
        TaskInput::MergeVerified {
            found: false,
            base_tip: None,
        },
    );
    assert_eq!(
        w.task(1).open_blocker().map(|b| b.code),
        Some(BlockerCode::DodFailed)
    );
}

#[test]
fn exhausted_jobs_leave_the_mapped_blocker() {
    let mut w = World::new();
    w.approve();
    w.decide(1, TaskInput::Admit);
    w.jobs.clear();
    w.decide(
        1,
        TaskInput::JobExhausted {
            kind: JobKind::EnvPrepare,
            error: Coded {
                code: "git_failed".into(),
                params: json!({}),
            },
        },
    );
    let b = w.task(1).open_blocker().unwrap();
    assert_eq!(b.code, BlockerCode::EnvSetupFailed);
    assert_eq!(b.params["error"]["code"], json!("git_failed"));
    w.decide(
        1,
        TaskInput::Retry {
            reason: "disk freed".into(),
        },
    );
    assert_eq!(w.place(1).0, WorkItemState::Admitted);
    assert_eq!(w.take_jobs(), vec![JobKind::EnvPrepare]);
    assert!(
        w.decide(
            1,
            TaskInput::JobExhausted {
                kind: JobKind::EnvTeardown,
                error: Coded {
                    code: "x".into(),
                    params: json!({})
                }
            }
        )
        .is_empty()
    );
}

#[test]
fn reconcile_recreates_the_job_of_the_current_phase() {
    let mut w = World::new();
    w.approve();
    w.run_to_awaiting_ci(1);
    let polls: Vec<String> = w
        .reconcile()
        .into_iter()
        .filter_map(|a| match a {
            Action::Enqueue(j) if j.kind == JobKind::ScmPollChangeRequest => {
                Some(j.idempotency_key)
            }
            _ => None,
        })
        .collect();
    assert_eq!(polls.len(), 1);
    assert!(polls[0].starts_with("scm.poll:"));
    w.now = w.now.plus_ms(1);
    let again: Vec<String> = w
        .reconcile()
        .into_iter()
        .filter_map(|a| match a {
            Action::Enqueue(j) if j.kind == JobKind::ScmPollChangeRequest => {
                Some(j.idempotency_key)
            }
            _ => None,
        })
        .collect();
    assert_eq!(again, polls);
}

#[test]
fn saka_verify_read_back_at_the_merge_commit_is_not_ci() {
    let mut w = World::new();
    w.approve();
    w.run_to_awaiting_ci(1);
    w.decide(
        1,
        observed('b', vec![check("test", Some(CheckConclusion::Failure))]),
    );
    w.decide(
        1,
        TaskInput::ChangeObserved {
            state: ChangeState::Merged,
            head_sha: sha('b'),
            merge_commit_sha: Some(sha('m')),
            merged_at: None,
            checks: vec![],
        },
    );
    w.decide(
        1,
        TaskInput::MergeVerified {
            found: true,
            base_tip: Some(sha('m')),
        },
    );
    let ours = Check {
        name: saka_core::gate::SAKA_STATUS_CONTEXT.to_string(),
        kind: CheckKind::CommitStatus,
        status: CheckStatus::Completed,
        conclusion: Some(CheckConclusion::Success),
        url: None,
    };
    w.decide(
        1,
        TaskInput::CommitChecksObserved {
            head_sha: sha('m'),
            checks: vec![ours],
        },
    );
    assert_eq!(w.place(1).2, Some(StagePhase::Verifying));
    w.now = w.now.plus_ms(cfg().ci_appear_timeout_ms + 1);
    w.decide(1, TaskInput::Tick);
    let t = w.task(1);
    let b = t.open_blocker().unwrap();
    assert_eq!(b.code, BlockerCode::DodFailed);
    assert_eq!(b.params["reason"], "no_checks");
    assert_eq!(b.params["head_sha"], json!(sha('m')));
}

fn polls(w: &World) -> usize {
    w.reconcile()
        .into_iter()
        .filter(|a| matches!(a, Action::Enqueue(j) if j.kind == JobKind::ScmPollChangeRequest))
        .count()
}

#[test]
fn an_open_change_is_polled_in_every_state_until_sync() {
    let mut w = World::new();
    w.approve();
    w.run_to_awaiting_ci(1);
    assert_eq!(polls(&w), 1, "merge/awaiting_ci");

    w.decide(
        1,
        observed('b', vec![check("test", Some(CheckConclusion::Failure))]),
    );
    assert_eq!(w.task(1).item.state, WorkItemState::Blocked);
    assert_eq!(polls(&w), 1, "blocked");

    w.decide(
        1,
        TaskInput::Retry {
            reason: "fix the test".into(),
        },
    );
    assert_eq!(w.place(1).1, Some(Stage::Dev));
    assert_eq!(polls(&w), 1, "dev/running_session");

    w.decide(
        1,
        TaskInput::ChangeObserved {
            state: ChangeState::Merged,
            head_sha: sha('b'),
            merge_commit_sha: Some(sha('m')),
            merged_at: None,
            checks: vec![],
        },
    );
    assert_eq!(
        w.place(1),
        (
            WorkItemState::InStage,
            Some(Stage::Sync),
            Some(StagePhase::Verifying)
        )
    );
    assert_eq!(polls(&w), 0, "sync");
}

#[test]
fn a_change_being_merged_by_saka_is_not_polled() {
    let mut w = World::new();
    w.approve();
    w.run_to_awaiting_ci(1);
    w.decide(
        1,
        observed('b', vec![check("test", Some(CheckConclusion::Success))]),
    );
    assert_eq!(w.place(1).2, Some(StagePhase::AwaitingMerge));
    assert_eq!(polls(&w), 1, "merge/awaiting_merge");
    let version = w.task(1).item.version;
    w.decide(
        1,
        TaskInput::RequestMerge {
            expected_version: version,
            reason: None,
        },
    );
    assert_eq!(w.place(1).2, Some(StagePhase::Merging));
    assert_eq!(polls(&w), 0);
}

#[test]
fn a_cancelled_task_is_not_polled() {
    let mut w = World::new();
    w.approve();
    w.run_to_awaiting_ci(1);
    w.decide(
        1,
        TaskInput::Cancel {
            reason: "not needed".into(),
        },
    );
    assert_eq!(polls(&w), 0);
}

#[test]
fn an_exhausted_teardown_still_finishes_the_merged_task_and_the_next_step_starts() {
    let mut w = World::new();
    w.approve();
    w.run_to_tearing_down(1);
    let env = w.task(1).environment.as_ref().unwrap().id;

    let events = w.decide(1, teardown_exhausted());
    assert!(
        matches!(
            &events[..],
            [
                Event::EnvironmentTeardownFailed { environment_id, error, .. },
                Event::StageRunSucceeded { .. },
                Event::WorkItemDone { merge_commit_sha, .. },
            ] if *environment_id == env
                && error.code == "env_io_failed"
                && error.params["op"] == json!("remove_env_root")
                && *merge_commit_sha == sha('c')
        ),
        "{events:?}"
    );
    assert_eq!(w.place(1).0, WorkItemState::Done);
    assert!(w.task(1).open_blocker().is_none());
    assert_eq!(w.environment_state(1), EnvironmentState::TeardownFailed);
    assert_eq!(w.admits(), vec![2]);

    assert!(w.decide(1, teardown_exhausted()).is_empty());
    assert_eq!(w.environment_state(1), EnvironmentState::TeardownFailed);
}

#[test]
fn a_leftover_environment_gets_one_cleanup_job_per_hour_until_it_is_torn_down() {
    let mut w = World::new();
    w.approve();
    w.run_to_tearing_down(1);
    w.decide(1, teardown_exhausted());
    let env = w.task(1).environment.as_ref().unwrap().id;
    let owner = w.task(1).environment.as_ref().unwrap().owner;

    w.now = Timestamp(3 * 3_600_000 + 10);
    let first = w.teardown_jobs();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].idempotency_key, format!("env.teardown:{env}:3"));
    assert_eq!(
        first[0].payload,
        json!({ "owner": owner, "environment_id": env })
    );
    w.now = Timestamp(4 * 3_600_000 - 1);
    let same_hour = w.teardown_jobs();
    assert_eq!(same_hour.len(), 1);
    assert_eq!(same_hour[0].idempotency_key, first[0].idempotency_key);
    w.now = Timestamp(4 * 3_600_000);
    let next = w.teardown_jobs();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].idempotency_key, format!("env.teardown:{env}:4"));

    let events = w.decide(
        1,
        TaskInput::EnvironmentTornDown {
            teardown_failures: Vec::new(),
        },
    );
    assert!(
        matches!(&events[..], [Event::EnvironmentTornDown { environment_id, .. }] if *environment_id == env),
        "{events:?}"
    );
    assert_eq!(w.place(1).0, WorkItemState::Done);
    assert_eq!(w.environment_state(1), EnvironmentState::TornDown);
    assert!(w.teardown_jobs().is_empty());
}

#[test]
fn an_exhausted_teardown_of_a_cancelled_task_only_marks_the_environment() {
    let mut w = World::new();
    w.approve();
    w.decide(1, TaskInput::Admit);
    w.decide(1, prepared(1));
    w.decide(
        1,
        TaskInput::Cancel {
            reason: "not needed".into(),
        },
    );
    let env = w.task(1).environment.as_ref().unwrap().id;
    assert_eq!(w.environment_state(1), EnvironmentState::Ready);
    let keys: Vec<String> = w
        .teardown_jobs()
        .into_iter()
        .map(|j| j.idempotency_key)
        .collect();
    assert_eq!(keys, vec![format!("env.teardown:{env}")]);

    let events = w.decide(1, teardown_exhausted());
    assert!(
        matches!(&events[..], [Event::EnvironmentTeardownFailed { environment_id, .. }] if *environment_id == env),
        "{events:?}"
    );
    assert_eq!(w.place(1).0, WorkItemState::Cancelled);
    assert_eq!(w.environment_state(1), EnvironmentState::TeardownFailed);
    let hour = w.now.0 / 3_600_000;
    let keys: Vec<String> = w
        .teardown_jobs()
        .into_iter()
        .map(|j| j.idempotency_key)
        .collect();
    assert_eq!(keys, vec![format!("env.teardown:{env}:{hour}")]);
}
