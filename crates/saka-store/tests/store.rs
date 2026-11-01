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
        max_running_ms: 3_600_000,
        ci_appear_timeout_ms: 600_000,
        poll_interval_ms: 15_000,
        merge_method: MergeMethod::Squash,
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
            repos: vec![ProjectRepo {
                repo_id,
                name: "app".into(),
                is_default: true,
                path_scope: vec!["**".into()],
            }],
        };
        let repo: Repo = workspace::evolve_repo(None, &repo_event, now).unwrap();
        let project = workspace::evolve_project(None, &project_event, now).unwrap();

        let revision = NewRevision {
            source_commit: "a".repeat(40),
            content_hash: "sha256:abc".into(),
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
                default_repo: self.project.default_repo().unwrap(),
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
        expected.permissions.retain(|p| {
            p.state == saka_core::vocab::PermissionState::Pending
                || p.delivered_at.is_none() && p.state != saka_core::vocab::PermissionState::Expired
        });
        assert_eq!(stored, expected, "stored task differs after {events:?}");
        events
    }

    fn tick(&mut self, ms: i64) {
        self.now = self.now.plus_ms(ms);
    }

    fn run_to_tearing_down(&mut self, id: WorkItemId, n: u128) {
        self.step(id, TaskInput::Admit);
        self.step(
            id,
            TaskInput::EnvironmentPrepared {
                environment_id: EnvironmentId(Uuid::from_u128(n)),
                root_path: format!("/e{n}"),
                worktree_path: format!("/e{n}/w"),
                base_sha: sha('a'),
            },
        );
        self.step(
            id,
            TaskInput::SessionLaunched {
                session_id: SessionId(Uuid::from_u128(n + 1)),
                cli: "claude".into(),
                cli_version: None,
                model: None,
                account: "default".into(),
                runner_unit: format!("saka-session-{n}"),
                socket_path: format!("/run/{n}.sock"),
                log_path: format!("/data/{n}/events.ndjson"),
            },
        );
        self.step(id, TaskInput::TurnAccepted { turn: 1 });
        self.step(
            id,
            TaskInput::TurnEnded {
                turn: 1,
                status: "success".into(),
                cost_usd_micros: None,
                input_tokens: None,
                output_tokens: None,
            },
        );
        self.step(
            id,
            TaskInput::WorktreeInspected {
                turn: 1,
                dirty: false,
                commits_ahead: 1,
                head_sha: sha('b'),
            },
        );
        self.step(id, TaskInput::BranchPushed { sha: sha('b') });
        self.step(
            id,
            TaskInput::ChangeOpened {
                change_request_id: ChangeRequestId(Uuid::from_u128(n + 2)),
                number: u64::try_from(n).unwrap(),
                url: format!("https://github.com/acme/app/pull/{n}"),
                head_sha: sha('b'),
                base_branch: "main".into(),
            },
        );
        self.step(
            id,
            TaskInput::ChangeObserved {
                state: ChangeState::Merged,
                head_sha: sha('b'),
                merge_commit_sha: Some(sha('c')),
                merged_at: None,
                checks: vec![],
            },
        );
        self.step(id, TaskInput::MergeVerified { found: true });
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
            environment_id: EnvironmentId(Uuid::from_u128(76)),
            root_path: "/e".into(),
            worktree_path: "/e/w".into(),
            base_sha: sha('a'),
        },
    );
    w.step(
        id,
        TaskInput::SessionLaunched {
            session_id: session,
            cli: "claude".into(),
            cli_version: Some("2.0.0".into()),
            model: None,
            account: "default".into(),
            runner_unit: "saka-session-77".into(),
            socket_path: "/run/77.sock".into(),
            log_path: "/data/77/events.ndjson".into(),
        },
    );
    w.step(
        id,
        TaskInput::SessionInitialized {
            cli_session_id: "cli-1".into(),
            model: Some("sonnet".into()),
        },
    );
    w.tick(1000);
    w.step(id, TaskInput::TurnAccepted { turn: 1 });
    w.tick(5000);
    let stored = w.store.read(|c| read::load_task(c, id)).unwrap().unwrap();
    assert_eq!(stored.session.as_ref().unwrap().running_ms_at(w.now), 5000);

    w.step(
        id,
        TaskInput::PermissionRequested {
            permission_request_id: permission,
            cli_request_id: "r1".into(),
            tool: "Bash".into(),
            summary: "npm install".into(),
            input: json!({ "command": "npm install" }),
            decision: Decision {
                effect: Effect::Ask,
                source: DecisionSource::Default,
            },
        },
    );
    w.tick(60_000);
    w.step(
        id,
        TaskInput::AnswerPermission {
            permission_request_id: permission,
            allow: true,
            message: None,
            actor: "operator:local".into(),
        },
    );
    w.step(
        id,
        TaskInput::PermissionDelivered {
            permission_request_id: permission,
        },
    );
    w.step(
        id,
        TaskInput::TurnEnded {
            turn: 1,
            status: "success".into(),
            cost_usd_micros: Some(1500),
            input_tokens: Some(10),
            output_tokens: Some(20),
        },
    );
    w.step(
        id,
        TaskInput::WorktreeInspected {
            turn: 1,
            dirty: false,
            commits_ahead: 1,
            head_sha: sha('b'),
        },
    );
    w.step(id, TaskInput::BranchPushed { sha: sha('b') });
    w.step(
        id,
        TaskInput::ChangeOpened {
            change_request_id: ChangeRequestId(Uuid::from_u128(79)),
            number: 12,
            url: "https://github.com/acme/app/pull/12".into(),
            head_sha: sha('b'),
            base_branch: "main".into(),
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
            state: ChangeState::Open,
            head_sha: sha('b'),
            merge_commit_sha: None,
            merged_at: None,
            checks,
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
            merge_commit_sha: sha('c'),
            merged_at: Some(Timestamp(5)),
        },
    );
    w.step(id, TaskInput::MergeVerified { found: true });
    w.step(id, TaskInput::EnvironmentTornDown);

    let done = w.store.read(|c| read::load_task(c, id)).unwrap().unwrap();
    assert_eq!(done.item.state, WorkItemState::Done);
    assert_eq!(
        count(&w.store, "stage_run"),
        3,
        "dev, merge, and sync runs are all kept"
    );
    assert_eq!(count(&w.store, "ci_check"), 1);
    assert_eq!(count(&w.store, "gate_result"), 1);
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
            default_repo: w.project.default_repo().unwrap(),
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
    assert_eq!(w.store.migration().applied, vec![1, 2, 3, 4]);
    assert_eq!(w.store.schema_version().unwrap(), Some(4));
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
        }
    );
    assert_eq!(
        reopened
            .read(|c| read::revision_by_hash(c, spec.id, "sha256:abc", 2))
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
    w.step(second, TaskInput::EnvironmentTornDown);

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
