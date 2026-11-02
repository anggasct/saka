use std::path::PathBuf;
use std::sync::Arc;

use saka_core::ids::{ProjectId, SpecId, SpecRevisionId, WorkItemId};
use saka_core::model::{Spec, Task, WorkItem};
use saka_core::policy::{Policy, RuleSpec};
use saka_core::task::LoopConfig;
use saka_core::vocab::{
    BlockerCode, MergeMethod, PermissionState, SpecState, Stage, StagePhase, WorkItemState,
};
use saka_core::vocab::{SpecOrigin, WorkItemOrigin};
use saka_engine::{
    Engine, EngineConfig, EngineError, EnvConfig, Integration, JobReport, NewProject, Ports,
    RegisteredSpec,
};
use saka_ports::Clock;
use saka_store::{Batch, SpecRevisionRow, Store, TaskSet, list, read};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::clock::{FakeClock, FakeIds};
use crate::fixtures::{TWO_STEP_PATH, TWO_STEP_SPEC};
use crate::world::{AgentScript, CiScript, FakeWorld, Stats};

pub const IDLE_TICK_MS: i64 = 5_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operator {
    pub merge: bool,
    pub retry: Vec<BlockerCode>,
    pub answer: Option<bool>,
}

impl Default for Operator {
    fn default() -> Self {
        Operator {
            merge: true,
            retry: vec![BlockerCode::SessionLost],
            answer: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SimConfig {
    pub loop_cfg: LoopConfig,
    pub max_active_tasks: usize,
    pub env: EnvConfig,
    pub rules: Vec<RuleSpec>,
    pub agent: AgentScript,
    pub ci: CiScript,
    pub operator: Operator,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            loop_cfg: LoopConfig {
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
            },
            max_active_tasks: 2,
            env: EnvConfig {
                port_first: 41000,
                port_last: 41099,
                ports_per_env: 10,
            },
            rules: Vec::new(),
            agent: AgentScript::default(),
            ci: CiScript::Pass,
            operator: Operator::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    RunnerEvent,
    Job(JobReport),
    Reconciled,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimOutcome {
    pub spec_state: SpecState,
    pub task_states: Vec<(String, WorkItemState)>,
    pub prs_opened: u32,
    pub merges: u32,
    pub pushes: u32,
    pub branches_deleted: u32,
    pub leftover_branches: Vec<String>,
    pub merges_on_base: bool,
}

pub struct Sim {
    pub world: Arc<FakeWorld>,
    pub clock: Arc<FakeClock>,
    pub ids: Arc<FakeIds>,
    pub cfg: SimConfig,
    pub project_id: ProjectId,
    pub spec_id: SpecId,
    pub revision_id: SpecRevisionId,
    pub restarts: u32,
    dir: tempfile::TempDir,
    engine: Option<Arc<Engine>>,
    rt: tokio::runtime::Runtime,
}

impl Sim {
    pub fn new(cfg: SimConfig) -> Sim {
        let clock = Arc::new(FakeClock::new());
        let ids = Arc::new(FakeIds::default());
        let world = Arc::new(FakeWorld::new(
            Arc::clone(&clock),
            cfg.agent.clone(),
            cfg.ci.clone(),
        ));
        // RAM-backed when available: the crash property test opens many databases.
        let dir = match std::path::Path::new("/dev/shm").is_dir() {
            true => tempfile::tempdir_in("/dev/shm"),
            false => tempfile::tempdir(),
        }
        .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let mut sim = Sim {
            world,
            clock,
            ids,
            cfg,
            project_id: ProjectId(uuid::Uuid::nil()),
            spec_id: SpecId(uuid::Uuid::nil()),
            revision_id: SpecRevisionId(uuid::Uuid::nil()),
            restarts: 0,
            dir,
            engine: None,
            rt,
        };
        sim.start_engine();
        let engine = sim.engine();
        let cause = engine.cause("human:operator:local");
        let (project_id, _) = engine
            .register_project(
                &NewProject {
                    name: "tasks".into(),
                    integration_id: "github".into(),
                    provider: "github".into(),
                    full_name: "acme/tasks".into(),
                    url: "https://github.com/acme/tasks".into(),
                    default_branch: "main".into(),
                    project_path: "docs".into(),
                    base_branch: "main".into(),
                    config_commit: sim.world.base_head(),
                    repo_name: "app".into(),
                },
                &cause,
            )
            .unwrap();
        let spec = engine
            .register_spec(
                project_id,
                TWO_STEP_PATH,
                &sim.world.base_head(),
                TWO_STEP_SPEC.as_bytes(),
                &cause,
            )
            .unwrap();
        assert!(spec.lint_ok, "fixture spec has issues: {:?}", spec.issues);
        sim.project_id = project_id;
        sim.spec_id = spec.spec_id;
        sim.revision_id = spec.revision_id;
        sim
    }

    fn db(&self) -> PathBuf {
        self.dir.path().join("saka.db")
    }

    fn start_engine(&mut self) {
        let store = Store::open(
            &self.db(),
            &self.dir.path().join("backups"),
            self.clock.now(),
        )
        .unwrap();
        let ports = Ports {
            clock: self.clock.clone(),
            ids: self.ids.clone(),
            env: self.world.clone(),
            scm: self.world.clone(),
            agents: self.world.clone(),
            docs: self.world.clone(),
            toolchains: self.world.clone(),
        };
        let cfg = EngineConfig {
            loop_cfg: self.cfg.loop_cfg.clone(),
            max_active_tasks: self.cfg.max_active_tasks,
            env: self.cfg.env,
            policy: Policy::compile(&self.cfg.rules).unwrap(),
            worker_id: format!("sim-{}", self.restarts),
            integrations: vec![Integration {
                id: "github".into(),
                provider: "github".into(),
                web_url: "https://github.com".into(),
            }],
        };
        let engine = Arc::new(Engine::new(Arc::new(store), ports, cfg));
        engine.recover().unwrap();
        self.engine = Some(engine);
    }

    pub fn engine(&self) -> Arc<Engine> {
        Arc::clone(self.engine.as_ref().expect("engine is running"))
    }

    pub fn restart(&mut self) {
        self.engine = None;
        self.world.disconnect_all();
        self.restarts += 1;
        self.start_engine();
    }

    pub fn crash_mid_job(&mut self) -> bool {
        let engine = self.engine();
        let Some(job) = engine.claim_job().unwrap() else {
            return false;
        };
        let _lost = self.rt.block_on(engine.execute(&job));
        drop(engine);
        self.restart();
        true
    }

    pub fn approve(&mut self) -> Vec<WorkItemId> {
        let engine = self.engine();
        let cause = engine.cause("human:operator:local");
        engine
            .approve_spec(self.spec_id, self.revision_id, None, &cause)
            .unwrap()
    }

    pub fn register(&self, path: &str, text: &str) -> RegisteredSpec {
        let engine = self.engine();
        let cause = engine.cause("human:operator:local");
        engine
            .register_spec(
                self.project_id,
                path,
                &self.world.base_head(),
                text.as_bytes(),
                &cause,
            )
            .unwrap()
    }

    pub fn approve_spec(
        &self,
        spec_id: SpecId,
        revision_id: SpecRevisionId,
    ) -> Result<Vec<WorkItemId>, EngineError> {
        let engine = self.engine();
        let cause = engine.cause("human:operator:local");
        engine.approve_spec(spec_id, revision_id, None, &cause)
    }

    pub fn old_parser_spec(&self, slug: &str, text: &str) -> (SpecId, SpecRevisionId) {
        let digest = Sha256::digest(text.as_bytes());
        let half = |range: std::ops::Range<usize>| {
            uuid::Uuid::from_slice(&digest[range]).expect("16 bytes of the digest")
        };
        let spec_id = SpecId(half(0..16));
        let revision_id = SpecRevisionId(half(16..32));
        let now = self.spec().created_at;
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let mut batch = Batch::new(now);
        batch.specs.push(Spec {
            id: spec_id,
            project_id: self.project_id,
            path: format!("docs/specs/{slug}.md"),
            slug: slug.into(),
            title: slug.into(),
            state: SpecState::Ready,
            origin: SpecOrigin::Manual,
            priority: 0,
            latest_revision_id: revision_id,
            approved_revision_id: None,
            approved_at: None,
            shipped_at: None,
            created_at: now,
            updated_at: now,
        });
        batch.revisions.push(SpecRevisionRow {
            id: revision_id.0,
            spec_id: spec_id.0,
            project_id: self.project_id,
            source_commit: self.world.base_head(),
            content_hash: format!("sha256:{hex}"),
            content: text.as_bytes().to_vec(),
            lint_ok: true,
            lint_issues: json!([]),
            model: json!({"slug": slug, "title": slug, "acceptance_criteria": [], "steps": [
                {"id": "S1", "index": 1, "work": slug, "ac": ["AC-01"], "commands": ["make test"], "line": 10}
            ]}),
            parser_version: 1,
            created_at: now,
        });
        self.engine().store().commit(&batch).unwrap();
        (spec_id, revision_id)
    }

    pub fn old_parser_revision(&self, spec_id: SpecId, text: &str) -> SpecRevisionId {
        let mut spec = self.spec_of(spec_id);
        let digest = Sha256::digest(text.as_bytes());
        let revision_id = SpecRevisionId(
            uuid::Uuid::from_slice(&digest[16..32]).expect("16 bytes of the digest"),
        );
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let now = self.clock.now();
        spec.latest_revision_id = revision_id;
        spec.updated_at = now;
        let mut batch = Batch::new(now);
        batch.specs.push(spec.clone());
        batch.revisions.push(SpecRevisionRow {
            id: revision_id.0,
            spec_id: spec_id.0,
            project_id: spec.project_id,
            source_commit: self.world.base_head(),
            content_hash: format!("sha256:{hex}"),
            content: text.as_bytes().to_vec(),
            lint_ok: true,
            lint_issues: json!([]),
            model: json!({"slug": spec.slug, "title": spec.title, "acceptance_criteria": [], "steps": [
                {"id": "S1", "index": 1, "work": spec.slug, "ac": ["AC-01"], "commands": ["make test"], "line": 10}
            ]}),
            parser_version: 1,
            created_at: now,
        });
        self.engine().store().commit(&batch).unwrap();
        revision_id
    }

    pub fn occupy_branch(&self, project_id: ProjectId, branch: &str) {
        let now = self.clock.now();
        let repo_id = self.spec_task_repo();
        let mut batch = Batch::new(now);
        batch.tasks.push(Task::new(WorkItem {
            id: WorkItemId(
                uuid::Uuid::from_slice(&Sha256::digest(branch.as_bytes())[..16])
                    .expect("16 bytes of the digest"),
            ),
            project_id,
            origin: WorkItemOrigin::ExternalChange,
            spec_id: None,
            spec_revision_id: None,
            step_id: None,
            step_index: None,
            task_key: format!("external/{branch}"),
            work: "External change".into(),
            repo_id,
            branch: branch.into(),
            state: WorkItemState::Queued,
            stage: None,
            stage_phase: None,
            priority: 0,
            version: 1,
            admitted_at: None,
            done_at: None,
            created_at: now,
            updated_at: now,
        }));
        self.engine().store().commit(&batch).unwrap();
    }

    fn spec_task_repo(&self) -> saka_core::ids::RepoId {
        self.engine()
            .store()
            .read(|c| read::load_project(c, self.project_id))
            .unwrap()
            .and_then(|p| p.default_repo())
            .expect("project with a default repo")
    }

    pub fn spec_by_slug(&self, slug: &str) -> SpecId {
        self.engine()
            .store()
            .read(|c| list::spec_by_slug(c, self.project_id, slug))
            .unwrap()
            .expect("spec with this slug")
            .id
    }

    pub fn tasks(&self) -> Vec<Task> {
        self.tasks_of(self.spec_id)
    }

    pub fn tasks_of(&self, spec_id: SpecId) -> Vec<Task> {
        let engine = self.engine();
        let mut tasks: Vec<Task> = engine
            .store()
            .read(|c| read::load_tasks(c, TaskSet::Spec(spec_id)))
            .unwrap();
        tasks.sort_by_key(|t| t.item.step_index);
        tasks
    }

    pub fn all_tasks(&self) -> Vec<Task> {
        let engine = self.engine();
        engine
            .store()
            .read(|c| read::load_tasks(c, TaskSet::Reconcile))
            .unwrap()
    }

    pub fn task(&self, step: u32) -> Task {
        self.tasks()
            .into_iter()
            .find(|t| t.item.step_index == Some(step))
            .expect("task for step")
    }

    pub fn sessions(&self, step: u32) -> Vec<saka_core::model::Session> {
        let id = self.task(step).item.id;
        self.engine()
            .store()
            .read(|c| read::sessions(c, id))
            .unwrap()
    }

    pub fn timeline(
        &self,
        session: saka_core::ids::SessionId,
    ) -> Vec<saka_core::model::SessionActivity> {
        self.engine()
            .store()
            .read(|c| read::timeline(c, session, None, 10_000))
            .unwrap()
    }

    pub fn usage(&self, step: u32) -> Vec<saka_core::model::UsageRecord> {
        let id = self.task(step).item.id;
        self.engine()
            .store()
            .read(|c| read::usage_records(c, Some(id), None))
            .unwrap()
            .into_iter()
            .map(|(u, _)| u)
            .collect()
    }

    pub fn spec(&self) -> Spec {
        self.spec_of(self.spec_id)
    }

    pub fn spec_of(&self, spec_id: SpecId) -> Spec {
        let engine = self.engine();
        engine
            .store()
            .read(|c| read::load_spec(c, spec_id))
            .unwrap()
            .unwrap()
    }

    pub fn stats(&self) -> Stats {
        self.world.stats()
    }

    pub fn block_on<F: std::future::Future>(&self, f: F) -> F::Output {
        self.rt.block_on(f)
    }

    pub fn step(&mut self) -> Step {
        let engine = self.engine();
        if let Some(event) = self.world.pop_event() {
            engine.on_runner_event(event).unwrap();
            return Step::RunnerEvent;
        }
        if let Some(report) = self.rt.block_on(engine.run_next_job()).unwrap() {
            return Step::Job(report);
        }
        let report = engine.reconcile().unwrap();
        let due = engine
            .store()
            .oldest_ready_job_age_ms(self.clock.now())
            .unwrap()
            .is_some();
        if report.decisions > 0 || report.shipped > 0 || due || self.world.has_events() {
            return Step::Reconciled;
        }
        self.clock.advance(IDLE_TICK_MS);
        Step::Idle
    }

    pub fn operate(&mut self) {
        let engine = self.engine();
        let cause = engine.cause("human:operator:local");
        for task in self.all_tasks() {
            let id = task.item.id;
            if self.cfg.operator.merge && task.is_at(Stage::Merge, StagePhase::AwaitingMerge) {
                let _ = engine.request_merge(id, task.item.version, None, &cause);
            }
            let retry = task
                .open_blocker()
                .is_some_and(|b| self.cfg.operator.retry.contains(&b.code));
            if task.item.state == WorkItemState::Blocked && retry {
                engine
                    .retry(id, "retried by the simulated operator", &cause)
                    .unwrap();
            }
            if let Some(allow) = self.cfg.operator.answer {
                for p in task
                    .permissions
                    .iter()
                    .filter(|p| p.state == PermissionState::Pending)
                {
                    let _ = engine.answer_permission(p.id, allow, None, &cause);
                }
            }
        }
    }

    pub fn run_until(&mut self, max_steps: usize, mut done: impl FnMut(&Sim) -> bool) -> bool {
        for _ in 0..max_steps {
            if done(self) {
                return true;
            }
            self.operate();
            self.step();
        }
        done(self)
    }

    pub fn run_to_shipped(&mut self, max_steps: usize) -> bool {
        self.run_until(max_steps, |s| s.spec().state == SpecState::Shipped)
    }

    pub fn outcome(&self) -> SimOutcome {
        let stats = self.stats();
        let base = self.world.base_head();
        let prs = self.world.prs();
        SimOutcome {
            spec_state: self.spec().state,
            task_states: self
                .tasks()
                .iter()
                .map(|t| (t.item.task_key.clone(), t.item.state))
                .collect(),
            prs_opened: stats.prs_opened,
            merges: stats.merges,
            pushes: stats.pushes,
            branches_deleted: stats.branches_deleted,
            leftover_branches: self
                .world
                .remote_branches()
                .into_keys()
                .filter(|b| b != crate::world::BASE_BRANCH)
                .collect(),
            merges_on_base: prs.iter().all(|p| {
                p.merge_commit_sha
                    .as_ref()
                    .is_some_and(|m| self.world.is_ancestor(m, &base))
            }),
        }
    }
}
