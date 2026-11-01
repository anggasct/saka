use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use saka_agents::launch::ClaudeSettings;
use saka_agents::{AgentsConfig, ClaudeAgents};
use saka_api::{ApiConfig, AppState};
use saka_core::policy::Policy;
use saka_core::task::LoopConfig;
use saka_core::time::Timestamp;
use saka_core::vocab::MergeMethod;
use saka_docs::GitDocs;
use saka_engine::{Engine, EngineConfig, Integration, Ports};
use saka_env::{Git, GitRepos, ReposConfig, SystemdSupervisor};
use saka_ports::{
    ChangeSnapshot, Clock, GitAccess, GitRemote, Ids, MergeOutcome, MergeRequest,
    OpenChangeRequest, OpenedChange, PortError, PortResult, PushOutcome, PushRequest, RepoInfo,
    RepoRef, Scm, UnitLimits,
};
use saka_scm::{GitHub, GitHubApp, GitHubConfig};
use saka_store::{Store, StoreError};
use serde_json::json;
use tokio::sync::watch;
use uuid::Uuid;

use crate::config::{self, Config, ConfigError};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const WORKERS: usize = 4;

pub const EXIT_REFUSED: u8 = 2;

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp(jiff::Timestamp::now().as_millisecond())
    }
}

pub struct UuidV7;

impl Ids for UuidV7 {
    fn new_id(&self) -> Uuid {
        Uuid::now_v7()
    }
}

fn unknown_integration(id: &str) -> PortError {
    PortError::permanent("scm_integration_unknown", json!({ "integration_id": id }))
}

struct ScmRouter(HashMap<String, Arc<GitHub>>);

impl ScmRouter {
    fn get(&self, id: &str) -> PortResult<&Arc<GitHub>> {
        self.0.get(id).ok_or_else(|| unknown_integration(id))
    }
}

#[async_trait]
impl Scm for ScmRouter {
    async fn repository(&self, integration_id: &str, full_name: &str) -> PortResult<RepoInfo> {
        self.get(integration_id)?
            .repository(integration_id, full_name)
            .await
    }
    async fn push_branch(&self, request: &PushRequest) -> PortResult<PushOutcome> {
        self.get(&request.repo.integration_id)?
            .push_branch(request)
            .await
    }
    async fn open_change_request(&self, request: &OpenChangeRequest) -> PortResult<OpenedChange> {
        self.get(&request.repo.integration_id)?
            .open_change_request(request)
            .await
    }
    async fn change_request(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSnapshot> {
        self.get(&repo.integration_id)?
            .change_request(repo, number)
            .await
    }
    async fn merge(&self, request: &MergeRequest) -> PortResult<MergeOutcome> {
        self.get(&request.repo.integration_id)?.merge(request).await
    }
    async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> PortResult<()> {
        self.get(&repo.integration_id)?
            .delete_branch(repo, branch)
            .await
    }
}

struct AccessRouter(HashMap<String, Arc<GitHubApp>>);

#[async_trait]
impl GitAccess for AccessRouter {
    async fn remote(&self, repo: &RepoRef) -> PortResult<GitRemote> {
        match self.0.get(&repo.integration_id) {
            Some(app) => app.remote(repo).await,
            None => Err(unknown_integration(&repo.integration_id)),
        }
    }
    async fn refused(&self, repo: &RepoRef) {
        if let Some(app) = self.0.get(&repo.integration_id) {
            app.refused(repo).await;
        }
    }
}

fn refuse(e: &ConfigError) -> ExitCode {
    tracing::error!(code = e.code, detail = %e.detail, "config refused");
    ExitCode::from(EXIT_REFUSED)
}

fn open_store(data_dir: &Path, now: Timestamp) -> Result<Store, ExitCode> {
    if let Err(e) = std::fs::create_dir_all(data_dir) {
        tracing::error!(code = "data_dir_unwritable", error = %e, path = %data_dir.display(), "cannot create the data directory");
        return Err(ExitCode::FAILURE);
    }
    match Store::open(&data_dir.join("saka.db"), &data_dir.join("backups"), now) {
        Ok(store) => {
            let m = store.migration();
            tracing::info!(applied = ?m.applied, backup = ?m.backup.as_ref().map(|p| p.display().to_string()), "store ready");
            Ok(store)
        }
        Err(
            e @ (StoreError::MigrationChecksumMismatch { .. }
            | StoreError::MigrationVersionAhead { .. }),
        ) => {
            tracing::error!(code = e.code(), error = %e, "migration refused");
            Err(ExitCode::from(EXIT_REFUSED))
        }
        Err(e) => {
            tracing::error!(code = e.code(), error = %e, "cannot open the store");
            Err(ExitCode::FAILURE)
        }
    }
}

pub fn migrate(cfg: &Config) -> ExitCode {
    match open_store(&cfg.data_dir(), SystemClock.now()) {
        Ok(_) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

fn loop_config(cfg: &Config) -> LoopConfig {
    let first = cfg.scm.first();
    LoopConfig {
        max_followup_turns: cfg.agent.max_followup_turns,
        max_running_ms: cfg.agent.max_running_minutes * 60_000,
        ci_appear_timeout_ms: first.map_or(600, |s| {
            i64::try_from(s.ci_appear_timeout_secs).unwrap_or(600)
        }) * 1000,
        poll_interval_ms: first.map_or(15, |s| i64::try_from(s.poll_interval_secs).unwrap_or(15))
            * 1000,
        merge_method: first.map_or(MergeMethod::Squash, |s| s.merge_method),
    }
}

fn runtime_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
        || std::path::PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw())),
        std::path::PathBuf::from,
    )
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        tracing::error!("cannot listen for signals");
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

pub fn serve(cfg: &Config) -> ExitCode {
    let listen = match cfg.listen() {
        Ok(a) => a,
        Err(e) => return refuse(&e),
    };
    if !config::is_loopback(&listen) {
        tracing::error!(code = "non_loopback_requires_auth", listen = %listen, "listening beyond loopback needs auth, which this version does not have");
        return ExitCode::from(EXIT_REFUSED);
    }
    if let Err(e) = cfg.check_key_files() {
        return refuse(&e);
    }
    let policy = match Policy::compile(&cfg.permission.rule) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(code = "config_invalid", error = %e, "permission rules refused");
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "cannot start the runtime");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(run(cfg, listen, policy))
}

async fn run(cfg: &Config, listen: std::net::SocketAddr, policy: Policy) -> ExitCode {
    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(code = "listen_failed", error = %e, listen = %listen, "cannot listen");
            return ExitCode::FAILURE;
        }
    };
    let mut api_cfg = ApiConfig::new(VERSION);
    api_cfg.allowed_origins = cfg.server.allowed_origins.clone();
    let state = AppState::new(api_cfg);
    if let Some(assets) = saka_api::spa::embedded() {
        state.set_assets(assets);
    }
    let (stop, stopped) = watch::channel(false);
    let mut http_stop = stopped.clone();
    let http = tokio::spawn(
        axum::serve(listener, saka_api::router(state.clone()))
            .with_graceful_shutdown(async move {
                let _ = http_stop.wait_for(|s| *s).await;
            })
            .into_future(),
    );
    tracing::info!(listen = %listen, version = VERSION, "listening");

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let data_dir = cfg.data_dir();
    let store = match open_store(&data_dir, clock.now()) {
        Ok(s) => Arc::new(s),
        Err(code) => {
            let _ = stop.send(true);
            let _ = http.await;
            return code;
        }
    };

    let mut apps = HashMap::new();
    let mut integrations = Vec::new();
    for s in &cfg.scm {
        let app = match GitHubApp::new(
            GitHubConfig {
                integration_id: s.id.clone(),
                api_url: s.api_url.clone(),
                web_url: s.web_url.clone(),
                app_id: s.app_id,
                private_key_file: config::expand(&s.private_key_file),
                user_agent: format!("saka/{VERSION}"),
            },
            clock.clone(),
        ) {
            Ok(a) => Arc::new(a),
            Err(e) => {
                tracing::error!(code = %e.code, "scm integration refused");
                let _ = stop.send(true);
                let _ = http.await;
                return ExitCode::from(EXIT_REFUSED);
            }
        };
        apps.insert(s.id.clone(), app);
        integrations.push(Integration {
            id: s.id.clone(),
            provider: s.provider.clone(),
            web_url: s.web_url.clone(),
        });
    }
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
    let repos = Arc::new(GitRepos::new(
        ReposConfig {
            repos_dir: data_dir.join("repos"),
            envs_dir: data_dir.join("envs"),
            git: Git::new("git", path),
        },
        Arc::new(AccessRouter(apps.clone())),
    ));
    let scm = ScmRouter(
        apps.iter()
            .map(|(id, app)| {
                (
                    id.clone(),
                    Arc::new(GitHub::new(app.clone(), repos.clone())),
                )
            })
            .collect(),
    );
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let agents = ClaudeAgents::new(
        AgentsConfig {
            claude: ClaudeSettings {
                command: cfg.agent.command.clone(),
                account_dir: cfg.account_dir().to_string_lossy().into_owned(),
                model: Some(cfg.agent.model.clone()).filter(|m| !m.is_empty()),
                path: config::paths_of(&cfg.agent.path),
                author_name: cfg.git.author_name.clone(),
                author_email: cfg.git.author_email.clone(),
            },
            sessions_dir: data_dir.join("sessions"),
            runtime_dir: runtime_dir(),
            runner_program: std::env::current_exe().unwrap_or_else(|_| "saka".into()),
            limits: UnitLimits {
                memory_max: cfg.agent.limits.memory_max.clone(),
                cpu_quota: cfg.agent.limits.cpu_quota.clone(),
                tasks_max: cfg.agent.limits.tasks_max,
            },
            known_values: Vec::new(),
            server_version: VERSION.to_string(),
            extra_env: Vec::new(),
        },
        Arc::new(SystemdSupervisor),
        events_tx,
    );
    let engine = Arc::new(Engine::new(
        store,
        Ports {
            clock,
            ids: Arc::new(UuidV7),
            env: repos.clone(),
            scm: Arc::new(scm),
            agents: Arc::new(agents),
            docs: Arc::new(GitDocs::new(repos)),
        },
        EngineConfig {
            loop_cfg: loop_config(cfg),
            max_active_tasks: cfg.scheduler.max_active_tasks,
            policy,
            worker_id: format!("serve-{}", std::process::id()),
            integrations,
        },
    ));
    match engine.recover() {
        Ok(lost) => tracing::info!(sessions_lost = lost, "recovered"),
        Err(e) => tracing::error!(error = %e, "recovery failed"),
    }
    state.set_engine(engine.clone());
    let forwarder = {
        let engine = engine.clone();
        tokio::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                if let Err(e) = engine.on_runner_event(event) {
                    tracing::warn!(error = %e, "runner event not recorded");
                }
            }
        })
    };
    let running = tokio::spawn(engine.clone().run(WORKERS, stopped));
    shutdown_signal().await;
    tracing::info!("shutting down");
    let _ = stop.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(30), running).await;
    let _ = http.await;
    forwarder.abort();
    ExitCode::SUCCESS
}
