use std::path::PathBuf;
use std::sync::Arc;

use saka_core::draft::DraftConfig;
use saka_core::ids::SpecDraftId;
use saka_core::ids::{ProjectId, SpecId, SpecRevisionId, WorkItemId};
use saka_core::model::{Draft, LintRound};
use saka_core::model::{Spec, Task, WorkItem};
use saka_core::policy::{Policy, RuleSpec};
use saka_core::task::LoopConfig;
use saka_core::vocab::{
    BlockerCode, MergeMethod, PermissionState, SpecState, Stage, StagePhase, WorkItemState,
};
use saka_core::vocab::{DraftMode, ReloadTrigger, SpecOrigin, TargetKind, WorkItemOrigin};
use saka_engine::{
    AddProject, DiagnosticLimits, Engine, EngineConfig, EngineError, EnvConfig, Integration,
    JobReport, NewBinding, NewProject, NewRepo, NewSpecDraft, Ports, ProjectView, RegisteredSpec,
    ReloadView, StoredEvent,
};
use saka_ports::Clock;
use saka_store::{Batch, SpecRevisionRow, Store, TaskSet, list, read};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::clock::{FakeClock, FakeEntropy, FakeIds};
use crate::fixtures::{TWO_STEP_PATH, TWO_STEP_SPEC};
use crate::world::{AgentScript, CiScript, FakeWorld, Stats};

pub const IDLE_TICK_MS: i64 = 5_000;
pub const DOCS_REPO: &str = "acme/docs";
pub const SHOP_REPO: &str = "acme/shop";
pub const WEB_REPO: &str = "acme/web";
pub const SECOND_INTEGRATION: &str = "ghes";
pub const SECOND_WEB_URL: &str = "https://ghe.acme.test";

#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub saka_toml: String,
    pub branches: Vec<(String, String)>,
    pub base_branch: Option<String>,
}
pub const ZERO_SHA256: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

fn home_repo() -> NewRepo {
    NewRepo {
        repo_id: None,
        integration_id: "github".into(),
        provider: "github".into(),
        full_name: crate::world::HOME_REPO.into(),
        url: format!("https://github.com/{}", crate::world::HOME_REPO),
        default_branch: "main".into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operator {
    pub start: bool,
    pub merge: bool,
    pub retry: Vec<BlockerCode>,
    pub answer: Option<bool>,
    pub decide: Option<(Option<u32>, Option<String>)>,
}

impl Default for Operator {
    fn default() -> Self {
        Operator {
            start: true,
            merge: true,
            retry: vec![BlockerCode::SessionLost],
            answer: None,
            decide: None,
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
    pub draft: DraftConfig,
    pub architect_model: Option<String>,
    pub config_poll_secs: u32,
    pub diagnostics: DiagnosticLimits,
    pub hooks: Vec<saka_core::hook::Hook>,
    pub auth_enabled: bool,
    pub workflow: saka_core::workflow::Workflow,
    pub autonomy_request: Option<saka_core::vocab::Autonomy>,
}

pub fn without_review() -> saka_core::workflow::Workflow {
    use saka_core::vocab::{Stage, Trigger, WorkflowPreset};
    let mut workflow = saka_core::workflow::Workflow::of(WorkflowPreset::Custom);
    workflow.spec = true;
    workflow.set_stage(Stage::Dev, Trigger::Auto);
    workflow.set_stage(Stage::Merge, Trigger::Manual);
    workflow
}

pub fn without_fix() -> saka_core::workflow::Workflow {
    let mut workflow = saka_core::workflow::Workflow::default();
    workflow.remove_stage(saka_core::vocab::Stage::Fix);
    workflow
}

pub fn draft_config() -> DraftConfig {
    DraftConfig {
        max_active: 1,
        lint_rounds: 4,
        session_idle_ms: 72 * 3_600_000,
        budget: saka_core::model::Budget {
            running_minutes: 30,
            turns: 6,
            tokens: 0,
            cost_usd_micros: 0,
        },
        session: saka_core::session::SessionConfig {
            permission_wait_ms: 24 * 3_600_000,
            decision_wait_ms: 72 * 3_600_000,
            idle_ms: 30 * 60_000,
            budget_warn_percent: 80,
            rate_limit_pause: 0.9,
            account: "claude:default".into(),
        },
    }
}

impl SimConfig {
    pub fn without_fix() -> SimConfig {
        SimConfig {
            workflow: without_fix(),
            ..SimConfig::default()
        }
    }

    pub fn without_review() -> SimConfig {
        SimConfig {
            workflow: without_review(),
            ..SimConfig::default()
        }
    }
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            loop_cfg: LoopConfig {
                max_followup_turns: 1,
                permission_wait_ms: 24 * 3_600_000,
                decision_wait_ms: 72 * 3_600_000,
                idle_ms: 30 * 60_000,
                budgets: std::collections::BTreeMap::from([
                    (
                        saka_core::vocab::Stage::Dev,
                        saka_core::model::Budget {
                            running_minutes: 60,
                            turns: 10,
                            tokens: 0,
                            cost_usd_micros: 0,
                        },
                    ),
                    (
                        saka_core::vocab::Stage::Review,
                        saka_core::model::Budget {
                            running_minutes: 20,
                            turns: 3,
                            tokens: 0,
                            cost_usd_micros: 0,
                        },
                    ),
                    (
                        saka_core::vocab::Stage::Fix,
                        saka_core::model::Budget {
                            running_minutes: 30,
                            turns: 6,
                            tokens: 0,
                            cost_usd_micros: 0,
                        },
                    ),
                ]),
                budget_warn_percent: 80,
                rate_limit_pause: 0.9,
                account: "claude:default".into(),
                autonomy: saka_core::autonomy::Ceilings::default(),
                ci_appear_timeout_ms: 600_000,
                poll_interval_ms: 15_000,
                merge_method: MergeMethod::Squash,
                delete_branch_after_merge: true,
                commit_status: true,
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
            draft: draft_config(),
            architect_model: None,
            config_poll_secs: 0,
            diagnostics: DiagnosticLimits::default(),
            hooks: Vec::new(),
            auth_enabled: false,
            workflow: saka_core::workflow::Workflow::default(),
            autonomy_request: None,
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
    pub hooks: Arc<crate::hooks::FakeHooks>,
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
    fn boot(cfg: SimConfig) -> Sim {
        let clock = Arc::new(FakeClock::new());
        let ids = Arc::new(FakeIds::default());
        let world = Arc::new(FakeWorld::new(
            Arc::clone(&clock),
            cfg.agent.clone(),
            cfg.ci.clone(),
        ));
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
            hooks: Arc::new(crate::hooks::FakeHooks::default()),
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
        sim
    }

    pub fn new(cfg: SimConfig) -> Sim {
        let mut sim = Sim::boot(cfg);
        let engine = sim.engine();
        let cause = engine.cause("human:operator:local");
        let (project_id, _) = engine
            .register_project(
                &NewProject {
                    name: "tasks".into(),
                    workspace: home_repo(),
                    project_path: "docs".into(),
                    base_branch: "main".into(),
                    config_commit: sim.world.base_head(),
                    config_sha256: ZERO_SHA256.into(),
                    workflow: sim.cfg.workflow.clone(),
                    hook_skip: Vec::new(),
                    bindings: vec![NewBinding {
                        repo: home_repo(),
                        name: "app".into(),
                        is_default: true,
                        base_branch: "main".into(),
                        path_scope: vec!["**".into()],
                        autonomy: sim.cfg.autonomy_request,
                    }],
                },
                &cause,
            )
            .unwrap();
        sim.project_id = project_id;
        sim.register_fixture_spec();
        sim
    }

    pub fn multi_repo(cfg: SimConfig, layout: &Layout) -> Sim {
        let mut sim = Sim::boot(cfg);
        sim.world.set_home(DOCS_REPO);
        for (repo, branch) in &layout.branches {
            sim.world.create_branch(repo, branch);
        }
        sim.world
            .put_doc("docs/saka.toml", layout.saka_toml.as_bytes());
        let engine = sim.engine();
        let cause = engine.cause("human:operator:local");
        let add = AddProject {
            workspace_url: format!("https://github.com/{DOCS_REPO}"),
            project_path: "docs".into(),
            base_branch: layout.base_branch.clone(),
        };
        let view = sim
            .rt
            .block_on(engine.add_project(&add, &cause, None))
            .unwrap();
        sim.project_id = view.project.id;
        sim.register_fixture_spec();
        sim
    }

    fn register_fixture_spec(&mut self) {
        let engine = self.engine();
        let cause = engine.cause("human:operator:local");
        let spec = self
            .rt
            .block_on(engine.register_spec(
                self.project_id,
                TWO_STEP_PATH,
                &self.world.base_head(),
                TWO_STEP_SPEC.as_bytes(),
                &cause,
            ))
            .unwrap();
        assert!(spec.lint_ok, "fixture spec has issues: {:?}", spec.issues);
        self.spec_id = spec.spec_id;
        self.revision_id = spec.revision_id;
    }

    pub fn reload(&self) -> Result<ReloadView, EngineError> {
        let engine = self.engine();
        let cause = engine.cause("human:operator:local");
        self.rt.block_on(engine.reload_project_config(
            self.project_id,
            ReloadTrigger::Operator,
            &cause,
            None,
        ))
    }

    pub fn project(&self) -> ProjectView {
        self.engine().project(self.project_id).unwrap().unwrap()
    }

    pub fn events_of(&self, kind: &str) -> Vec<StoredEvent> {
        let engine = self.engine();
        let mut out = Vec::new();
        let mut after = 0;
        loop {
            let page = engine.events_after(after, 500).unwrap();
            let Some(last) = page.last() else {
                return out;
            };
            after = last.seq;
            out.extend(page.into_iter().filter(|e| e.kind == kind));
        }
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
            entropy: Arc::new(FakeEntropy::default()),
            env: self.world.clone(),
            scm: self.world.clone(),
            agents: self.world.clone(),
            docs: self.world.clone(),
            toolchains: self.world.clone(),
            secrets: self.world.clone(),
            supervisor: self.world.clone(),
            releases: self.world.clone(),
            webhooks: self.hooks.clone(),
            notifier: self.hooks.clone(),
            host: self.hooks.clone(),
        };
        let cfg = EngineConfig {
            loop_cfg: self.cfg.loop_cfg.clone(),
            max_active_tasks: self.cfg.max_active_tasks,
            env: self.cfg.env,
            policy: Policy::compile(&self.cfg.rules).unwrap(),
            worker_id: format!("sim-{}", self.restarts),
            integrations: vec![
                Integration {
                    id: "github".into(),
                    provider: "github".into(),
                    web_url: "https://github.com".into(),
                },
                Integration {
                    id: SECOND_INTEGRATION.into(),
                    provider: "github".into(),
                    web_url: SECOND_WEB_URL.into(),
                },
            ],
            git_identity: saka_core::authorship::GitIdentity {
                name: "Saka Agent".into(),
                email: "agent@saka.invalid".into(),
            },
            draft: self.cfg.draft.clone(),
            architect_model: self.cfg.architect_model.clone(),
            config_poll_secs: self.cfg.config_poll_secs,
            diagnostics: self.cfg.diagnostics,
            hooks: self.cfg.hooks.clone(),
            auth_enabled: self.cfg.auth_enabled,
        };
        let engine = Arc::new(Engine::new(Arc::new(store), ports, cfg));
        engine.recover().unwrap();
        let decisions: Arc<dyn saka_ports::Decisions> = engine.clone();
        self.world.connect_decisions(Arc::downgrade(&decisions));
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
        self.rt
            .block_on(engine.register_spec(
                self.project_id,
                path,
                &self.world.base_head(),
                text.as_bytes(),
                &cause,
            ))
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
            context_hash: saka_core::event::EMPTY_CONTEXT_HASH.to_string(),
            context: Vec::new(),
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
            context_hash: saka_core::event::EMPTY_CONTEXT_HASH.to_string(),
            context: Vec::new(),
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
        self.occupy_branch_in(project_id, self.spec_task_repo(), branch);
    }

    pub fn occupy_branch_in(
        &self,
        project_id: ProjectId,
        repo_id: saka_core::ids::RepoId,
        branch: &str,
    ) {
        let now = self.clock.now();
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
            start_approved_at: None,
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

    pub fn create_draft(
        &self,
        path: &str,
        idea: &str,
        context_paths: &[&str],
    ) -> Result<Draft, EngineError> {
        self.create_draft_as(DraftMode::Draft, TargetKind::New, path, idea, context_paths)
    }

    pub fn create_session(
        &self,
        path: &str,
        idea: &str,
        context_paths: &[&str],
    ) -> Result<Draft, EngineError> {
        self.create_draft_as(
            DraftMode::Session,
            TargetKind::New,
            path,
            idea,
            context_paths,
        )
    }

    pub fn revise_session(&self, path: &str, idea: &str) -> Result<Draft, EngineError> {
        self.create_draft_as(DraftMode::Session, TargetKind::Revise, path, idea, &[])
    }

    fn create_draft_as(
        &self,
        mode: DraftMode,
        target_kind: TargetKind,
        path: &str,
        idea: &str,
        context_paths: &[&str],
    ) -> Result<Draft, EngineError> {
        let engine = self.engine();
        let cause = engine.cause("human:operator:local");
        self.rt.block_on(engine.create_spec_draft(
            self.project_id,
            NewSpecDraft {
                path: path.into(),
                idea: idea.into(),
                locale: saka_core::vocab::DraftLocale::En,
                mode,
                target_kind,
                context_paths: context_paths.iter().map(|p| (*p).to_string()).collect(),
            },
            &cause,
            None,
        ))
    }

    pub fn draft(&self, id: SpecDraftId) -> Draft {
        self.engine()
            .store()
            .read(|c| saka_store::draft::load_draft(c, id))
            .unwrap()
            .expect("draft")
    }

    pub fn draft_rounds(&self, id: SpecDraftId) -> Vec<(u32, LintRound)> {
        self.engine()
            .store()
            .read(|c| saka_store::draft::lint_rounds(c, id))
            .unwrap()
    }

    pub fn draft_sessions(&self, id: SpecDraftId) -> Vec<saka_core::model::Session> {
        self.engine()
            .store()
            .read(|c| read::sessions(c, read::RunOf::SpecDraft(id)))
            .unwrap()
            .into_iter()
            .map(|(s, _)| s)
            .collect()
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

    pub fn externals(&self) -> Vec<Task> {
        self.engine()
            .store()
            .read(|c| {
                let ids: Vec<WorkItemId> = {
                    let mut stmt = c.prepare(
                        "SELECT id FROM work_item WHERE origin = 'external_change' ORDER BY created_at, id",
                    )?;
                    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                    rows.filter_map(|r| r.ok()?.parse::<uuid::Uuid>().ok())
                        .map(WorkItemId)
                        .collect()
                };
                read::load_tasks(c, TaskSet::Ids(&ids))
            })
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
            .read(|c| read::sessions(c, read::RunOf::WorkItem(id)))
            .unwrap()
            .into_iter()
            .map(|(s, _)| s)
            .collect()
    }

    pub fn timeline(
        &self,
        session: saka_core::ids::SessionId,
    ) -> Vec<saka_core::model::SessionActivity> {
        self.engine()
            .store()
            .read(|c| read::timeline(c, session, read::TimelineCursor::Start, 10_000))
            .unwrap()
            .items
    }

    pub fn usage(&self, step: u32) -> Vec<saka_core::model::UsageRecord> {
        let id = self.task(step).item.id;
        self.engine()
            .store()
            .read(|c| read::usage_records(c, Some(read::RunOf::WorkItem(id)), None))
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
            if self.cfg.operator.start && task.item.state == WorkItemState::AwaitingStart {
                let _ = engine.start_step_with(id, task.item.version, &cause, None);
            }
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
            if let Some((option, note)) = self.cfg.operator.decide.clone() {
                for d in task
                    .decisions
                    .iter()
                    .filter(|d| d.state == saka_core::vocab::DecisionState::Asked)
                {
                    let _ = engine.answer_decision(d.id, option, note.clone(), &cause);
                }
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
