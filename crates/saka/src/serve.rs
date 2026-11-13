use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use saka_agents::launch::{self, ClaudeSettings, UnsupportedPath};
use saka_agents::{AgentsConfig, ClaudeAgents};
use saka_api::{ApiConfig, AppState};
use saka_core::diagnostics::{CredentialStatus, RepoGrant, RepoHooks, WebhookStatus};
use saka_core::ids::SessionId;
use saka_core::model::Budget;
use saka_core::policy::Policy;
use saka_core::secret::SecretText;
use saka_core::task::LoopConfig;
use saka_core::time::Timestamp;
use saka_core::vocab::{MergeMethod, Stage};
use saka_docs::GitDocs;
use saka_engine::{DiagnosticLimits, Engine, EngineConfig, EnvConfig, Integration, Ports};
use saka_env::{
    AeadSecrets, Git, GitRepos, HttpReleaseIndex, Platform, RecipeRunner, ReleaseIndexConfig,
    ReposConfig, SystemdSupervisor, ToolchainConfig, ToolchainInstaller, runtime_dir,
};
use saka_ports::BestEffort;
use saka_ports::{
    ChangeOpening, ChangeSignal, ChangeSnapshot, ChangeSummary, Clock, DeviceCode, DevicePoll,
    EnqueueOutcome, GitAccess, GitRemote, IdentityLookup, Ids, IntakeCapabilities, IntakeSource,
    Issue, IssueCloseOutcome, IssueListing, IssueRef, IssueSignal, MergeOutcome, MergeRequest,
    OpenChangeRequest, PortError, PortResult, PostedReview, PushOutcome, PushRequest, RepoAccess,
    RepoInfo, RepoRef, Repos, RerunOutcome, ReviewPost, Scm, ScmCapabilities, StatusReport,
    TokenRefresh, UnitLimits, UpdateOutcome, WebhookDelivery,
};
use saka_scm::{GitHub, GitHubApp, GitHubConfig, GitLab, GitLabClient, GitLabConfig};
use saka_store::{Store, StoreError};
use serde_json::json;
use tokio::sync::watch;
use uuid::Uuid;

use crate::config::{self, Config, ConfigError, ScmEntry};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const WORKERS: usize = 4;

pub const EXIT_REFUSED: u8 = 2;

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp(jiff::Timestamp::now().as_millisecond())
    }
}

pub struct OsEntropy;

impl saka_ports::Entropy for OsEntropy {
    fn fill(&self, buf: &mut [u8]) -> PortResult<()> {
        getrandom::fill(buf).map_err(|_| PortError::permanent("entropy_unavailable", json!({})))
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

struct IntakeRouter(HashMap<String, Arc<dyn IntakeSource>>);

impl IntakeRouter {
    fn get(&self, id: &str) -> PortResult<&Arc<dyn IntakeSource>> {
        self.0.get(id).ok_or_else(|| unknown_integration(id))
    }
}

#[async_trait]
impl IntakeSource for IntakeRouter {
    fn capabilities(&self, integration_id: &str) -> Option<IntakeCapabilities> {
        self.0.get(integration_id)?.capabilities(integration_id)
    }
    async fn list_issues(
        &self,
        repo: &RepoRef,
        updated_since: saka_core::time::Timestamp,
        labels: &[String],
    ) -> PortResult<IssueListing> {
        self.get(&repo.integration_id)?
            .list_issues(repo, updated_since, labels)
            .await
    }
    async fn issue(&self, issue: &IssueRef) -> PortResult<Issue> {
        self.get(&issue.repo.integration_id)?.issue(issue).await
    }
    async fn issue_signals(
        &self,
        issue: &IssueRef,
        since: saka_core::time::Timestamp,
    ) -> PortResult<Vec<IssueSignal>> {
        self.get(&issue.repo.integration_id)?
            .issue_signals(issue, since)
            .await
    }
    async fn can_write(&self, repo: &RepoRef, login: &str) -> PortResult<bool> {
        IntakeSource::can_write(self.get(&repo.integration_id)?.as_ref(), repo, login).await
    }
    async fn post_issue_comment(&self, issue: &IssueRef, body: &str) -> PortResult<()> {
        self.get(&issue.repo.integration_id)?
            .post_issue_comment(issue, body)
            .await
    }
    async fn close_issue(&self, issue: &IssueRef) -> PortResult<IssueCloseOutcome> {
        self.get(&issue.repo.integration_id)?
            .close_issue(issue)
            .await
    }
}

struct ScmRouter(HashMap<String, Arc<dyn Scm>>);

impl ScmRouter {
    fn get(&self, id: &str) -> PortResult<&Arc<dyn Scm>> {
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
    async fn branch_head(
        &self,
        integration_id: &str,
        full_name: &str,
        branch: &str,
    ) -> PortResult<Option<String>> {
        self.get(integration_id)?
            .branch_head(integration_id, full_name, branch)
            .await
    }
    async fn file_at(
        &self,
        integration_id: &str,
        full_name: &str,
        commit: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<Option<Vec<u8>>> {
        self.get(integration_id)?
            .file_at(integration_id, full_name, commit, path, max_bytes)
            .await
    }
    async fn push_branch(&self, request: &PushRequest) -> PortResult<PushOutcome> {
        self.get(&request.repo.integration_id)?
            .push_branch(request)
            .await
    }
    async fn open_change_request(&self, request: &OpenChangeRequest) -> PortResult<ChangeOpening> {
        self.get(&request.repo.integration_id)?
            .open_change_request(request)
            .await
    }
    async fn change_request(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSnapshot> {
        self.get(&repo.integration_id)?
            .change_request(repo, number)
            .await
    }
    async fn commit_checks(
        &self,
        repo: &RepoRef,
        sha: &str,
    ) -> PortResult<Vec<saka_core::gate::Check>> {
        self.get(&repo.integration_id)?
            .commit_checks(repo, sha)
            .await
    }
    async fn rerun_failed_checks(
        &self,
        repo: &RepoRef,
        head_sha: &str,
    ) -> PortResult<RerunOutcome> {
        self.get(&repo.integration_id)?
            .rerun_failed_checks(repo, head_sha)
            .await
    }
    async fn list_open_changes(
        &self,
        repo: &RepoRef,
        max: usize,
    ) -> PortResult<Vec<ChangeSummary>> {
        self.get(&repo.integration_id)?
            .list_open_changes(repo, max)
            .await
    }
    async fn change_summary(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSummary> {
        self.get(&repo.integration_id)?
            .change_summary(repo, number)
            .await
    }
    async fn change_files(
        &self,
        repo: &RepoRef,
        number: u64,
        max: usize,
    ) -> PortResult<Vec<String>> {
        self.get(&repo.integration_id)?
            .change_files(repo, number, max)
            .await
    }
    async fn change_signals(
        &self,
        repo: &RepoRef,
        number: u64,
        since: saka_core::time::Timestamp,
    ) -> PortResult<Vec<ChangeSignal>> {
        self.get(&repo.integration_id)?
            .change_signals(repo, number, since)
            .await
    }
    async fn can_write(&self, repo: &RepoRef, login: &str) -> PortResult<bool> {
        self.get(&repo.integration_id)?.can_write(repo, login).await
    }
    async fn post_review(&self, post: &ReviewPost) -> PortResult<PostedReview> {
        self.get(&post.repo.integration_id)?.post_review(post).await
    }
    async fn post_comment(&self, repo: &RepoRef, number: u64, body: &str) -> PortResult<()> {
        self.get(&repo.integration_id)?
            .post_comment(repo, number, body)
            .await
    }
    async fn update_change_request(
        &self,
        repo: &RepoRef,
        number: u64,
        body: &str,
    ) -> PortResult<()> {
        self.get(&repo.integration_id)?
            .update_change_request(repo, number, body)
            .await
    }
    async fn update_branch(
        &self,
        repo: &RepoRef,
        number: u64,
        expected_head_sha: &str,
    ) -> PortResult<UpdateOutcome> {
        self.get(&repo.integration_id)?
            .update_branch(repo, number, expected_head_sha)
            .await
    }
    async fn merge(&self, request: &MergeRequest) -> PortResult<MergeOutcome> {
        self.get(&request.repo.integration_id)?.merge(request).await
    }
    async fn merge_queue(&self, repo: &RepoRef, base_branch: &str) -> PortResult<bool> {
        self.get(&repo.integration_id)?
            .merge_queue(repo, base_branch)
            .await
    }
    async fn enqueue(&self, request: &MergeRequest) -> PortResult<EnqueueOutcome> {
        self.get(&request.repo.integration_id)?
            .enqueue(request)
            .await
    }
    async fn in_merge_queue(&self, repo: &RepoRef, number: u64) -> PortResult<bool> {
        self.get(&repo.integration_id)?
            .in_merge_queue(repo, number)
            .await
    }
    async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> PortResult<()> {
        self.get(&repo.integration_id)?
            .delete_branch(repo, branch)
            .await
    }
    async fn report_status(
        &self,
        repo: &RepoRef,
        sha: &str,
        report: &StatusReport,
    ) -> PortResult<()> {
        self.get(&repo.integration_id)?
            .report_status(repo, sha, report)
            .await
    }
    fn capabilities(&self, integration_id: &str) -> Option<ScmCapabilities> {
        self.0.get(integration_id)?.capabilities(integration_id)
    }
    async fn credential_status(&self, integration_id: &str) -> PortResult<CredentialStatus> {
        self.get(integration_id)?
            .credential_status(integration_id)
            .await
    }
    async fn repo_grant(&self, repo: &RepoRef) -> PortResult<RepoGrant> {
        self.get(&repo.integration_id)?.repo_grant(repo).await
    }
    async fn repo_webhooks(&self, repo: &RepoRef) -> PortResult<RepoHooks> {
        self.get(&repo.integration_id)?.repo_webhooks(repo).await
    }
    async fn webhook_status(&self, integration_id: &str) -> PortResult<WebhookStatus> {
        self.get(integration_id)?
            .webhook_status(integration_id)
            .await
    }
    fn webhook_configured(&self, integration_id: &str) -> PortResult<bool> {
        self.get(integration_id)?.webhook_configured(integration_id)
    }
    fn parse_webhook(
        &self,
        integration_id: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> PortResult<WebhookDelivery> {
        self.get(integration_id)?
            .parse_webhook(integration_id, headers, body)
    }
    fn device_flow_configured(&self, integration_id: &str) -> PortResult<bool> {
        self.get(integration_id)?
            .device_flow_configured(integration_id)
    }
    async fn start_device_flow(&self, integration_id: &str) -> PortResult<DeviceCode> {
        self.get(integration_id)?
            .start_device_flow(integration_id)
            .await
    }
    async fn poll_device_flow(
        &self,
        integration_id: &str,
        device_code: &SecretText,
    ) -> PortResult<DevicePoll> {
        self.get(integration_id)?
            .poll_device_flow(integration_id, device_code)
            .await
    }
    async fn refresh_user_token(
        &self,
        integration_id: &str,
        refresh_token: &SecretText,
    ) -> PortResult<TokenRefresh> {
        self.get(integration_id)?
            .refresh_user_token(integration_id, refresh_token)
            .await
    }
    async fn user_identity(
        &self,
        integration_id: &str,
        token: &SecretText,
    ) -> PortResult<IdentityLookup> {
        self.get(integration_id)?
            .user_identity(integration_id, token)
            .await
    }
    async fn user_repository_access(
        &self,
        integration_id: &str,
        full_name: &str,
        token: &SecretText,
    ) -> PortResult<RepoAccess> {
        self.get(integration_id)?
            .user_repository_access(integration_id, full_name, token)
            .await
    }
}

struct AccessRouter(HashMap<String, Arc<dyn GitAccess>>);

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

enum ScmClient {
    GitHub(Arc<GitHubApp>),
    GitLab(Arc<GitLabClient>),
}

impl ScmClient {
    fn access(&self) -> Arc<dyn GitAccess> {
        match self {
            ScmClient::GitHub(app) => app.clone(),
            ScmClient::GitLab(client) => client.clone(),
        }
    }

    fn adapters(
        self,
        repos: Arc<dyn Repos>,
        secret: Option<Vec<u8>>,
    ) -> (Arc<dyn Scm>, Arc<dyn IntakeSource>) {
        match self {
            ScmClient::GitHub(app) => {
                let github = GitHub::new(app, repos);
                let github = Arc::new(match secret {
                    Some(secret) => github.with_webhook_secret(secret),
                    None => github,
                });
                (github.clone(), github)
            }
            ScmClient::GitLab(client) => {
                let gitlab = GitLab::new(client, repos);
                let gitlab = Arc::new(match secret {
                    Some(secret) => gitlab.with_webhook_secret(secret),
                    None => gitlab,
                });
                (gitlab.clone(), gitlab)
            }
        }
    }
}

fn scm_client(s: &ScmEntry, clock: Arc<dyn Clock>) -> PortResult<ScmClient> {
    let missing =
        |field: &str| PortError::permanent("scm_config_invalid", json!({ "field": field }));
    let user_agent = format!("saka/{VERSION}");
    match s.provider.as_str() {
        "github" => {
            let app_id = s.app_id.ok_or_else(|| missing("app_id"))?;
            let key = s
                .private_key_file
                .as_deref()
                .ok_or_else(|| missing("private_key_file"))?;
            let app = GitHubApp::new(
                GitHubConfig {
                    integration_id: s.id.clone(),
                    api_url: s.api_url.clone(),
                    web_url: s.web_url.clone(),
                    app_id,
                    private_key_file: config::expand(key),
                    user_agent,
                    client_id: s.client_id.clone(),
                },
                clock,
            )?;
            Ok(ScmClient::GitHub(Arc::new(app)))
        }
        "gitlab" => {
            let token = s
                .token_file
                .as_deref()
                .ok_or_else(|| missing("token_file"))?;
            let client = GitLabClient::new(
                GitLabConfig {
                    integration_id: s.id.clone(),
                    api_url: s.api_url.clone(),
                    web_url: s.web_url.clone(),
                    token_file: config::expand(token),
                    user_agent,
                },
                clock,
            )?;
            Ok(ScmClient::GitLab(Arc::new(client)))
        }
        _ => Err(missing("provider")),
    }
}

fn refuse(e: &ConfigError) -> ExitCode {
    tracing::error!(code = e.code, detail = %e.detail, "config refused");
    ExitCode::from(EXIT_REFUSED)
}

fn check_envs_dir(envs_dir: &Path) -> Result<(), ExitCode> {
    let physical = std::fs::create_dir_all(envs_dir).and_then(|()| std::fs::canonicalize(envs_dir));
    let physical = match physical {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(code = "data_dir_unwritable", error = %e, path = %envs_dir.display(), "cannot create the environments directory");
            return Err(ExitCode::FAILURE);
        }
    };
    launch::memory_scope(&physical).map(drop).map_err(|UnsupportedPath(path)| {
        tracing::error!(code = "envs_dir_unsupported", path = %path.display(), "the environments directory has a character agent sessions cannot scope their memory to");
        ExitCode::from(EXIT_REFUSED)
    })
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

pub const CLI_TOKEN_FILE: &str = "cli-token";

fn publish_cli_token(engine: &Engine, data_dir: &Path) -> Result<(), ExitCode> {
    let refused = |code: &'static str, detail: String| {
        tracing::error!(code, detail = %detail, "cannot publish the CLI token");
        ExitCode::from(EXIT_REFUSED)
    };
    let token = engine
        .issue_local_cli_token()
        .map_err(|e| refused("cli_token_unavailable", e.to_string()))?;
    write_private_file(&data_dir.join(CLI_TOKEN_FILE), token.expose())
        .map_err(|e| refused("cli_token_unwritable", e.to_string()))
}

fn write_private_file(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::remove_file(&temp).best_effort("remove a temp file left by an earlier write");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let written = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    match written.and_then(|()| std::fs::rename(&temp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            std::fs::remove_file(&temp).best_effort("remove the temp file of a failed write");
            Err(e)
        }
    }
}

pub fn migrate(cfg: &Config) -> ExitCode {
    match open_store(&cfg.data_dir(), SystemClock.now()) {
        Ok(_) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

fn env_config(cfg: &Config, store: &Store) -> Result<EnvConfig, ConfigError> {
    let (port_first, port_last) = cfg.env.ports()?;
    let env = EnvConfig {
        port_first,
        port_last,
        ports_per_env: cfg.env.ports_per_env,
    };
    let live = store
        .read(saka_store::read::live_port_blocks)
        .map_err(|e| ConfigError {
            code: e.code(),
            detail: e.to_string(),
        })?;
    let fits = |(start, count): &(u16, u16)| {
        *count == env.ports_per_env
            && *start >= port_first
            && (*start - port_first) % env.ports_per_env == 0
            && usize::from((*start - port_first) / env.ports_per_env) < env.port_blocks()
    };
    match live.iter().find(|b| !fits(b)) {
        None => Ok(env),
        Some((start, count)) => Err(ConfigError {
            code: "config_port_layout_in_use",
            detail: format!(
                "a live environment holds ports {start}+{count}; tear it down before changing env.port_range or env.ports_per_env"
            ),
        }),
    }
}

pub fn claude_settings(cfg: &Config) -> ClaudeSettings {
    ClaudeSettings {
        command: cfg.agent.command.clone(),
        account: cfg.account_name(),
        account_dir: cfg.account_dir().to_string_lossy().into_owned(),
        model: Some(cfg.agent.model.clone()).filter(|m| !m.is_empty()),
        path: config::paths_of(&cfg.agent.path),
        committer: saka_identity(cfg),
    }
}

pub fn agents_config(cfg: &Config) -> AgentsConfig {
    AgentsConfig {
        claude: claude_settings(cfg),
        sessions_dir: cfg.data_dir().join("sessions"),
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
        mcp_socket: Some(runtime_dir().join("saka").join("mcp.sock")),
    }
}

pub fn toolchain_config(cfg: &Config) -> ToolchainConfig {
    let data_dir = cfg.data_dir();
    ToolchainConfig {
        root: data_dir.join("toolchains"),
        caches: data_dir.join("caches"),
        go_dl_url: cfg.toolchain.go_dl_url.clone(),
        node_dist_url: cfg.toolchain.node_dist_url.clone(),
        npm_registry_url: cfg.toolchain.npm_registry_url.clone(),
        timeout: Duration::from_secs(cfg.toolchain.download_timeout_secs),
        user_agent: format!("saka/{VERSION}"),
        platform: Platform::current(),
        host_path: config::paths_of(&cfg.agent.path),
    }
}

pub fn release_index_config(cfg: &Config) -> ReleaseIndexConfig {
    ReleaseIndexConfig {
        go_dl_url: cfg.toolchain.go_dl_url.clone(),
        node_dist_url: cfg.toolchain.node_dist_url.clone(),
        npm_registry_url: cfg.toolchain.npm_registry_url.clone(),
        user_agent: format!("saka/{VERSION}"),
    }
}

fn saka_identity(cfg: &Config) -> saka_core::authorship::GitIdentity {
    saka_core::authorship::GitIdentity {
        name: cfg.git.name.clone(),
        email: cfg.git.email.clone(),
    }
}

fn micros_of(usd: f64) -> i64 {
    let micros = (usd * 1_000_000.0).round();
    if micros.is_finite() && micros < i64::MAX as f64 {
        micros as i64
    } else {
        0
    }
}

fn draft_config(cfg: &Config) -> saka_core::draft::DraftConfig {
    let budget = &cfg.budget.architect;
    saka_core::draft::DraftConfig {
        max_active: cfg.architect.max_active,
        lint_rounds: cfg.architect.lint_rounds,
        session_idle_ms: cfg.architect.session_idle_hours * 3_600_000,
        budget: Budget {
            running_minutes: budget.running_minutes,
            turns: budget.turns,
            tokens: budget.tokens,
            cost_usd_micros: micros_of(budget.cost_usd),
        },
        session: saka_core::session::SessionConfig {
            permission_wait_ms: cfg.agent.permission_wait_hours * 3_600_000,
            decision_wait_ms: cfg.agent.decision_wait_hours * 3_600_000,
            idle_ms: cfg.agent.idle_minutes * 60_000,
            budget_warn_percent: cfg.budget.warn_percent,
            rate_limit_pause: cfg.agent.rate_limit_pause,
            account: format!("{}:{}", cfg.agent.cli, cfg.account_name()),
        },
    }
}

fn loop_config(cfg: &Config) -> LoopConfig {
    let first = cfg.scm.first();
    let dev = &cfg.budget.dev;
    let review = &cfg.budget.review;
    let fix = &cfg.budget.fix;
    LoopConfig {
        max_followup_turns: cfg.agent.max_followup_turns,
        permission_wait_ms: cfg.agent.permission_wait_hours * 3_600_000,
        decision_wait_ms: cfg.agent.decision_wait_hours * 3_600_000,
        idle_ms: cfg.agent.idle_minutes * 60_000,
        budgets: BTreeMap::from([
            (
                Stage::Dev,
                Budget {
                    running_minutes: dev.running_minutes,
                    turns: dev.turns,
                    tokens: dev.tokens,
                    cost_usd_micros: micros_of(dev.cost_usd),
                },
            ),
            (
                Stage::Review,
                Budget {
                    running_minutes: review.running_minutes,
                    turns: review.turns,
                    tokens: review.tokens,
                    cost_usd_micros: micros_of(review.cost_usd),
                },
            ),
            (
                Stage::Fix,
                Budget {
                    running_minutes: fix.running_minutes,
                    turns: fix.turns,
                    tokens: fix.tokens,
                    cost_usd_micros: micros_of(fix.cost_usd),
                },
            ),
        ]),
        budget_warn_percent: cfg.budget.warn_percent,
        rate_limit_pause: cfg.agent.rate_limit_pause,
        account: format!("{}:{}", cfg.agent.cli, cfg.account_name()),
        autonomy: cfg.autonomy_ceilings(),
        max_ci_reruns: cfg.scheduler.max_ci_reruns,
        ci_appear_timeout_ms: first.map_or(600, |s| {
            i64::try_from(s.ci_appear_timeout_secs).unwrap_or(600)
        }) * 1000,
        poll_interval_ms: first.map_or(15, |s| i64::try_from(s.poll_interval_secs).unwrap_or(15))
            * 1000,
        merge_method: first.map_or(MergeMethod::Squash, |s| s.merge_method),
        delete_branch_after_merge: true,
        commit_status: true,
    }
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
    if !config::is_loopback(&listen) && cfg.auth.is_none() {
        tracing::error!(code = "config_listen_not_loopback", listen = %listen, "listening beyond loopback needs an [auth] section");
        return ExitCode::from(EXIT_REFUSED);
    }
    if let Err(e) = cfg
        .check_key_files()
        .and_then(|()| cfg.check_incident_secrets())
    {
        return refuse(&e);
    }
    let hooks = match cfg.hooks.as_ref().map(|_| cfg.hooks_listen()).transpose() {
        Ok(h) => h,
        Err(e) => return refuse(&e),
    };
    let webhook_secrets = match cfg.webhook_secrets() {
        Ok(s) => s,
        Err(e) => return refuse(&e),
    };
    let policy = match Policy::compile(&cfg.permission.rule) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(code = "config_invalid", error = %e, "permission rules refused");
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    if let Err(code) = check_envs_dir(&cfg.data_dir().join("envs")) {
        return code;
    }
    let Some(key_file) = config::secret_key_path() else {
        tracing::error!(
            code = "config_key_file_unreadable",
            "no config directory for the secret key"
        );
        return ExitCode::from(EXIT_REFUSED);
    };
    let secrets = match AeadSecrets::from_key_file(&key_file) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            tracing::error!(code = e.code, detail = %e.detail, "secret key refused");
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
    let webhooks = Webhooks {
        listen: hooks,
        secrets: webhook_secrets.into_iter().collect(),
    };
    runtime.block_on(run(cfg, listen, policy, secrets, webhooks))
}

struct Webhooks {
    listen: Option<std::net::SocketAddr>,
    secrets: HashMap<String, Vec<u8>>,
}

fn capability_refusal(
    cfg: &Config,
    caps_of: impl Fn(&str) -> Option<ScmCapabilities>,
) -> Option<(&'static str, String)> {
    cfg.scm.iter().find_map(|s| {
        let caps = caps_of(&s.id)?;
        if !caps.merge_methods.contains(&s.merge_method) {
            return Some(("config_merge_method_unsupported", s.id.clone()));
        }
        if s.webhook_secret_file.is_some() && caps.webhook_signature.is_none() {
            return Some(("config_webhook_unsupported", s.id.clone()));
        }
        if s.client_id.is_some() && !caps.user_authorship {
            return Some(("config_user_authorship_unsupported", s.id.clone()));
        }
        None
    })
}

async fn stop_http<T>(stop: &watch::Sender<bool>, http: tokio::task::JoinHandle<T>) {
    stop.send(true).best_effort("signal the stop");
    http.await.best_effort("wait for the HTTP server");
}

async fn run(
    cfg: &Config,
    listen: std::net::SocketAddr,
    policy: Policy,
    secrets: Arc<AeadSecrets>,
    mut webhooks: Webhooks,
) -> ExitCode {
    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(code = "listen_failed", error = %e, listen = %listen, "cannot listen");
            return ExitCode::FAILURE;
        }
    };
    let mut api_cfg = ApiConfig::new(VERSION);
    api_cfg.allowed_origins = cfg.server.allowed_origins.clone();
    match cfg
        .auth_settings()
        .map(saka_api::Authenticator::new)
        .transpose()
    {
        Ok(auth) => api_cfg.auth = auth,
        Err(detail) => {
            tracing::error!(code = "config_auth_invalid", detail = %detail, "auth refused");
            return ExitCode::from(EXIT_REFUSED);
        }
    }
    let state = AppState::new(api_cfg);
    if let Some(assets) = saka_api::spa::embedded() {
        state.set_assets(assets);
    }
    let (stop, stopped) = watch::channel(false);
    let mut http_stop = stopped.clone();
    let http = tokio::spawn(
        axum::serve(
            listener,
            saka_api::router(state.clone())
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            http_stop
                .wait_for(|s| *s)
                .await
                .best_effort("wait for the stop signal");
        })
        .into_future(),
    );
    tracing::info!(listen = %listen, version = VERSION, "listening");
    let hooks_http = match webhooks.listen {
        Some(at) => match tokio::net::TcpListener::bind(at).await {
            Ok(l) => {
                let mut hooks_stop = stopped.clone();
                tracing::info!(listen = %at, "listening for webhooks");
                Some(tokio::spawn(
                    axum::serve(l, saka_api::hooks_router(state.clone()))
                        .with_graceful_shutdown(async move {
                            hooks_stop
                                .wait_for(|s| *s)
                                .await
                                .best_effort("wait for the stop signal");
                        })
                        .into_future(),
                ))
            }
            Err(e) => {
                tracing::error!(code = "listen_failed", error = %e, listen = %at, "cannot listen for webhooks");
                stop_http(&stop, http).await;
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let data_dir = cfg.data_dir();
    let store = match open_store(&data_dir, clock.now()) {
        Ok(s) => Arc::new(s),
        Err(code) => {
            stop_http(&stop, http).await;
            return code;
        }
    };
    let env_cfg = match env_config(cfg, &store) {
        Ok(c) => c,
        Err(e) => {
            stop_http(&stop, http).await;
            return refuse(&e);
        }
    };

    let mut clients = HashMap::new();
    let mut integrations = Vec::new();
    for s in &cfg.scm {
        let client = match scm_client(s, clock.clone()) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(code = %e.code, "scm integration refused");
                stop_http(&stop, http).await;
                return ExitCode::from(EXIT_REFUSED);
            }
        };
        clients.insert(s.id.clone(), client);
        integrations.push(Integration {
            id: s.id.clone(),
            provider: s.provider.clone(),
            web_url: s.web_url.clone(),
        });
    }
    let toolchains = match ToolchainInstaller::new(toolchain_config(cfg)) {
        Ok(t) => Arc::new(t),
        Err(e) => {
            tracing::error!(code = %e.code, "toolchain installer refused");
            stop_http(&stop, http).await;
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
    let repos = Arc::new(GitRepos::new(
        ReposConfig {
            repos_dir: data_dir.join("repos"),
            envs_dir: data_dir.join("envs"),
            git: Git::new("git", path),
            recipe: RecipeRunner {
                supervisor: Arc::new(SystemdSupervisor),
                limits: UnitLimits {
                    memory_max: cfg.env.limits.memory_max.clone(),
                    cpu_quota: cfg.env.limits.cpu_quota.clone(),
                    tasks_max: cfg.env.limits.tasks_max,
                },
                timeout: Duration::from_secs(u64::from(cfg.env.setup_timeout_minutes) * 60),
                logs_dir: data_dir.join("logs").join("environments"),
                path: config::paths_of(&cfg.agent.path),
            },
        },
        Arc::new(AccessRouter(
            clients
                .iter()
                .map(|(id, client)| (id.clone(), client.access()))
                .collect(),
        )),
    ));
    let mut scm_adapters = HashMap::new();
    let mut intake_adapters = HashMap::new();
    for (id, client) in clients {
        let secret = webhooks.secrets.remove(&id);
        let (scm, intake) = client.adapters(repos.clone(), secret);
        scm_adapters.insert(id.clone(), scm);
        intake_adapters.insert(id, intake);
    }
    let scm = ScmRouter(scm_adapters);
    let intake = IntakeRouter(intake_adapters);
    if let Some((code, integration)) = capability_refusal(cfg, |id| scm.capabilities(id)) {
        tracing::error!(code, integration = %integration, "the integration lacks a capability the config asks for");
        stop_http(&stop, http).await;
        return ExitCode::from(EXIT_REFUSED);
    }
    let first_caps = cfg.scm.first().and_then(|s| scm.capabilities(&s.id));
    let delete_branch_after_merge = first_caps
        .as_ref()
        .is_none_or(|c| c.delete_branch_after_merge);
    let commit_status = first_caps.as_ref().is_some_and(|c| c.commit_status);
    let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
    let releases = match HttpReleaseIndex::new(release_index_config(cfg)) {
        Ok(r) => Arc::new(r),
        Err(e) => {
            tracing::error!(code = %e.code, "release index refused");
            stop_http(&stop, http).await;
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    let hooks_cfg = match cfg.hooks_parsed() {
        Ok(h) => h,
        Err(e) => {
            tracing::error!(code = e.code, detail = %e.detail, "hook config refused");
            stop_http(&stop, http).await;
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    let environments_cfg = match cfg.environments_parsed() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(code = e.code, detail = %e.detail, "deploy config refused");
            stop_http(&stop, http).await;
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    let incident_signals = match cfg.incident_signals() {
        Ok(signals) => signals,
        Err(e) => {
            tracing::error!(code = e.code, detail = %e.detail, "incident config refused");
            stop_http(&stop, http).await;
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    let incident_sources_cfg = match cfg.incident_sources_parsed() {
        Ok(sources) => sources,
        Err(e) => {
            tracing::error!(code = e.code, detail = %e.detail, "incident config refused");
            stop_http(&stop, http).await;
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    let outbound = match saka_notify::HttpWebhooks::new(format!("saka/{VERSION}")) {
        Ok(w) => Arc::new(w),
        Err(e) => {
            tracing::error!(code = %e.code, "outbound webhooks refused");
            stop_http(&stop, http).await;
            return ExitCode::from(EXIT_REFUSED);
        }
    };
    let mut chat_notifier = None;
    let notifier: Arc<dyn saka_ports::Notifier> = if cfg.notify.channel.is_empty() {
        Arc::new(saka_notify::UnconfiguredNotifier)
    } else {
        match cfg.chat_notifier(&format!("saka/{VERSION}"), &clock) {
            Ok(n) => {
                let n = Arc::new(n);
                chat_notifier = Some(n.clone());
                n
            }
            Err(e) => {
                tracing::error!(code = e.code, detail = %e.detail, "notify config refused");
                stop_http(&stop, http).await;
                return ExitCode::from(EXIT_REFUSED);
            }
        }
    };
    let agents = ClaudeAgents::new(agents_config(cfg), Arc::new(SystemdSupervisor), events_tx);
    let engine = Arc::new(Engine::new(
        store,
        Ports {
            clock,
            ids: Arc::new(UuidV7),
            entropy: Arc::new(OsEntropy),
            env: repos.clone(),
            scm: Arc::new(scm),
            intake: Arc::new(intake),
            agents: Arc::new(agents),
            docs: Arc::new(GitDocs::new(repos)),
            toolchains,
            secrets,
            supervisor: Arc::new(SystemdSupervisor),
            releases,
            webhooks: outbound,
            notifier,
            signals: Arc::new(incident_signals),
            host: Arc::new(saka_env::HostRunner {
                supervisor: Arc::new(SystemdSupervisor),
                limits: UnitLimits {
                    memory_max: cfg.env.limits.memory_max.clone(),
                    cpu_quota: cfg.env.limits.cpu_quota.clone(),
                    tasks_max: cfg.env.limits.tasks_max,
                },
                path: config::paths_of(&cfg.agent.path),
                scratch_root: data_dir.join("hooks"),
                logs_dir: data_dir.join("logs").join("hooks"),
            }),
        },
        EngineConfig {
            loop_cfg: LoopConfig {
                delete_branch_after_merge,
                commit_status,
                ..loop_config(cfg)
            },
            max_active_tasks: cfg.scheduler.max_active_tasks,
            policy,
            worker_id: format!("serve-{}", std::process::id()),
            integrations,
            env: env_cfg,
            git_identity: saka_identity(cfg),
            draft: draft_config(cfg),
            architect_model: Some(cfg.architect.model.clone()).filter(|m| !m.is_empty()),
            config_poll_secs: cfg.workspace.config_poll_secs,
            diagnostics: DiagnosticLimits::default(),
            hooks: hooks_cfg,
            environments: environments_cfg,
            incident_sources: incident_sources_cfg,
            auth_enabled: cfg.auth.is_some(),
            workflow_policy: saka_core::workflow::WorkflowPolicy {
                allow_unreviewed_chore: cfg.workflow.allow_unreviewed_chore,
            },
            public_url: cfg.notify.public_url.clone(),
        },
    ));
    match engine.recover() {
        Ok(lost) => tracing::info!(sessions_lost = lost, "recovered"),
        Err(e) => tracing::error!(error = %e, "recovery failed"),
    }
    if cfg.auth.is_some()
        && let Err(code) = publish_cli_token(&engine, &data_dir)
    {
        stop_http(&stop, http).await;
        return code;
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
    let mcp = start_mcp(cfg, &engine);
    let chat_pollers = chat_notifier.map_or_else(Vec::new, |chat| {
        let actions: Arc<dyn saka_ports::ChatActions> = engine.clone();
        chat.spawn_listeners(&actions, &stopped)
    });
    let running = tokio::spawn(engine.clone().run(WORKERS, stopped));
    shutdown_signal().await;
    tracing::info!("shutting down");
    stop.send(true).best_effort("signal the stop");
    tokio::time::timeout(Duration::from_secs(30), running)
        .await
        .best_effort("wait for the running jobs");
    http.await.best_effort("wait for the HTTP server");
    if let Some(hooks) = hooks_http {
        hooks.await.best_effort("wait for the webhook server");
    }
    for poller in chat_pollers {
        tokio::time::timeout(Duration::from_secs(5), poller)
            .await
            .best_effort("wait for the poller");
    }
    forwarder.abort();
    if let Some((task, socket)) = mcp {
        task.abort();
        std::fs::remove_file(socket).best_effort("remove the MCP socket");
    }
    ExitCode::SUCCESS
}

struct SessionTokens {
    sessions_dir: PathBuf,
}

impl saka_mcp::Tokens for SessionTokens {
    fn token(&self, session: SessionId) -> Option<SecretText> {
        let path = saka_agents::launch::mcp_token_path(&self.sessions_dir, &session.to_string());
        let text = std::fs::read_to_string(path).ok()?;
        Some(SecretText::new(text.trim().to_string()))
    }
}

fn start_mcp(cfg: &Config, engine: &Arc<Engine>) -> Option<(tokio::task::JoinHandle<()>, PathBuf)> {
    let socket = agents_config(cfg).mcp_socket?;
    let listener = match saka_mcp::bind(&socket) {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!(path = %socket.display(), error = %e, "mcp socket not started");
            return None;
        }
    };
    let tokens = Arc::new(SessionTokens {
        sessions_dir: cfg.data_dir().join("sessions"),
    });
    let decisions: Arc<dyn saka_ports::Decisions> = engine.clone();
    let regressions: Arc<dyn saka_ports::Regressions> = engine.clone();
    Some((
        tokio::spawn(saka_mcp::serve(listener, tokens, decisions, regressions)),
        socket,
    ))
}

#[cfg(test)]
mod capability_tests {
    use saka_core::vocab::{CheckKind, MergeMethod};

    use super::*;

    const LIMITED: ScmCapabilities = ScmCapabilities {
        provider: "limited",
        merge_methods: &[MergeMethod::Merge],
        webhook_signature: None,
        check_kinds: &[CheckKind::CommitStatus],
        draft_change_requests: false,
        delete_branch_after_merge: false,
        commit_status: false,
        merge_queue: false,
        update_branch: false,
        user_authorship: false,
        change_head_ref: "refs/pull/{number}/head",
        diagnostics: None,
    };

    fn config(extra: &str) -> Config {
        Config::parse(&format!(
            "[[scm]]\nid = \"github\"\nprovider = \"github\"\napp_id = 1\nprivate_key_file = \"/k\"\n{extra}"
        ))
        .unwrap()
    }

    fn gitlab(extra: &str) -> Config {
        Config::parse(&format!(
            "[[scm]]\nid = \"gitlab\"\nprovider = \"gitlab\"\ntoken_file = \"/t\"\n{extra}"
        ))
        .unwrap()
    }

    #[test]
    fn gitlab_takes_merge_and_squash_with_a_webhook_but_no_rebase_and_no_user_accounts() {
        let caps = || Some(saka_scm::GITLAB_CAPABILITIES);
        for ok in [
            "",
            "merge_method = \"merge\"\n",
            "merge_method = \"squash\"\nwebhook_secret_file = \"/s\"\n",
        ] {
            assert_eq!(capability_refusal(&gitlab(ok), |_| caps()), None, "{ok}");
        }
        assert_eq!(
            capability_refusal(&gitlab("merge_method = \"rebase\"\n"), |_| caps()),
            Some(("config_merge_method_unsupported", "gitlab".to_string()))
        );
        assert_eq!(
            capability_refusal(&gitlab("client_id = \"Iv23liAbCdEfGh123456\"\n"), |_| caps(
            )),
            Some(("config_user_authorship_unsupported", "gitlab".to_string()))
        );
    }

    #[test]
    fn a_config_asking_for_a_missing_capability_is_refused() {
        let full = saka_scm::GITHUB_CAPABILITIES;
        assert_eq!(
            capability_refusal(&config(""), |_| Some(full.clone())),
            None
        );
        assert_eq!(
            capability_refusal(&config("merge_method = \"squash\"\n"), |_| Some(LIMITED)),
            Some(("config_merge_method_unsupported", "github".to_string()))
        );
        assert_eq!(
            capability_refusal(
                &config("merge_method = \"merge\"\nwebhook_secret_file = \"/s\"\n"),
                |_| Some(LIMITED)
            ),
            Some(("config_webhook_unsupported", "github".to_string()))
        );
        assert_eq!(
            capability_refusal(
                &config("merge_method = \"merge\"\nclient_id = \"Iv23liAbCdEfGh123456\"\n"),
                |_| Some(LIMITED)
            ),
            Some(("config_user_authorship_unsupported", "github".to_string()))
        );
        assert_eq!(
            capability_refusal(&config("client_id = \"Iv23liAbCdEfGh123456\"\n"), |_| {
                Some(full.clone())
            }),
            None
        );
        assert_eq!(capability_refusal(&config(""), |_| None), None);
    }
}

#[cfg(test)]
mod mcp_tests {
    use saka_mcp::Tokens;

    use super::*;

    #[test]
    fn a_session_token_is_read_from_its_private_file_and_unknown_sessions_have_none() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let known = SessionId(Uuid::from_u128(1));
        let folder = dir.path().join(known.to_string());
        std::fs::create_dir_all(&folder).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(folder.join("mcp.token"), "abc123\n").unwrap_or_else(|e| panic!("{e}"));
        let tokens = SessionTokens {
            sessions_dir: dir.path().to_path_buf(),
        };
        assert_eq!(
            tokens.token(known).map(|t| t.expose().to_string()),
            Some("abc123".to_string())
        );
        assert!(tokens.token(SessionId(Uuid::from_u128(2))).is_none());
    }
}
