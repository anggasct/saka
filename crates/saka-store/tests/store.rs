#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail loudly on a broken setup"
)]

use saka_core::Ctx;
use saka_core::event::Event;
use saka_core::gate::Check;
use saka_core::ids::{
    ChangeRequestId, EnvironmentId, IdSource, PermissionRequestId, ProjectId, RepoId, SessionId,
    SpecRevisionId, WorkItemId,
};
use saka_core::job::{Job, JobKind};
use saka_core::model::{Coded, Project, ProjectRepo, Repo, RevisionView, Spec, StepPlan, Task};
use saka_core::policy::{Decision, DecisionSource, Effect};
use saka_core::react::react;
use saka_core::session::SessionInput;
use saka_core::spec::{self, NewRevision};
use saka_core::task::{self, LoopConfig, TaskInput};
use saka_core::time::Timestamp;
use saka_core::vocab::{
    BlockerCode, ChangeState, CheckConclusion, CheckKind, CheckStatus, EnvironmentState,
    MergeMethod, SpecState, WorkItemState,
};
use saka_core::workspace;
use saka_store::read::{self, TaskSet};
use saka_store::{
    ActionRecord, Batch, EventRecord, JobOutcome, NewJob, SpecRevisionRow, Store, StoreError,
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

fn record(e: &Event, project: ProjectId, ids: &mut Counter, now: Timestamp) -> EventRecord {
    let stream = e.stream();
    EventRecord {
        id: ids.next_id(),
        kind: e.kind(),
        version: Event::VERSION,
        stream_type: stream.kind().to_string(),
        stream_id: stream.id(),
        project_id: Some(project),
        actor: "system:engine".into(),
        causation_id: None,
        correlation_id: Uuid::from_u128(1),
        occurred_at: now,
        payload: e.payload(),
    }
}

struct World {
    _dir: tempfile::TempDir,
    store: Store,
    ids: Counter,
    now: Timestamp,
    project: Project,
    spec: Spec,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let now = Timestamp(1_000_000);
        let store = Store::open(
            &dir.path().join("saka.db"),
            &dir.path().join("backups"),
            now,
        )
        .unwrap();
        let mut ids = Counter(1000);
        let repo_id = RepoId(ids.next_id());
        let project_id = ProjectId(ids.next_id());
        let repo_event = Event::RepoRegistered {
            repo_id,
            integration_id: "github".into(),
            provider: "github".into(),
            full_name: "acme/app".into(),
            url: "https://github.com/acme/app".into(),
            default_branch: "main".into(),
        };
        let project_event = Event::ProjectRegistered {
            project_id,
            name: "app".into(),
            workspace_repo_id: repo_id,
            project_path: "docs".into(),
            base_branch: "main".into(),
            config_commit: "a".repeat(40),
            config_sha256: format!("sha256:{}", "0".repeat(64)),
            workflow: saka_core::workflow::Workflow::default(),
            repos: vec![ProjectRepo {
                repo_id,
                name: "app".into(),
                is_default: true,
                base_branch: "main".into(),
                path_scope: vec!["**".into()],
            }],
        };
        let repo: Repo = workspace::evolve_repo(None, &repo_event, now).unwrap();
        let project = workspace::evolve_project(None, &project_event, now).unwrap();

        let revision = NewRevision {
            origin: saka_core::vocab::SpecOrigin::Manual,
            source_commit: "a".repeat(40),
            content_hash: "sha256:abc".into(),
            context_hash: "sha256:none".into(),
            context: Vec::new(),
            lint_ok: true,
            issue_codes: vec![],
            title: "Mark tasks as done".into(),
            parser_version: 2,
            priority: 0,
        };
        let mut ctx = Ctx { now, ids: &mut ids };
        let spec_events = spec::record_revision(
            None,
            project_id,
            "docs/specs/tasks-done.md",
            "tasks-done",
            revision,
            None,
            &mut ctx,
        );
        let spec = spec_events
            .iter()
            .fold(None, |s, e| spec::evolve(s, e, now))
            .unwrap();

        let mut batch = Batch::new(now);
        for e in [&repo_event, &project_event]
            .into_iter()
            .chain(&spec_events)
        {
            batch.events.push(record(e, project_id, &mut ids, now));
        }
        batch.repos.push(repo);
        batch.projects.push(project.clone());
        batch.specs.push(spec.clone());
        batch.revisions.push(SpecRevisionRow {
            id: spec.latest_revision_id.0,
            spec_id: spec.id.0,
            project_id,
            source_commit: "a".repeat(40),
            content_hash: "sha256:abc".into(),
            context_hash: "sha256:none".into(),
            context: Vec::new(),
            content: b"---\nsaka: spec\n---\n".to_vec(),
            lint_ok: true,
            lint_issues: json!([]),
            model: json!({ "steps": [
                { "id": "S1", "index": 1, "work": "Add done", "ac": ["AC-01"], "commands": ["go test ./..."], "after": [], "line": 3 },
                { "id": "S2", "index": 2, "work": "Show done", "ac": ["AC-01"], "commands": ["npm test"], "after": ["S1"], "line": 4 },
            ]}),
            parser_version: 2,
            created_at: now,
        });
        store.commit(&batch).unwrap();
        World {
            _dir: dir,
            store,
            ids,
            now,
            project,
            spec,
        }
    }

    fn approve(&mut self) -> Vec<Task> {
        let view = self
            .store
            .read(|c| read::revision_view(c, self.spec.latest_revision_id))
            .unwrap()
            .unwrap();
        assert_eq!(
            view.steps.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["S1", "S2"]
        );
        let mut ctx = Ctx {
            now: self.now,
            ids: &mut self.ids,
        };
        let events = spec::approve(
            &self.spec,
            &view,
            spec::Approval {
                if_match: self.spec.latest_revision_id,
                reason: None,
                repos: &self.project.repos,
                parser_version: 2,
                depends_on_line: 1,
                project_specs: &[],
                project_graph: &[],
                approved: None,
                tasks: &[],
            },
            &mut ctx,
        )
        .unwrap();
        let (spec_rows, item_rows) = spec::approval_graph(&self.spec, &view, &events, &[]);
        let mut tasks = Vec::new();
        for e in &events {
            self.spec = spec::evolve(Some(self.spec.clone()), e, self.now).unwrap();
            if let Some(t) = task::evolve(None, e, self.now) {
                tasks.push(t);
            }
        }
        let mut batch = Batch::new(self.now);
        for e in &events {
            batch
                .events
                .push(record(e, self.project.id, &mut self.ids, self.now));
        }
        batch.specs.push(self.spec.clone());
        batch.tasks = tasks.clone();
        batch.spec_dependencies = spec_rows;
        batch.work_item_dependencies = item_rows;
        self.store.commit(&batch).unwrap();
        tasks
    }

    fn run_environment(&self, id: WorkItemId) -> EnvironmentId {
        let task = self
            .store
            .read(|c| read::load_task(c, id))
            .unwrap()
            .unwrap();
        EnvironmentId(task.stage_run.unwrap().id.0)
    }

    fn step(&mut self, id: WorkItemId, input: TaskInput) -> Vec<Event> {
        let before = self
            .store
            .read(|c| read::load_task(c, id))
            .unwrap()
            .unwrap();
        let mut ctx = Ctx {
            now: self.now,
            ids: &mut self.ids,
        };
        let events = task::decide(&before, input, &cfg(), &mut ctx).unwrap();
        let mut after = before;
        let mut batch = Batch::new(self.now);
        for e in &events {
            after = task::evolve(Some(after), e, self.now).unwrap();
            for job in react(&after, e, &cfg(), self.now) {
                batch.jobs.push(NewJob {
                    id: self.ids.next_id(),
                    job,
                    project_id: Some(self.project.id),
                    causation_id: None,
                    correlation_id: Uuid::from_u128(1),
                });
            }
            batch
                .events
                .push(record(e, self.project.id, &mut self.ids, self.now));
        }
        batch.tasks.push(after.clone());
        self.store.commit(&batch).unwrap();

        let stored = self
            .store
            .read(|c| read::load_task(c, id))
            .unwrap()
            .unwrap();
        let mut expected = after;
        expected.retired_runs.clear();
        expected.retired_sessions.clear();
        expected.usage_records.clear();
        expected.quota_observations.clear();
        expected.added_rules.clear();
        expected.permissions.retain(|p| {
            p.state == saka_core::vocab::PermissionState::Pending
                || p.delivered_at.is_none() && saka_core::session::awaits_delivery(p)
        });
        assert_eq!(stored, expected, "stored task differs after {events:?}");
        events
    }

    fn tick(&mut self, ms: i64) {
        self.now = self.now.plus_ms(ms);
    }

    fn run_to_tearing_down(&mut self, id: WorkItemId, n: u128) {
        self.step(id, TaskInput::Admit);
        let environment_id = self.run_environment(id);
        self.step(
            id,
            TaskInput::EnvironmentPrepared {
                environment_id,
                root_path: format!("/e{n}"),
                worktree_path: format!("/e{n}/w"),
                base_sha: sha('a'),
                setup: Default::default(),
            },
        );
        self.step(
            id,
            TaskInput::Session(SessionInput::Launched {
                session_id: SessionId(Uuid::from_u128(n + 1)),
                cli: "claude".into(),
                cli_version: None,
                model: None,
                account: "default".into(),
                runner_unit: format!("saka-session-{n}"),
                socket_path: format!("/run/{n}.sock"),
                log_path: format!("/data/{n}/events.ndjson"),
                author: saka_core::authorship::SessionAuthor::saka(),
            }),
        );
        self.step(
            id,
            TaskInput::Session(SessionInput::TurnAccepted { turn: 1 }),
        );
        self.step(
            id,
            TaskInput::Session(SessionInput::TurnEnded {
                turn: 1,
                status: "success".into(),
                cost_usd_micros: None,
                input_tokens: None,
                output_tokens: None,
            }),
        );
        self.step(
            id,
            TaskInput::WorktreeInspected {
                docs: None,
                turn: 1,
                dirty: false,
                commits_ahead: 1,
                head_sha: sha('b'),
                outside: None,
            },
        );
        self.step(
            id,
            TaskInput::BranchPushed {
                target: saka_core::vocab::ChangeRole::Code,
                sha: sha('b'),
            },
        );
        self.step(
            id,
            TaskInput::ChangeOpened {
                role: saka_core::vocab::ChangeRole::Code,
                change_request_id: ChangeRequestId(Uuid::from_u128(n + 2)),
                number: u64::try_from(n).unwrap(),
                url: format!("https://github.com/acme/app/pull/{n}"),
                head_sha: sha('b'),
                base_branch: "main".into(),
                opened_with: saka_core::vocab::ChangeCredentialKind::App,
            },
        );
        self.step(
            id,
            TaskInput::ChangeObserved {
                role: saka_core::vocab::ChangeRole::Code,
                state: ChangeState::Open,
                head_sha: sha('b'),
                merge_commit_sha: None,
                merged_at: None,
                checks: vec![Check {
                    name: "test".into(),
                    kind: CheckKind::CheckRun,
                    status: CheckStatus::Completed,
                    conclusion: Some(CheckConclusion::Success),
                    url: None,
                }],
            },
        );
        self.step(
            id,
            TaskInput::SakaGateObserved {
                head_sha: sha('b'),
                checks: vec![Check {
                    name: "make test".into(),
                    kind: CheckKind::Command,
                    ..Check {
                        name: "test".into(),
                        kind: CheckKind::CheckRun,
                        status: CheckStatus::Completed,
                        conclusion: Some(CheckConclusion::Success),
                        url: None,
                    }
                }],
                failure: None,
            },
        );
        self.step(
            id,
            TaskInput::ChangeObserved {
                role: saka_core::vocab::ChangeRole::Code,
                state: ChangeState::Merged,
                head_sha: sha('b'),
                merge_commit_sha: Some(sha('c')),
                merged_at: None,
                checks: vec![],
            },
        );
        self.step(
            id,
            TaskInput::MergeVerified {
                docs_found: None,
                found: true,
                base_tip: None,
            },
        );
    }
}

fn sha(c: char) -> String {
    c.to_string().repeat(40)
}

fn count(store: &Store, table: &str) -> i64 {
    store
        .read(|c| Ok(c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?))
        .unwrap()
}

#[test]
fn a_whole_task_round_trips_through_the_schema() {
    let mut w = World::new();
    let tasks = w.approve();
    let id = tasks[0].item.id;
    let session = SessionId(Uuid::from_u128(77));
    let permission = PermissionRequestId(Uuid::from_u128(78));

    w.step(id, TaskInput::Admit);
    w.tick(1000);
    w.step(
        id,
        TaskInput::EnvironmentPrepared {
            environment_id: w.run_environment(id),
            root_path: "/e".into(),
            worktree_path: "/e/w".into(),
            base_sha: sha('a'),
            setup: Default::default(),
        },
    );
    w.step(
        id,
        TaskInput::Session(SessionInput::Launched {
            session_id: session,
            cli: "claude".into(),
            cli_version: Some("2.0.0".into()),
            model: None,
            account: "default".into(),
            runner_unit: "saka-session-77".into(),
            socket_path: "/run/77.sock".into(),
            log_path: "/data/77/events.ndjson".into(),
            author: saka_core::authorship::SessionAuthor::saka(),
        }),
    );
    w.step(
        id,
        TaskInput::Session(SessionInput::Initialized {
            cli_session_id: "cli-1".into(),
            model: Some("sonnet".into()),
        }),
    );
    w.tick(1000);
    w.step(
        id,
        TaskInput::Session(SessionInput::TurnAccepted { turn: 1 }),
    );
    w.tick(5000);
    let stored = w.store.read(|c| read::load_task(c, id)).unwrap().unwrap();
    assert_eq!(stored.session.as_ref().unwrap().running_ms_at(w.now), 5000);

    w.step(
        id,
        TaskInput::Session(SessionInput::PermissionRequested {
            permission_request_id: permission,
            cli_request_id: "r1".into(),
            tool: "Bash".into(),
            summary: "npm install".into(),
            input: json!({ "command": "npm install" }),
            decision: Decision {
                effect: Effect::Ask,
                source: DecisionSource::Default,
                message: None,
            },
        }),
    );
    w.tick(60_000);
    w.step(
        id,
        TaskInput::Session(SessionInput::AnswerPermission {
            permission_request_id: permission,
            allow: true,
            message: None,
            actor: "operator:local".into(),
            remember: false,
            active_rules: Vec::new(),
        }),
    );
    w.step(
        id,
        TaskInput::Session(SessionInput::PermissionDelivered {
            permission_request_id: permission,
        }),
    );
    w.step(
        id,
        TaskInput::Session(SessionInput::TurnEnded {
            turn: 1,
            status: "success".into(),
            cost_usd_micros: Some(1500),
            input_tokens: Some(10),
            output_tokens: Some(20),
        }),
    );
    w.step(
        id,
        TaskInput::WorktreeInspected {
            docs: None,
            turn: 1,
            dirty: false,
            commits_ahead: 1,
            head_sha: sha('b'),
            outside: None,
        },
    );
    w.step(
        id,
        TaskInput::BranchPushed {
            target: saka_core::vocab::ChangeRole::Code,
            sha: sha('b'),
        },
    );
    w.step(
        id,
        TaskInput::ChangeOpened {
            role: saka_core::vocab::ChangeRole::Code,
            change_request_id: ChangeRequestId(Uuid::from_u128(79)),
            number: 12,
            url: "https://github.com/acme/app/pull/12".into(),
            head_sha: sha('b'),
            base_branch: "main".into(),
            opened_with: saka_core::vocab::ChangeCredentialKind::App,
        },
    );
    let checks = vec![Check {
        name: "test".into(),
        kind: CheckKind::CheckRun,
        status: CheckStatus::Completed,
        conclusion: Some(CheckConclusion::Success),
        url: Some("https://ci/1".into()),
    }];
    w.step(
        id,
        TaskInput::ChangeObserved {
            role: saka_core::vocab::ChangeRole::Code,
            state: ChangeState::Open,
            head_sha: sha('b'),
            merge_commit_sha: None,
            merged_at: None,
            checks,
        },
    );
    w.step(id, TaskInput::SakaGateStarted { head_sha: sha('b') });
    w.step(
        id,
        TaskInput::SakaGateObserved {
            head_sha: sha('b'),
            checks: vec![Check {
                name: "make test".into(),
                kind: CheckKind::Command,
                status: CheckStatus::Completed,
                conclusion: Some(CheckConclusion::Success),
                url: None,
            }],
            failure: None,
        },
    );
    let version = w
        .store
        .read(|c| read::load_task(c, id))
        .unwrap()
        .unwrap()
        .item
        .version;
    w.step(
        id,
        TaskInput::RequestMerge {
            expected_version: version,
            reason: None,
        },
    );
    w.step(
        id,
        TaskInput::MergeSucceeded {
            role: saka_core::vocab::ChangeRole::Code,
            merge_commit_sha: sha('c'),
            merged_at: Some(Timestamp(5)),
        },
    );
    w.step(
        id,
        TaskInput::MergeVerified {
            docs_found: None,
            found: true,
            base_tip: None,
        },
    );
    w.step(
        id,
        TaskInput::EnvironmentTornDown {
            teardown_failures: Vec::new(),
        },
    );

    let done = w.store.read(|c| read::load_task(c, id)).unwrap().unwrap();
    assert_eq!(done.item.state, WorkItemState::Done);
    assert_eq!(
        count(&w.store, "stage_run"),
        3,
        "dev, merge, and sync runs are all kept"
    );
    assert_eq!(count(&w.store, "ci_check"), 1);
    assert_eq!(count(&w.store, "gate_result"), 3);
    assert_eq!(count(&w.store, "ci_check"), 1);
    let pending_jobs: i64 = count(&w.store, "job");
    assert!(pending_jobs >= 10);

    let set = w
        .store
        .read(|c| read::load_tasks(c, TaskSet::Reconcile))
        .unwrap();
    assert_eq!(set.len(), 2);
    assert_eq!(w.store.read(read::reconcile_specs).unwrap().len(), 1);
}

#[test]
fn approve_writes_the_step_graph_with_the_work_items() {
    let mut w = World::new();
    let tasks = w.approve();
    let id = |n: u32| {
        tasks
            .iter()
            .find(|t| t.item.step_index == Some(n))
            .unwrap()
            .item
            .id
    };
    let queued = w.store.read(read::queued_work_item_dependencies).unwrap();
    let pairs: Vec<_> = queued
        .iter()
        .map(|d| (d.work_item_id, d.depends_on_work_item_id))
        .collect();
    assert_eq!(pairs, vec![(id(2), id(1))]);
    assert_eq!(
        w.store
            .read(|c| read::work_item_dependencies_of(c, &[id(2), id(1)]))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(count(&w.store, "spec_dependency"), 0);
}

#[test]
fn re_approval_repins_the_row_and_replaces_its_dependencies() {
    let mut w = World::new();
    let tasks = w.approve();
    let first = w.spec.clone();
    let v2 = SpecRevisionId(w.ids.next_id());
    let recorded = Event::SpecRevisionRecorded {
        spec_id: w.spec.id,
        revision_id: v2,
        source_commit: "b".repeat(40),
        content_hash: "sha256:def".into(),
        context_hash: "sha256:none".into(),
        context: Vec::new(),
        lint_ok: true,
        issue_codes: vec![],
        title: "Mark tasks as done".into(),
        reused: false,
        parser_version: 2,
        priority: 4,
    };
    w.spec = spec::evolve(Some(w.spec.clone()), &recorded, w.now).unwrap();
    let mut batch = Batch::new(w.now);
    batch
        .events
        .push(record(&recorded, w.project.id, &mut w.ids, w.now));
    batch.specs.push(w.spec.clone());
    batch.revisions.push(SpecRevisionRow {
        id: v2.0,
        spec_id: w.spec.id.0,
        project_id: w.project.id,
        source_commit: "b".repeat(40),
        content_hash: "sha256:def".into(),
        context_hash: "sha256:none".into(),
        context: Vec::new(),
        content: b"---\nsaka: spec\n---\n".to_vec(),
        lint_ok: true,
        lint_issues: json!([]),
        model: json!({ "priority": 4, "steps": [
            { "id": "S1", "index": 1, "work": "Add done", "ac": ["AC-01"], "commands": ["go test ./..."], "after": [], "line": 3 },
            { "id": "S2", "index": 2, "work": "Show done twice", "ac": ["AC-01"], "commands": ["npm test"], "after": [], "line": 4 },
        ]}),
        parser_version: 2,
        created_at: w.now,
    });
    w.store.commit(&batch).unwrap();

    let (from, to) = w
        .store
        .read(|c| {
            Ok((
                read::revision_view(c, first.latest_revision_id)?.unwrap(),
                read::revision_view(c, v2)?.unwrap(),
            ))
        })
        .unwrap();
    let items: Vec<_> = tasks.iter().map(|t| t.item.clone()).collect();
    let mut ctx = Ctx {
        now: w.now,
        ids: &mut w.ids,
    };
    let events = spec::approve(
        &w.spec,
        &to,
        spec::Approval {
            if_match: v2,
            reason: None,
            repos: &w.project.repos,
            parser_version: 2,
            depends_on_line: 1,
            project_specs: &[],
            project_graph: &[],
            approved: Some(&from),
            tasks: &items,
        },
        &mut ctx,
    )
    .unwrap();
    let (spec_rows, item_rows) = spec::approval_graph(&w.spec, &to, &events, &items);
    let mut batch = Batch::new(w.now);
    batch.spec_dependencies = spec_rows;
    batch.work_item_dependencies = item_rows;
    batch.replaced_spec_dependencies.push(w.spec.id);
    for e in &events {
        w.spec = spec::evolve(Some(w.spec.clone()), e, w.now).unwrap();
        if let Event::WorkItemRepinned { work_item_id, .. } = e {
            batch.replaced_work_item_dependencies.push(*work_item_id);
            let before = tasks.iter().find(|t| t.item.id == *work_item_id).cloned();
            batch.tasks.extend(task::evolve(before, e, w.now));
        }
        batch
            .events
            .push(record(e, w.project.id, &mut w.ids, w.now));
    }
    batch.specs.push(w.spec.clone());
    w.store.commit(&batch).unwrap();

    let s2 = tasks.iter().find(|t| t.item.step_index == Some(2)).unwrap();
    let stored = w
        .store
        .read(|c| read::load_task(c, s2.item.id))
        .unwrap()
        .unwrap();
    assert_eq!(stored.item.spec_revision_id, Some(v2));
    assert_eq!(stored.item.work, "Show done twice");
    assert_eq!(stored.item.priority, 4);
    assert_eq!(
        w.store
            .read(|c| read::work_item_dependencies_of(c, &[s2.item.id]))
            .unwrap(),
        vec![]
    );
    let spec_row = w
        .store
        .read(|c| read::load_spec(c, w.spec.id))
        .unwrap()
        .unwrap();
    assert_eq!(spec_row.approved_revision_id, Some(v2));
    assert_eq!(spec_row.approved_at, first.approved_at);
    assert_eq!(spec_row.priority, 4);
}

#[test]
fn blocked_retried_and_cancelled_tasks_round_trip() {
    let mut w = World::new();
    let tasks = w.approve();
    let id = tasks[0].item.id;
    w.step(id, TaskInput::Admit);
    w.step(
        id,
        TaskInput::JobExhausted {
            kind: JobKind::EnvPrepare,
            error: Coded {
                code: "git_failed".into(),
                params: json!({ "exit": 128 }),
            },
        },
    );
    let t = w.store.read(|c| read::load_task(c, id)).unwrap().unwrap();
    assert_eq!(t.open_blocker().unwrap().code, BlockerCode::EnvSetupFailed);
    w.step(
        id,
        TaskInput::Retry {
            reason: "disk freed".into(),
        },
    );
    w.step(
        id,
        TaskInput::Cancel {
            reason: "not needed".into(),
        },
    );
    let t = w.store.read(|c| read::load_task(c, id)).unwrap().unwrap();
    assert_eq!(t.item.state, WorkItemState::Cancelled);
    assert_eq!(
        read::attempts(&rusqlite_conn(&w), id).unwrap(),
        [(saka_core::vocab::Stage::Dev, 2)].into_iter().collect()
    );
}

fn rusqlite_conn(w: &World) -> rusqlite::Connection {
    rusqlite::Connection::open(w.store.path()).unwrap()
}

#[test]
fn a_failing_projection_write_leaves_nothing_behind() {
    let mut w = World::new();
    let tasks = w.approve();
    let events_before = count(&w.store, "event");
    let jobs_before = count(&w.store, "job");

    let mut broken = tasks[0].clone();
    broken.item.version = 0; // violates CHECK (version >= 1)
    let mut batch = Batch::new(w.now);
    batch.events.push(record(
        &Event::WorkItemAdmitted {
            work_item_id: broken.item.id,
        },
        w.project.id,
        &mut w.ids,
        w.now,
    ));
    batch.jobs.push(NewJob {
        id: w.ids.next_id(),
        job: Job::new(JobKind::EnvPrepare, "x", json!({}), w.now),
        project_id: Some(w.project.id),
        causation_id: None,
        correlation_id: Uuid::from_u128(1),
    });
    batch.tasks.push(broken);
    assert!(matches!(
        w.store.commit(&batch),
        Err(StoreError::Constraint(_))
    ));
    assert_eq!(count(&w.store, "event"), events_before);
    assert_eq!(count(&w.store, "job"), jobs_before);
    let t = w
        .store
        .read(|c| read::load_task(c, tasks[0].item.id))
        .unwrap()
        .unwrap();
    assert_eq!(t.item.version, 1);
}

fn enqueue(w: &mut World, key: &str) {
    let mut batch = Batch::new(w.now);
    batch.jobs.push(NewJob {
        id: w.ids.next_id(),
        job: Job::new(JobKind::EnvPrepare, key, json!({ "k": key }), w.now),
        project_id: Some(w.project.id),
        causation_id: None,
        correlation_id: Uuid::from_u128(1),
    });
    w.store.commit(&batch).unwrap();
}

#[test]
fn job_queue_leases_reclaims_retries_and_dies_with_its_blocker() {
    let mut w = World::new();
    let tasks = w.approve();
    enqueue(&mut w, "a");
    enqueue(&mut w, "a");
    assert_eq!(
        count(&w.store, "job"),
        1,
        "duplicate idempotency keys are ignored"
    );

    let first = w.store.claim_job("w1", w.now).unwrap().unwrap();
    assert_eq!(
        (
            first.attempts,
            first.max_attempts,
            first.idempotency_key.as_str()
        ),
        (1, 3, "env.prepare:a")
    );
    assert!(
        w.store.claim_job("w2", w.now).unwrap().is_none(),
        "a leased job is not claimed twice"
    );

    w.tick(saka_store::LEASE_MS + 1);
    let second = w.store.claim_job("w2", w.now).unwrap().unwrap();
    assert_eq!((second.id, second.attempts), (first.id, 2));
    let stale = Batch {
        job_outcome: Some(JobOutcome::Succeeded {
            id: first.id,
            owner: "w1".into(),
        }),
        ..Batch::new(w.now)
    };
    assert!(matches!(
        w.store.commit(&stale),
        Err(StoreError::LeaseLost { .. })
    ));
    assert!(matches!(
        w.store.extend_lease(first.id, "w1", w.now),
        Err(StoreError::LeaseLost { .. })
    ));
    w.store.extend_lease(second.id, "w2", w.now).unwrap();

    let run_at = second.backoff(w.now);
    assert_eq!(run_at, w.now.plus_ms(20_000));
    let retry = Batch {
        job_outcome: Some(JobOutcome::Retry {
            id: second.id,
            owner: "w2".into(),
            error: json!({ "code": "git_failed" }),
            run_at,
        }),
        ..Batch::new(w.now)
    };
    w.store.commit(&retry).unwrap();
    assert!(w.store.claim_job("w3", w.now).unwrap().is_none());
    w.now = run_at;
    let third = w.store.claim_job("w3", w.now).unwrap().unwrap();
    assert_eq!(third.attempts, 3);
    assert_eq!(third.last_error, Some(json!({ "code": "git_failed" })));

    let exhausted = Event::WorkItemAdmitted {
        work_item_id: tasks[0].item.id,
    };
    let mut dead = Batch::new(w.now);
    dead.job_outcome = Some(JobOutcome::Dead {
        id: third.id,
        owner: "w3".into(),
        error: json!({ "code": "git_failed" }),
    });
    let mut rec = record(&exhausted, w.project.id, &mut w.ids, w.now);
    rec.kind = "engine.job.exhausted".into();
    rec.stream_type = "job".into();
    rec.stream_id = third.id;
    rec.payload = json!({ "job_id": third.id, "kind": "env.prepare", "last_error": { "code": "git_failed", "params": {} } });
    dead.events.push(rec);
    let mut blocked = w
        .store
        .read(|c| read::load_task(c, tasks[0].item.id))
        .unwrap()
        .unwrap();
    let mut ctx = Ctx {
        now: w.now,
        ids: &mut w.ids,
    };
    for e in task::decide(&blocked.clone(), TaskInput::Admit, &cfg(), &mut ctx).unwrap() {
        blocked = task::evolve(Some(blocked), &e, w.now).unwrap();
    }
    for e in task::decide(
        &blocked.clone(),
        TaskInput::JobExhausted {
            kind: JobKind::EnvPrepare,
            error: Coded {
                code: "git_failed".into(),
                params: json!({}),
            },
        },
        &cfg(),
        &mut ctx,
    )
    .unwrap()
    {
        blocked = task::evolve(Some(blocked), &e, w.now).unwrap();
    }
    dead.tasks.push(blocked);
    w.store.commit(&dead).unwrap();
    let state: String = w
        .store
        .read(|c| {
            Ok(c.query_row(
                "SELECT state FROM job WHERE id = ?1",
                [third.id.to_string()],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(state, "dead");
    let kinds: Vec<String> = w
        .store
        .read(|c| read::events_after(c, 0, 1000))
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("engine.job.exhausted")
    );
    assert_eq!(count(&w.store, "blocker"), 1);
    assert!(
        w.store
            .claim_job("w4", w.now.plus_ms(10_000_000))
            .unwrap()
            .is_none()
    );
}

#[test]
fn events_stream_in_order_and_actions_replay() {
    let w = World::new();
    let all = w.store.read(|c| read::events_after(c, 0, 100)).unwrap();
    assert_eq!(
        all.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        vec![
            "workspace.repo.registered",
            "workspace.project.registered",
            "spec.spec.registered",
            "spec.revision.recorded",
        ]
    );
    let after = w
        .store
        .read(|c| read::events_after(c, all[1].seq, 100))
        .unwrap();
    assert_eq!(after.len(), 2);
    assert_eq!(after[0].seq, all[2].seq);

    let action = ActionRecord {
        id: Uuid::from_u128(5),
        actor: "operator:local".into(),
        idempotency_key: "key-1".into(),
        request_hash: "sha256:1".into(),
        action: "spec.approve".into(),
        target_type: "spec".into(),
        target_id: Some(w.spec.id.0),
        project_id: Some(w.project.id),
        reason: None,
        response_status: 200,
        response_body: json!({ "ok": true }),
        created_at: w.now,
    };
    let batch = Batch {
        action: Some(action.clone()),
        ..Batch::new(w.now)
    };
    w.store.commit(&batch).unwrap();
    assert_eq!(
        w.store
            .read(|c| read::find_action(c, "operator:local", "key-1"))
            .unwrap(),
        Some(action.clone())
    );
    assert!(
        matches!(w.store.commit(&batch), Err(StoreError::Constraint(_))),
        "a key is stored once"
    );
}

#[test]
fn reopening_applies_nothing_and_reads_the_same_rows() {
    let w = World::new();
    assert_eq!(
        w.store.migration().applied,
        vec![
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24
        ]
    );
    assert_eq!(w.store.schema_version().unwrap(), Some(24));
    let path = w.store.path().to_path_buf();
    let reopened = Store::open(&path, &path.with_file_name("backups"), w.now).unwrap();
    assert!(reopened.migration().applied.is_empty());
    assert!(reopened.migration().backup.is_none());
    let spec = reopened
        .read(|c| read::spec_by_path(c, w.project.id, "docs/specs/tasks-done.md"))
        .unwrap()
        .unwrap();
    assert_eq!(spec, w.spec);
    let project = reopened
        .read(|c| read::load_project(c, w.project.id))
        .unwrap()
        .unwrap();
    assert_eq!(project, w.project);
    let view: RevisionView = reopened
        .read(|c| read::revision_view(c, spec.latest_revision_id))
        .unwrap()
        .unwrap();
    assert_eq!(
        view.steps[1],
        StepPlan {
            id: "S2".into(),
            index: 2,
            work: "Show done".into(),
            ac: vec!["AC-01".into()],
            commands: vec!["npm test".into()],
            after: vec!["S1".into()],
            repo: None,
            line: 4,
        }
    );
    assert_eq!(
        reopened
            .read(|c| read::revision_by_hash(c, spec.id, "sha256:abc", "sha256:none", 2))
            .unwrap(),
        Some(spec.latest_revision_id)
    );
}

#[test]
fn connections_use_the_documented_settings() {
    let w = World::new();
    let (fk, mode, sync, query_only): (i64, String, i64, i64) = w
        .store
        .read(|c| {
            Ok((
                c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?,
                c.query_row("PRAGMA journal_mode", [], |r| r.get(0))?,
                c.query_row("PRAGMA synchronous", [], |r| r.get(0))?,
                c.query_row("PRAGMA query_only", [], |r| r.get(0))?,
            ))
        })
        .unwrap();
    assert_eq!((fk, mode.as_str(), sync, query_only), (1, "wal", 1, 1));
    let write = w.store.read(|c| Ok(c.execute("DELETE FROM event", [])?));
    assert!(write.is_err(), "readers cannot write");
}

#[test]
fn foreign_keys_are_still_enforced_at_commit() {
    let mut w = World::new();
    let tasks = w.approve();
    let events_before = count(&w.store, "event");
    let mut orphan = tasks[0].clone();
    orphan.item.id = WorkItemId(w.ids.next_id());
    orphan.item.task_key = "other/s1".into();
    orphan.item.branch = "saka/other/s1".into();
    orphan.item.project_id = ProjectId(Uuid::from_u128(424_242));
    let mut batch = Batch::new(w.now);
    batch.events.push(record(
        &Event::WorkItemAdmitted {
            work_item_id: orphan.item.id,
        },
        w.project.id,
        &mut w.ids,
        w.now,
    ));
    batch.tasks.push(orphan);
    assert!(matches!(
        w.store.commit(&batch),
        Err(StoreError::Constraint(_))
    ));
    assert_eq!(count(&w.store, "event"), events_before);
}

#[test]
fn reconcile_keeps_loading_shipped_tasks_whose_environment_still_needs_cleanup() {
    let mut w = World::new();
    let tasks = w.approve();
    let (first, second) = (tasks[0].item.id, tasks[1].item.id);
    w.run_to_tearing_down(first, 300);
    w.step(
        first,
        TaskInput::JobExhausted {
            kind: JobKind::EnvTeardown,
            error: Coded {
                code: "env_io_failed".into(),
                params: json!({ "op": "remove_env_root", "error": "Permission denied (os error 13)" }),
            },
        },
    );
    w.run_to_tearing_down(second, 400);
    w.step(
        second,
        TaskInput::EnvironmentTornDown {
            teardown_failures: Vec::new(),
        },
    );

    let shipped = spec::ship(&w.spec, true).unwrap();
    w.spec = spec::evolve(Some(w.spec.clone()), &shipped[0], w.now).unwrap();
    assert_eq!(w.spec.state, SpecState::Shipped);
    let mut batch = Batch::new(w.now);
    batch
        .events
        .push(record(&shipped[0], w.project.id, &mut w.ids, w.now));
    batch.specs.push(w.spec.clone());
    w.store.commit(&batch).unwrap();

    let set = w
        .store
        .read(|c| read::load_tasks(c, TaskSet::Reconcile))
        .unwrap();
    assert_eq!(
        set.iter().map(|t| t.item.id).collect::<Vec<_>>(),
        vec![first]
    );
    assert_eq!(set[0].item.state, WorkItemState::Done);
    assert_eq!(
        set[0].environment.as_ref().unwrap().state,
        EnvironmentState::TeardownFailed
    );
}

fn launched(n: u128) -> TaskInput {
    TaskInput::Session(SessionInput::Launched {
        session_id: SessionId(Uuid::from_u128(n)),
        cli: "claude".into(),
        cli_version: Some("2.1.0".into()),
        model: None,
        account: "default".into(),
        runner_unit: format!("saka-session-{n}"),
        socket_path: format!("/run/{n}.sock"),
        log_path: format!("/data/{n}/events.ndjson"),
        author: saka_core::authorship::SessionAuthor::saka(),
    })
}

#[test]
fn runtime_state_round_trips_resume_ledger_rules_and_quota() {
    let mut w = World::new();
    let tasks = w.approve();
    let id = tasks[0].item.id;
    w.step(id, TaskInput::Admit);
    w.step(
        id,
        TaskInput::EnvironmentPrepared {
            environment_id: w.run_environment(id),
            root_path: "/e".into(),
            worktree_path: "/e/w".into(),
            base_sha: "a".repeat(40),
            setup: Default::default(),
        },
    );
    w.step(id, launched(77));
    w.step(
        id,
        TaskInput::Session(SessionInput::Initialized {
            cli_session_id: "cli-1".into(),
            model: None,
        }),
    );
    w.step(
        id,
        TaskInput::Session(SessionInput::TurnAccepted { turn: 1 }),
    );
    let first = SessionId(Uuid::from_u128(77));
    w.tick(60_000);
    w.step(
        id,
        TaskInput::Session(SessionInput::Steer {
            session_id: first,
            text: "keep going".into(),
        }),
    );
    w.step(
        id,
        TaskInput::Session(SessionInput::PermissionRequested {
            permission_request_id: PermissionRequestId(Uuid::from_u128(78)),
            cli_request_id: "c1".into(),
            tool: "Bash".into(),
            summary: "npm install".into(),
            input: json!({ "command": "npm install" }),
            decision: Decision {
                effect: Effect::Ask,
                source: DecisionSource::Default,
                message: None,
            },
        }),
    );
    w.step(
        id,
        TaskInput::Session(SessionInput::AnswerPermission {
            permission_request_id: PermissionRequestId(Uuid::from_u128(78)),
            allow: true,
            message: None,
            actor: "operator:local".into(),
            remember: true,
            active_rules: Vec::new(),
        }),
    );
    w.step(
        id,
        TaskInput::Session(SessionInput::QuotaObserved {
            window: "five_hour".into(),
            utilization: 0.4,
            status: "allowed".into(),
            resets_at: Some(Timestamp(9_000_000)),
            refused: false,
        }),
    );
    w.tick(30_000);
    w.step(
        id,
        TaskInput::Session(SessionInput::TurnEnded {
            turn: 1,
            status: "success".into(),
            cost_usd_micros: Some(1_500),
            input_tokens: Some(10),
            output_tokens: Some(20),
        }),
    );
    w.step(
        id,
        TaskInput::Session(SessionInput::Steer {
            session_id: first,
            text: "add a test".into(),
        }),
    );
    w.step(
        id,
        TaskInput::Session(SessionInput::TurnAccepted { turn: 2 }),
    );
    w.tick(10_000);
    w.step(
        id,
        TaskInput::Session(SessionInput::CliExited {
            code: Some(137),
            signal: None,
        }),
    );
    w.step(id, launched(79));
    w.step(
        id,
        TaskInput::Session(SessionInput::TurnAccepted { turn: 1 }),
    );
    let second = SessionId(Uuid::from_u128(79));
    w.step(
        id,
        TaskInput::Session(SessionInput::Interrupt { session_id: second }),
    );
    w.tick(5_000);
    w.step(
        id,
        TaskInput::Session(SessionInput::TurnEnded {
            turn: 1,
            status: "error_during_execution".into(),
            cost_usd_micros: None,
            input_tokens: None,
            output_tokens: None,
        }),
    );

    let stored = w.store.read(|c| read::load_task(c, id)).unwrap().unwrap();
    let run = stored.stage_run.as_ref().unwrap();
    assert!(run.resumed_lost);
    assert_eq!(run.budget.map(|b| b.turns), Some(10));
    assert_eq!(stored.prior_usage.running_ms, 100_000);
    assert_eq!(stored.prior_usage.turns, 2);
    assert_eq!(stored.prior_usage.tokens, 30);
    let s = stored.session.as_ref().unwrap();
    assert_eq!(
        (s.state, s.resumed_from_session_id, s.resume_cause),
        (
            saka_core::vocab::SessionState::Idle,
            Some(first),
            Some(saka_core::vocab::ResumeCause::Lost)
        )
    );
    assert_eq!(
        s.interrupt_requested,
        Some(saka_core::vocab::InterruptBy::Operator)
    );
    assert_eq!((s.recorded_turn, s.recorded_running_ms), (1, 5_000));

    let sessions = w
        .store
        .read(|c| read::sessions(c, read::RunOf::WorkItem(id)))
        .unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].0.state, saka_core::vocab::SessionState::Resumed);
    assert_eq!(sessions[0].0.steers, 2);
    assert_eq!(
        (
            sessions[0].0.recorded_turn,
            sessions[0].0.recorded_running_ms
        ),
        (1, 90_000)
    );

    let usage: Vec<(String, i64, i64)> = w
        .store
        .read(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT session_id, turn, running_ms FROM usage_record ORDER BY ended_at, turn",
                )
                .unwrap();
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            Ok(rows)
        })
        .unwrap();
    assert_eq!(
        usage,
        vec![
            (first.to_string(), 1, 90_000),
            (second.to_string(), 1, 5_000)
        ]
    );

    let rules = w
        .store
        .read(|c| read::active_permission_rules(c, w.project.id))
        .unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(
        (rules[0].tool.as_str(), rules[0].command.as_deref()),
        ("Bash", Some("npm install"))
    );
    let revoked = saka_core::rule::evolve(
        Some(rules[0].clone()),
        &saka_core::rule::revoke(&rules[0], "too broad".into()).unwrap()[0],
        w.now,
    )
    .unwrap();
    w.store
        .commit(&Batch {
            permission_rules: vec![revoked.clone()],
            ..Batch::new(w.now)
        })
        .unwrap();
    assert!(
        w.store
            .read(|c| read::active_permission_rules(c, w.project.id))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        w.store
            .read(|c| read::load_permission_rule(c, revoked.id))
            .unwrap(),
        Some(revoked)
    );

    let quotas = w.store.read(read::account_quotas).unwrap();
    assert_eq!(quotas.len(), 1);
    assert_eq!(
        (
            quotas[0].account.as_str(),
            quotas[0].window.as_str(),
            quotas[0].utilization
        ),
        ("claude:default", "five_hour", 0.4)
    );
    let mut stale = stored.clone();
    stale.quota_observations = vec![saka_core::model::AccountQuota {
        utilization: 0.99,
        observed_at: Timestamp(1),
        ..quotas[0].clone()
    }];
    w.store
        .commit(&Batch {
            tasks: vec![stale],
            ..Batch::new(w.now)
        })
        .unwrap();
    assert_eq!(
        w.store.read(read::account_quotas).unwrap()[0].utilization,
        0.4,
        "an older observation does not replace a newer one"
    );
}

#[test]
fn two_live_environments_never_hold_the_same_port_block() {
    let mut w = World::new();
    let tasks = w.approve();
    let id = tasks[0].item.id;
    w.step(id, TaskInput::Admit);
    w.step(
        id,
        TaskInput::EnvironmentPrepared {
            environment_id: w.run_environment(id),
            root_path: "/e".into(),
            worktree_path: "/e/w".into(),
            base_sha: sha('a'),
            setup: Box::new(saka_core::model::EnvironmentSetup {
                port_start: Some(41000),
                port_count: Some(10),
                recipe: Some(json!({ "ports": 3, "env": {}, "setup": [], "teardown": [] })),
                recipe_sha256: Some("f".repeat(64)),
                setup: Vec::new(),
                toolchains: vec![saka_core::model::ToolchainUse {
                    tool: saka_core::vocab::Tool::Node,
                    version: "26.13.0".into(),
                    bin_dir: "/data/toolchains/node/26.13.0-linux-x64/bin".into(),
                    env: Default::default(),
                    downloaded: true,
                }],
                path_scope: vec!["services/api/**".into(), "docs/**".into()],
                docs: Some(saka_core::model::DocsCheckout {
                    repo_id: saka_core::ids::RepoId(Uuid::from_u128(77)),
                    full_name: "acme/docs".into(),
                    commit: sha('c'),
                    project_path: "docs".into(),
                    path: "/e/docs/docs".into(),
                    mode: saka_core::vocab::DocsMode::ReadOnly,
                    branch: None,
                    base_sha: sha('c'),
                    pushed_sha: None,
                }),
            }),
        },
    );
    let env = w
        .store
        .read(|c| read::load_task(c, id))
        .unwrap()
        .unwrap()
        .environment
        .unwrap();
    assert_eq!((env.port_start, env.port_count), (Some(41000), Some(10)));
    assert_eq!(env.recipe.unwrap()["ports"], json!(3));
    assert_eq!(env.toolchains[0].version, "26.13.0");
    assert_eq!(env.path_scope, ["services/api/**", "docs/**"]);
    let docs = env.docs.clone().unwrap();
    assert_eq!(
        (docs.path.as_str(), docs.mode, docs.commit.as_str()),
        (
            "/e/docs/docs",
            saka_core::vocab::DocsMode::ReadOnly,
            sha('c').as_str()
        )
    );
    assert!(
        !env.toolchains[0].downloaded,
        "a download is a fact of the event, not of the row"
    );
    assert_eq!(w.store.read(read::live_port_blocks).unwrap(), [(41000, 10)]);

    let conn = rusqlite_conn(&w);
    let copy = |state: &str| {
        conn.execute(
            &format!(
                "INSERT INTO environment (id, project_id, work_item_id, repo_id, root_path, worktree_path,
                   branch, base_sha, state, created_at, updated_at, port_start, port_count)
                 SELECT '{}', project_id, work_item_id, repo_id, root_path, worktree_path, branch, base_sha,
                   '{state}', created_at, updated_at, port_start, port_count
                 FROM environment WHERE port_start = 41000 LIMIT 1",
                Uuid::from_u128(900 + state.len() as u128)
            ),
            [],
        )
    };
    assert!(
        copy("teardown_failed").is_err(),
        "a second live row took the block"
    );
    assert_eq!(copy("torn_down").unwrap(), 1);
}

#[test]
fn a_draft_attempt_and_its_packet_round_trip_and_one_live_draft_holds_a_path() {
    use saka_core::draft::{self, NewDraft};
    use saka_core::ids::{SpecDraftAttemptId, SpecDraftId};
    use saka_core::model::Budget;
    use saka_core::vocab::{DraftLocale, SpecDraftState, Trigger};
    use saka_store::draft as rows;

    let mut w = World::new();
    let path = "docs/specs/export.md";
    let request = |id: SpecDraftId| {
        draft::request(NewDraft {
            spec_draft_id: id,
            project_id: w.project.id,
            path: path.into(),
            slug: "export".into(),
            idea: "Export tasks.".into(),
            locale: DraftLocale::En,
            context_paths: vec!["docs/prd.md".into()],
        })
    };
    let id = SpecDraftId(w.ids.next_id());
    let mut d = draft::evolve(None, &request(id), w.now).unwrap();
    d.draft.created_by = "operator:local".into();
    let mut batch = Batch::new(w.now);
    batch.drafts.push(d.clone());
    w.store.commit(&batch).unwrap();
    let loaded = w.store.read(|c| rows::load_draft(c, id)).unwrap().unwrap();
    assert_eq!(loaded.draft, d.draft);
    assert_eq!(
        w.store
            .read(|c| rows::live_draft_at(c, w.project.id, path))
            .unwrap(),
        Some(id)
    );
    assert_eq!(w.store.read(rows::active_count).unwrap(), 0);

    let attempt_id = SpecDraftAttemptId(w.ids.next_id());
    let budget = Budget {
        running_minutes: 30,
        turns: 6,
        tokens: 0,
        cost_usd_micros: 0,
    };
    for e in [
        Event::SpecDraftAttemptStarted {
            spec_draft_id: id,
            attempt_id,
            attempt: 1,
            trigger: Trigger::Auto,
            reason: None,
            note: None,
            budget,
        },
        Event::SpecDraftPacketRendered {
            spec_draft_id: id,
            attempt_id,
            base_sha: sha('b'),
            packet_sha256: "c".repeat(64),
            documents: vec![],
        },
    ] {
        d = draft::evolve(Some(d), &e, w.now).unwrap();
    }
    for text in ["packet one", "packet two"] {
        let mut batch = Batch::new(w.now);
        batch.drafts.push(d.clone());
        batch.draft_packets.push((attempt_id, text.into()));
        w.store.commit(&batch).unwrap();
    }
    assert_eq!(
        w.store.read(|c| rows::packet(c, attempt_id)).unwrap(),
        Some("packet one".into()),
        "the packet is written once"
    );
    let loaded = w.store.read(|c| rows::load_draft(c, id)).unwrap().unwrap();
    assert_eq!(loaded.draft.state, SpecDraftState::Drafting);
    assert_eq!(loaded.attempt, d.attempt);
    assert_eq!(w.store.read(rows::active_count).unwrap(), 1);

    let other = SpecDraftId(w.ids.next_id());
    let mut batch = Batch::new(w.now);
    batch
        .drafts
        .push(draft::evolve(None, &request(other), w.now).unwrap());
    assert!(
        w.store.commit(&batch).is_err(),
        "a second live draft for the path"
    );
}

#[test]
fn diagnostics_read_the_last_received_webhook_and_the_latest_authorization() {
    use saka_core::authorship::{UserLinkAuthorization, failure};
    use saka_core::ids::UserLinkAuthorizationId;
    use saka_core::vocab::{AuthorizationFailure, AuthorizationState};
    use saka_store::user_link::latest_authorization;
    use saka_store::{AuthorizationRow, WebhookDeliveryRow};

    let dir = tempfile::tempdir().unwrap();
    let now = Timestamp(1_000_000);
    let store = Store::open(&dir.path().join("saka.db"), &dir.path().join("b"), now).unwrap();
    assert_eq!(
        store
            .read(|c| read::last_webhook_received_at(c, "github"))
            .unwrap(),
        None
    );
    let delivery = |n: u128, integration: &str, at: i64| WebhookDeliveryRow {
        id: Uuid::from_u128(n),
        integration_id: integration.into(),
        delivery_id: format!("d-{n}"),
        event: "ping".into(),
        action: None,
        repo_id: None,
        refreshed: 0,
        received_at: Timestamp(at),
    };
    let authorization = |n: u128, at: i64, failed: bool| AuthorizationRow {
        authorization: UserLinkAuthorization {
            id: UserLinkAuthorizationId(Uuid::from_u128(n)),
            owner: "operator:local".into(),
            integration_id: "github".into(),
            state: if failed {
                AuthorizationState::Failed
            } else {
                AuthorizationState::Expired
            },
            user_code: "ABCD-1234".into(),
            verification_uri: "https://github.com/login/device".into(),
            interval_secs: 5,
            expires_at: Timestamp(at + 900_000),
            failure: failed.then(|| failure(AuthorizationFailure::DeviceFlowDisabled)),
            link_id: None,
            created_at: Timestamp(at),
            updated_at: Timestamp(at),
        },
        device: None,
    };
    let mut batch = Batch::new(now);
    batch.webhook_deliveries = vec![
        delivery(1, "github", 5_000),
        delivery(2, "github", 9_000),
        delivery(3, "ghes", 12_000),
    ];
    batch.user_link_authorizations = vec![
        authorization(4, 2_000, false),
        authorization(5, 3_000, true),
    ];
    store.commit(&batch).unwrap();
    assert_eq!(
        store
            .read(|c| read::last_webhook_received_at(c, "github"))
            .unwrap(),
        Some(Timestamp(9_000))
    );
    let latest = store
        .read(|c| latest_authorization(c, "operator:local", "github"))
        .unwrap()
        .unwrap();
    assert_eq!(latest.id, UserLinkAuthorizationId(Uuid::from_u128(5)));
    assert_eq!(
        latest.failure.map(|f| f.code),
        Some("device_flow_disabled".into())
    );
    assert!(
        store
            .read(|c| latest_authorization(c, "operator:local", "ghes"))
            .unwrap()
            .is_none()
    );
}
