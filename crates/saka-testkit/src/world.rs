use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use saka_core::authorship::GitIdentity;
use saka_core::diagnostics::{
    AgentCli, AgentLogin, AppStatus, Installation, SupervisorProbe, ToolchainCache, WebhookStatus,
};
use saka_core::gate::Check;
use saka_core::ids::{EnvironmentId, SessionId};
use saka_core::model::{RunOwner, ToolchainUse};
use saka_core::policy::RequestPath;
use saka_core::secret::{SecretScope, SecretText, SecretVar};
use saka_core::time::Timestamp;
use saka_core::vocab::SecretPurpose;
use saka_core::vocab::{ChangeState, CheckConclusion, CheckKind, CheckStatus, MergeMethod};
use saka_core::vocab::{HostTool, Tool};
use saka_ports::{
    Agents, Blob, ChangeCredential, ChangeOpening, ChangeSignal, ChangeSnapshot, ChangeSummary,
    ChangedPaths, Clock, CommandOutcome, CommandUnit, DeviceCode, DevicePoll, DocBlob, DocCommit,
    DocEntry, DocExpect, DocFile, DocOpened, DocSource, DocTree, DocWrite, DocsRequest, EnvRef,
    Environments, IdentityLookup, LaunchProfile, LaunchedSession, Link, MergeOutcome, MergeRequest,
    OpenChangeRequest, OpenedChange, PortError, PortResult, PostedReview, PrepareRequest,
    PreparedDocs, PreparedEnvironment, PushOutcome, PushRequest, Reattached, RecipeCommand,
    RecipeCommandOutcome, Release, ReleaseIndex, RepoAccess, RepoInfo, RepoRef, ReviewPost,
    RunnerEvent, RunnerEventKind, RunnerUnit, Scm, ScmCapabilities, Sealed, SecretStore,
    SessionLaunch, SessionRef, StatusReport, Supervisor, TokenRefresh, Toolchains, WebhookDelivery,
    WebhookSignature, WebhookTarget, WorktreeFile, WorktreeState,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::clock::FakeClock;

mod changes;

use changes::Signals;
pub use changes::{BOT_AUTHOR, ChangeSpec, PrMeta};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentOutcome {
    Commit,
    CommitOnFollowup,
    LeaveDirty,
    NoCommit,
    Fail(String),
    Exit,
    Hang,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentScript {
    pub outcome: AgentOutcome,
    pub ask: Option<(String, Value)>,
    pub usage: TurnUsage,
    pub rate_limit: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnUsage {
    pub cost_usd_micros: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

impl Default for TurnUsage {
    fn default() -> Self {
        TurnUsage {
            cost_usd_micros: 1_000,
            input_tokens: 100,
            output_tokens: 50,
        }
    }
}

impl Default for AgentScript {
    fn default() -> Self {
        AgentScript {
            outcome: AgentOutcome::Commit,
            ask: None,
            usage: TurnUsage::default(),
            rate_limit: None,
        }
    }
}

pub const SIM_RUNNER_VERSION: &str = "sim";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftTurn {
    Write { path: String, bytes: Vec<u8> },
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiScript {
    Pass,
    Fail(Vec<String>),
    Silent,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub prepares: u32,
    pub teardowns: u32,
    pub pushes: u32,
    pub prs_opened: u32,
    pub merges: u32,
    pub branches_deleted: u32,
    pub session_starts: u32,
    pub stale_runners_stopped: u32,
    pub sessions_stopped: u32,
    pub interrupts: u32,
    pub answers_to_cli: u32,
    pub answer_frames: u32,
    pub steers_to_cli: u32,
    pub reattaches: u32,
    pub mirror_reads: u32,
    pub change_listings: u32,
    pub change_file_reads: u32,
}

#[derive(Debug, Clone)]
struct Worktree {
    path: String,
    base_sha: String,
    head: String,
    dirty: bool,
    torn_down: bool,
    files: BTreeMap<String, Vec<u8>>,
    links: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct Pr {
    pub repo: String,
    pub number: u64,
    pub head_branch: String,
    pub base_branch: String,
    pub title: String,
    pub body: String,
    pub state: ChangeState,
    pub merge_commit_sha: Option<String>,
    pub merged_head: Option<String>,
    pub opened_with: String,
    pub meta: PrMeta,
}

pub const APP_CREDENTIAL: &str = "app";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScmCall {
    pub op: &'static str,
    pub credential: String,
}

#[derive(Debug, Clone)]
struct FakeSession {
    env: EnvironmentId,
    running: bool,
    connected: bool,
    silent_since: Option<Timestamp>,
    other_version: bool,
    generation: u32,
    log: Vec<RunnerEvent>,
    turns: BTreeSet<u32>,
    open_turn: Option<u32>,
    current_turn: u32,
    asked: bool,
    waiting_turn: Option<u32>,
    answered: BTreeSet<String>,
    steers: BTreeSet<u32>,
    architect: bool,
}

struct State {
    script: AgentScript,
    ci: CiScript,
    commit_ci: CiScript,
    commit_reads_fail: bool,
    doc_opens_fail: bool,
    checkouts: Vec<(EnvironmentId, String)>,
    lose_merges: bool,
    refuse_teardowns: bool,
    next_sha: u64,
    parents: HashMap<String, Vec<String>>,
    home: String,
    remotes: BTreeMap<String, BTreeMap<String, String>>,
    prepared: Vec<Prepared>,
    recipe_reads: Vec<(String, String)>,
    merge_checks: Vec<(String, String, String)>,
    worktrees: BTreeMap<EnvironmentId, Worktree>,
    prs: Vec<Pr>,
    signals: Signals,
    sessions: BTreeMap<SessionId, FakeSession>,
    events: VecDeque<RunnerEvent>,
    stats: Stats,
    next_request: u32,
    docs: BTreeMap<String, Vec<u8>>,
    doc_history: HashMap<String, BTreeMap<String, Vec<u8>>>,
    doc_links: BTreeSet<String>,
    doc_writes: VecDeque<DocWrite>,
    doc_commits: Vec<DocCommit>,
    inaccessible: BTreeSet<String>,
    prompts: Vec<String>,
    resumes: Vec<Option<String>>,
    recipe: Option<String>,
    recipe_runs: Vec<RecipeRun>,
    command_scripts: HashMap<String, VecDeque<CommandScript>>,
    command_output: HashMap<String, String>,
    statuses: Vec<(String, StatusReport)>,
    statuses_refused: bool,
    reviews: Vec<ReviewPost>,
    review_outages: u32,
    reviews_refused: bool,
    draft_turns: VecDeque<DraftTurn>,
    push_outages: u32,
    provider_reads_fail: bool,
    change_reads_fail: bool,
    launches: Vec<Launch>,
    repo_files: BTreeMap<String, String>,
    toolchain_scripts: HashMap<Tool, VecDeque<ToolchainScript>>,
    toolchain_installs: Vec<(Tool, String, bool)>,
    secrets_unreadable: bool,
    webhook_secret: Option<Vec<u8>>,
    capabilities: ScmCapabilities,
    trees: HashMap<String, BTreeMap<String, String>>,
    agent_edits: VecDeque<Vec<FileEdit>>,
    scope_checks: Vec<ScopeCheck>,
    scm_calls: Vec<ScmCall>,
    docs_checkouts: BTreeMap<EnvironmentId, DocsCheckoutState>,
    docs_refusal: Option<PortError>,
    docs_turns: VecDeque<DocsTurn>,
    merge_scripts: HashMap<u64, VecDeque<String>>,
    merge_calls: Vec<(String, u64)>,
    lost_merge_repos: BTreeSet<String>,
}

impl State {
    fn call(&mut self, op: &'static str) {
        self.scm_calls.push(ScmCall {
            op,
            credential: APP_CREDENTIAL.into(),
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub owner: RunOwner,
    pub profile: LaunchProfile,
    pub model: Option<String>,
    pub author: GitIdentity,
    pub vars: BTreeMap<String, String>,
    pub path_prefix: Vec<String>,
    pub secrets: BTreeMap<String, String>,
    pub redacted: Vec<String>,
    pub extra_dirs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsCheckoutState {
    pub repo: String,
    pub commit: String,
    pub project_path: String,
    pub torn_down: bool,
    pub branch: Option<String>,
    pub head: String,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocsTurn {
    Commit(Vec<FileEdit>),
    Dirty,
    Nothing,
}

fn injected(secrets: &[SecretVar], purpose: SecretPurpose) -> BTreeMap<String, String> {
    secrets
        .iter()
        .filter(|s| s.serves(purpose))
        .map(|s| (s.name.clone(), s.value.expose().to_string()))
        .collect()
}

fn redacted(secrets: &[SecretVar]) -> Vec<String> {
    secrets.iter().map(|s| s.name.clone()).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolchainScript {
    Missing,
    DownloadFails,
}

pub type FileEdit = (String, Option<String>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeCheck {
    pub repo: String,
    pub base: String,
    pub head_sha: String,
    pub fetch: bool,
}

pub struct FakeWorld {
    clock: Arc<FakeClock>,
    state: Mutex<State>,
    accounts: crate::accounts::FakeAccounts,
    host: crate::diagnostics::FakeHost,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeRun {
    pub environment_id: EnvironmentId,
    pub phase: &'static str,
    pub index: usize,
    pub command: String,
    pub vars: BTreeMap<String, String>,
    pub path_prefix: Vec<String>,
    pub secrets: BTreeMap<String, String>,
    pub redacted: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandScript {
    Exit(i32),
    Timeout,
    Crash,
}

pub const BASE_BRANCH: &str = "main";
pub const HOME_REPO: &str = "acme/tasks";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub environment_id: EnvironmentId,
    pub repo: String,
    pub base_branch: String,
    pub base_sha: String,
}

impl FakeWorld {
    pub fn new(clock: Arc<FakeClock>, script: AgentScript, ci: CiScript) -> FakeWorld {
        let root = sha(0);
        let mut parents = HashMap::new();
        parents.insert(root.clone(), Vec::new());
        let mut remote = BTreeMap::new();
        remote.insert(BASE_BRANCH.to_string(), root);
        let remotes = BTreeMap::from([(HOME_REPO.to_string(), remote)]);
        FakeWorld {
            accounts: crate::accounts::FakeAccounts::new(Arc::clone(&clock)),
            host: crate::diagnostics::FakeHost::default(),
            clock,
            state: Mutex::new(State {
                script,
                ci,
                commit_ci: CiScript::Silent,
                commit_reads_fail: false,
                doc_opens_fail: false,
                checkouts: Vec::new(),
                lose_merges: false,
                refuse_teardowns: false,
                recipe: None,
                recipe_runs: Vec::new(),
                command_scripts: HashMap::new(),
                command_output: HashMap::new(),
                statuses: Vec::new(),
                statuses_refused: false,
                reviews: Vec::new(),
                review_outages: 0,
                reviews_refused: false,
                draft_turns: VecDeque::new(),
                push_outages: 0,
                provider_reads_fail: false,
                change_reads_fail: false,
                launches: Vec::new(),
                repo_files: BTreeMap::new(),
                toolchain_scripts: HashMap::new(),
                toolchain_installs: Vec::new(),
                secrets_unreadable: false,
                webhook_secret: None,
                capabilities: FAKE_CAPABILITIES,
                trees: HashMap::new(),
                agent_edits: VecDeque::new(),
                scope_checks: Vec::new(),
                scm_calls: Vec::new(),
                docs_checkouts: BTreeMap::new(),
                docs_refusal: None,
                docs_turns: VecDeque::new(),
                merge_scripts: HashMap::new(),
                merge_calls: Vec::new(),
                lost_merge_repos: BTreeSet::new(),
                next_sha: 1,
                parents,
                home: HOME_REPO.to_string(),
                remotes,
                prepared: Vec::new(),
                recipe_reads: Vec::new(),
                merge_checks: Vec::new(),
                worktrees: BTreeMap::new(),
                prs: Vec::new(),
                signals: Signals::default(),
                sessions: BTreeMap::new(),
                events: VecDeque::new(),
                stats: Stats::default(),
                next_request: 0,
                docs: BTreeMap::new(),
                doc_links: BTreeSet::new(),
                doc_writes: VecDeque::new(),
                doc_commits: Vec::new(),
                doc_history: HashMap::new(),
                inaccessible: BTreeSet::new(),
                prompts: Vec::new(),
                resumes: Vec::new(),
            }),
        }
    }

    pub fn accounts(&self) -> &crate::accounts::FakeAccounts {
        &self.accounts
    }

    pub fn host(&self) -> &crate::diagnostics::FakeHost {
        &self.host
    }

    pub fn scm_calls(&self) -> Vec<ScmCall> {
        self.state().scm_calls.clone()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub fn stats(&self) -> Stats {
        self.state().stats.clone()
    }

    pub fn script_draft(&self, turns: Vec<DraftTurn>) {
        self.state().draft_turns = turns.into();
    }

    // Each committing agent turn applies the next edit list; the last list repeats.
    pub fn script_edits(&self, turns: Vec<Vec<FileEdit>>) {
        self.state().agent_edits = turns.into();
    }

    pub fn push_outside(&self, repo: &str, branch: &str, edits: &[FileEdit]) -> String {
        let mut s = self.state();
        let parent = s.refs(repo)[branch].clone();
        let commit = s.commit_edits(&parent, edits);
        s.refs(repo).insert(branch.to_string(), commit.clone());
        commit
    }

    pub fn docs_checkouts(&self) -> BTreeMap<EnvironmentId, DocsCheckoutState> {
        self.state().docs_checkouts.clone()
    }

    pub fn script_docs(&self, turns: Vec<DocsTurn>) {
        self.state().docs_turns = turns.into();
    }

    pub fn script_merges(&self, number: u64, reasons: Vec<&str>) {
        self.state()
            .merge_scripts
            .insert(number, reasons.into_iter().map(String::from).collect());
    }

    pub fn lose_merges_in(&self, repo: &str) {
        self.state().lost_merge_repos.insert(repo.to_string());
    }

    pub fn merge_calls(&self) -> Vec<(String, u64)> {
        self.state().merge_calls.clone()
    }

    pub fn refuse_docs_checkouts(&self, refusal: Option<PortError>) {
        self.state().docs_refusal = refusal;
    }

    pub fn scope_checks(&self) -> Vec<ScopeCheck> {
        self.state().scope_checks.clone()
    }

    pub fn worktree_file(&self, env: EnvironmentId, path: &str) -> Option<Vec<u8>> {
        self.state()
            .worktrees
            .get(&env)
            .and_then(|w| w.files.get(path).cloned())
    }

    pub fn link_in_worktree(&self, env: EnvironmentId, path: &str) {
        if let Some(w) = self.state().worktrees.get_mut(&env) {
            w.links.insert(path.to_string());
        }
    }

    pub fn worktree_live(&self, env: EnvironmentId) -> bool {
        self.state()
            .worktrees
            .get(&env)
            .is_some_and(|w| !w.torn_down)
    }

    pub fn set_script(&self, script: AgentScript) {
        self.state().script = script;
    }

    pub fn set_ci(&self, ci: CiScript) {
        self.state().ci = ci;
    }

    pub fn set_lose_merges(&self, lose: bool) {
        self.state().lose_merges = lose;
    }

    pub fn pop_event(&self) -> Option<RunnerEvent> {
        self.state().events.pop_front()
    }

    pub fn has_events(&self) -> bool {
        !self.state().events.is_empty()
    }

    pub fn prompts(&self) -> Vec<String> {
        self.state().prompts.clone()
    }

    pub fn prs(&self) -> Vec<Pr> {
        self.state().prs.clone()
    }

    pub fn remote_branches(&self) -> BTreeMap<String, String> {
        let mut s = self.state();
        let home = s.home.clone();
        s.refs(&home).clone()
    }

    pub fn branches_of(&self, repo: &str) -> BTreeMap<String, String> {
        self.state().refs(repo).clone()
    }

    pub fn base_head(&self) -> String {
        let mut s = self.state();
        let home = s.home.clone();
        s.refs(&home)[BASE_BRANCH].clone()
    }

    pub fn set_home(&self, repo: &str) {
        let mut s = self.state();
        let home = s.home.clone();
        let refs = s.refs(&home).clone();
        s.home = repo.to_string();
        s.remotes.insert(repo.to_string(), refs);
    }

    pub fn create_branch(&self, repo: &str, branch: &str) -> String {
        let mut s = self.state();
        let base = s.refs(repo)[BASE_BRANCH].clone();
        let commit = s.new_commit(vec![base]);
        s.refs(repo).insert(branch.to_string(), commit.clone());
        commit
    }

    pub fn prepared(&self) -> Vec<Prepared> {
        self.state().prepared.clone()
    }

    pub fn recipe_reads(&self) -> Vec<(String, String)> {
        self.state().recipe_reads.clone()
    }

    pub fn merge_checks(&self) -> Vec<(String, String, String)> {
        self.state().merge_checks.clone()
    }

    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        self.state().is_ancestor(ancestor, descendant)
    }

    pub fn resumes(&self) -> Vec<Option<String>> {
        self.state().resumes.clone()
    }

    pub fn session_log(&self, id: SessionId) -> Vec<RunnerEvent> {
        self.state()
            .sessions
            .get(&id)
            .map(|s| s.log.clone())
            .unwrap_or_default()
    }

    pub fn disconnect_all(&self) {
        let mut s = self.state();
        for fs in s.sessions.values_mut().filter(|f| f.connected) {
            fs.connected = false;
            fs.generation += 1;
        }
        s.events.clear();
    }

    pub fn kill_runner(&self, id: SessionId) {
        let mut s = self.state();
        if let Some(fs) = s.sessions.get_mut(&id) {
            fs.running = false;
            fs.connected = false;
        }
        s.events.retain(|e| e.session_id != id);
    }

    pub fn silence_runner(&self, id: SessionId) {
        let now = self.clock.now();
        if let Some(fs) = self.state().sessions.get_mut(&id) {
            fs.silent_since = Some(now);
        }
    }

    pub fn swap_runner_version(&self, id: SessionId) {
        if let Some(fs) = self.state().sessions.get_mut(&id) {
            fs.other_version = true;
        }
    }

    pub fn running_sessions(&self) -> Vec<SessionId> {
        self.state()
            .sessions
            .iter()
            .filter(|(_, s)| s.running)
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn put_doc(&self, path: &str, bytes: &[u8]) -> String {
        let mut s = self.state();
        let base = s.home_refs()[BASE_BRANCH].clone();
        let commit = s.new_commit(vec![base]);
        s.home_refs()
            .insert(BASE_BRANCH.to_string(), commit.clone());
        s.docs.insert(path.to_string(), bytes.to_vec());
        let snapshot = s.docs.clone();
        s.doc_history.insert(commit.clone(), snapshot);
        commit
    }

    pub fn remove_doc(&self, path: &str) -> String {
        let mut s = self.state();
        let base = s.home_refs()[BASE_BRANCH].clone();
        let commit = s.new_commit(vec![base]);
        s.home_refs()
            .insert(BASE_BRANCH.to_string(), commit.clone());
        s.docs.remove(path);
        let snapshot = s.docs.clone();
        s.doc_history.insert(commit.clone(), snapshot);
        commit
    }

    pub fn put_doc_link(&self, path: &str) {
        self.state().doc_links.insert(path.to_string());
    }

    pub fn doc(&self, path: &str) -> Option<Vec<u8>> {
        self.state().docs.get(path).cloned()
    }

    pub fn script_doc_write(&self, outcome: DocWrite) {
        self.state().doc_writes.push_back(outcome);
    }

    pub fn doc_commits(&self) -> Vec<DocCommit> {
        self.state().doc_commits.clone()
    }

    pub fn set_capabilities(&self, capabilities: ScmCapabilities) {
        self.state().capabilities = capabilities;
    }

    pub fn set_webhook_secret(&self, secret: &[u8]) {
        self.state().webhook_secret = Some(secret.to_vec());
    }

    pub fn make_inaccessible(&self, full_name: &str) {
        self.state().inaccessible.insert(full_name.to_string());
    }

    pub fn merge_outside(&self, number: u64) {
        let mut s = self.state();
        s.merge(number, None);
    }

    pub fn refuse_teardowns(&self, refuse: bool) {
        self.state().refuse_teardowns = refuse;
    }

    pub fn set_recipe(&self, recipe: Option<&str>) {
        self.state().recipe = recipe.map(String::from);
    }

    pub fn dirty_worktrees(&self) {
        for w in self.state().worktrees.values_mut() {
            w.dirty = true;
        }
    }

    pub fn statuses(&self) -> Vec<(String, StatusReport)> {
        self.state().statuses.clone()
    }

    pub fn reviews(&self) -> Vec<ReviewPost> {
        self.state().reviews.clone()
    }

    pub fn fail_reviews(&self, times: u32) {
        self.state().review_outages = times;
    }

    pub fn refuse_reviews(&self) {
        self.state().reviews_refused = true;
    }

    pub fn refuse_statuses(&self) {
        self.state().statuses_refused = true;
    }

    pub fn fail_provider_reads(&self, fail: bool) {
        self.state().provider_reads_fail = fail;
    }

    pub fn fail_pushes(&self, times: u32) {
        self.state().push_outages = times;
    }

    pub fn set_commit_ci(&self, ci: CiScript) {
        self.state().commit_ci = ci;
    }

    pub fn fail_commit_reads(&self, fail: bool) {
        self.state().commit_reads_fail = fail;
    }

    pub fn fail_doc_opens(&self, fail: bool) {
        self.state().doc_opens_fail = fail;
    }

    pub fn checkouts(&self) -> Vec<(EnvironmentId, String)> {
        self.state().checkouts.clone()
    }

    pub fn push_to_base(&self) -> String {
        let mut s = self.state();
        let base = s.home_refs().get(BASE_BRANCH).cloned().unwrap_or_default();
        let commit = s.new_commit(vec![base]);
        s.home_refs()
            .insert(BASE_BRANCH.to_string(), commit.clone());
        commit
    }

    pub fn fail_change_reads(&self, fail: bool) {
        self.state().change_reads_fail = fail;
    }

    pub fn command_output(&self, command: &str, output: &str) {
        self.state()
            .command_output
            .insert(command.to_string(), output.to_string());
    }

    pub fn script_command(&self, command: &str, script: CommandScript) {
        self.state()
            .command_scripts
            .entry(command.to_string())
            .or_default()
            .push_back(script);
    }

    pub fn recipe_runs(&self) -> Vec<RecipeRun> {
        self.state().recipe_runs.clone()
    }

    pub fn launch_vars(&self) -> Vec<BTreeMap<String, String>> {
        self.state()
            .launches
            .iter()
            .map(|l| l.vars.clone())
            .collect()
    }

    pub fn launches(&self) -> Vec<Launch> {
        self.state().launches.clone()
    }

    pub fn set_repo_file(&self, path: &str, text: Option<&str>) {
        let mut s = self.state();
        match text {
            Some(t) => s.repo_files.insert(path.to_string(), t.to_string()),
            None => s.repo_files.remove(path),
        };
    }

    pub fn break_secret_key(&self, broken: bool) {
        self.state().secrets_unreadable = broken;
    }

    pub fn script_toolchain(&self, tool: Tool, script: ToolchainScript) {
        self.state()
            .toolchain_scripts
            .entry(tool)
            .or_default()
            .push_back(script);
    }

    pub fn toolchain_installs(&self) -> Vec<(Tool, String, bool)> {
        self.state().toolchain_installs.clone()
    }

    pub fn close_outside(&self, number: u64) {
        let mut s = self.state();
        if let Some(pr) = s.prs.iter_mut().find(|p| p.number == number) {
            pr.state = ChangeState::Closed;
        }
    }
}

fn script_checks(ci: &CiScript) -> Vec<Check> {
    let check = |name: &str, conclusion| Check {
        name: name.to_string(),
        kind: CheckKind::CheckRun,
        status: CheckStatus::Completed,
        conclusion: Some(conclusion),
        url: None,
    };
    match ci {
        CiScript::Pass => vec![check("test", CheckConclusion::Success)],
        CiScript::Fail(names) => names
            .iter()
            .map(|n| check(n, CheckConclusion::Failure))
            .collect(),
        CiScript::Silent => Vec::new(),
    }
}

fn sha(n: u64) -> String {
    format!("{n:040x}")
}

pub fn git_blob(bytes: &[u8]) -> String {
    use sha1::{Digest as _, Sha1};
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn inside_folder(project_path: &str, path: &str) -> bool {
    match project_path.trim_matches('/') {
        "" | "." => true,
        dir => path.starts_with(&format!("{dir}/")),
    }
}

fn markdown_name(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

impl State {
    fn refs(&mut self, repo: &str) -> &mut BTreeMap<String, String> {
        self.remotes
            .entry(repo.to_string())
            .or_insert_with(|| BTreeMap::from([(BASE_BRANCH.to_string(), sha(0))]))
    }

    fn home_refs(&mut self) -> &mut BTreeMap<String, String> {
        let home = self.home.clone();
        self.refs(&home)
    }

    fn is_home(&self, repo: &RepoRef) -> bool {
        repo.full_name == self.home
    }

    fn new_commit(&mut self, parents: Vec<String>) -> String {
        let id = sha(self.next_sha);
        self.next_sha += 1;
        self.parents.insert(id.clone(), parents);
        id
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        let mut stack = vec![descendant.to_string()];
        let mut seen = BTreeSet::new();
        while let Some(c) = stack.pop() {
            if c == ancestor {
                return true;
            }
            if seen.insert(c.clone()) {
                stack.extend(self.parents.get(&c).cloned().unwrap_or_default());
            }
        }
        false
    }

    fn tree(&self, commit: &str) -> BTreeMap<String, String> {
        self.trees.get(commit).cloned().unwrap_or_default()
    }

    fn commit_edits(&mut self, parent: &str, edits: &[FileEdit]) -> String {
        let mut tree = self.tree(parent);
        for (path, content) in edits {
            match content {
                Some(text) => tree.insert(path.clone(), text.clone()),
                None => tree.remove(path),
            };
        }
        let commit = self.new_commit(vec![parent.to_string()]);
        self.trees.insert(commit.clone(), tree);
        commit
    }

    fn next_edits(&mut self) -> Option<Vec<FileEdit>> {
        if self.agent_edits.len() > 1 {
            self.agent_edits.pop_front()
        } else {
            self.agent_edits.front().cloned()
        }
    }

    fn merge_base(&self, a: &str, b: &str) -> Option<String> {
        let mut ancestors = BTreeSet::new();
        let mut stack = vec![a.to_string()];
        while let Some(c) = stack.pop() {
            if ancestors.insert(c.clone()) {
                stack.extend(self.parents.get(&c).cloned().unwrap_or_default());
            }
        }
        let mut queue = VecDeque::from([b.to_string()]);
        let mut seen = BTreeSet::new();
        while let Some(c) = queue.pop_front() {
            if ancestors.contains(&c) {
                return Some(c);
            }
            if seen.insert(c.clone()) {
                queue.extend(self.parents.get(&c).cloned().unwrap_or_default());
            }
        }
        None
    }

    fn diff(&self, from: &str, to: &str) -> Vec<String> {
        let (a, b) = (self.tree(from), self.tree(to));
        a.keys()
            .chain(b.keys())
            .filter(|p| a.get(*p) != b.get(*p))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn commits_ahead(&self, base: &str, head: &str) -> u32 {
        let mut n = 0;
        let mut at = head.to_string();
        while at != base && !self.is_ancestor(&at, base) {
            n += 1;
            match self.parents.get(&at).and_then(|p| p.first()) {
                Some(p) => at = p.clone(),
                None => break,
            }
        }
        n
    }

    fn checks(&self) -> Vec<Check> {
        script_checks(&self.ci)
    }

    fn merge(&mut self, number: u64, expected_head: Option<&str>) -> MergeOutcome {
        let Some(i) = self.prs.iter().position(|p| p.number == number) else {
            return MergeOutcome::Rejected {
                reason: "rejected".into(),
            };
        };
        let pr = self.prs[i].clone();
        let head = self
            .refs(&pr.repo)
            .get(&pr.head_branch)
            .cloned()
            .unwrap_or_default();
        if pr.state != ChangeState::Open {
            return MergeOutcome::Rejected {
                reason: "not_mergeable".into(),
            };
        }
        if expected_head.is_some_and(|h| h != head) {
            return MergeOutcome::Rejected {
                reason: "head_changed".into(),
            };
        }
        let base = self.refs(&pr.repo)[&pr.base_branch].clone();
        let mut tree = self.tree(&base);
        let fork = self.merge_base(&base, &head).unwrap_or_default();
        let theirs = self.tree(&head);
        for path in self.diff(&fork, &head) {
            match theirs.get(&path) {
                Some(text) => tree.insert(path, text.clone()),
                None => tree.remove(&path),
            };
        }
        let merge = self.new_commit(vec![base, head.clone()]);
        self.trees.insert(merge.clone(), tree);
        if !self.lose_merges && !self.lost_merge_repos.contains(&pr.repo) {
            self.refs(&pr.repo)
                .insert(pr.base_branch.clone(), merge.clone());
        }
        let pr = &mut self.prs[i];
        pr.state = ChangeState::Merged;
        pr.merge_commit_sha = Some(merge.clone());
        pr.merged_head = Some(head);
        self.stats.merges += 1;
        MergeOutcome::Merged {
            merge_commit_sha: merge,
            merged_at: None,
        }
    }

    fn emit(&mut self, session: SessionId, at: Timestamp, kind: RunnerEventKind) {
        let Some(s) = self.sessions.get_mut(&session) else {
            return;
        };
        if matches!(kind, RunnerEventKind::TurnEnded { .. }) {
            s.open_turn = None;
        }
        let event = RunnerEvent {
            session_id: session,
            offset: s.log.len() as u64,
            at,
            kind,
        };
        s.log.push(event.clone());
        if s.connected && s.silent_since.is_none() {
            self.events.push_back(event);
        }
    }

    fn work(&mut self, session: SessionId, turn: u32, at: Timestamp) {
        let Some((env, architect)) = self.sessions.get(&session).map(|s| (s.env, s.architect))
        else {
            return;
        };
        let outcome = self.script.outcome.clone();
        let drafting = architect
            && matches!(
                outcome,
                AgentOutcome::Commit
                    | AgentOutcome::CommitOnFollowup
                    | AgentOutcome::LeaveDirty
                    | AgentOutcome::NoCommit
            );
        if drafting {
            // The last scripted turn repeats, so a script of one turn covers every turn.
            let next = if self.draft_turns.len() > 1 {
                self.draft_turns.pop_front()
            } else {
                self.draft_turns.front().cloned()
            };
            if let Some(DraftTurn::Write { path, bytes }) = next
                && let Some(w) = self.worktrees.get_mut(&env)
            {
                w.files.insert(path, bytes);
            }
        }
        if !architect
            && let Some(turn) = self.docs_turns.pop_front()
            && let Some(d) = self.docs_checkouts.get(&env).cloned()
        {
            match turn {
                DocsTurn::Commit(edits) => {
                    let c = self.commit_edits(&d.head, &edits);
                    if let Some(d) = self.docs_checkouts.get_mut(&env) {
                        d.head = c;
                        d.dirty = false;
                    }
                }
                DocsTurn::Dirty => {
                    if let Some(d) = self.docs_checkouts.get_mut(&env) {
                        d.dirty = true;
                    }
                }
                DocsTurn::Nothing => {}
            }
        }
        let usage = self.script.usage;
        if let Some(info) = self.script.rate_limit.clone() {
            self.emit(session, at, RunnerEventKind::RateLimit { info });
        }
        let end = |s: &mut State, status: &str| {
            s.emit(
                session,
                at,
                RunnerEventKind::TurnEnded {
                    turn,
                    status: status.to_string(),
                    cost_usd_micros: Some(usage.cost_usd_micros),
                    input_tokens: Some(usage.input_tokens),
                    output_tokens: Some(usage.output_tokens),
                    result_summary: Some(format!("turn {turn} done")),
                },
            );
        };
        let commit = |s: &mut State| {
            let Some(w) = s.worktrees.get(&env).cloned() else {
                return;
            };
            let edits = s.next_edits();
            if edits.is_some() || s.commits_ahead(&w.base_sha, &w.head) == 0 {
                let c = s.commit_edits(&w.head, &edits.unwrap_or_default());
                if let Some(w) = s.worktrees.get_mut(&env) {
                    w.head = c;
                }
            }
            if let Some(w) = s.worktrees.get_mut(&env) {
                w.dirty = false;
            }
        };
        let make_dirty = |s: &mut State| {
            if let Some(w) = s.worktrees.get_mut(&env) {
                w.dirty = true;
            }
        };
        match outcome {
            _ if drafting => end(self, "success"),
            AgentOutcome::Commit => {
                commit(self);
                end(self, "success");
            }
            AgentOutcome::CommitOnFollowup => {
                if turn == 1 {
                    make_dirty(self);
                } else {
                    commit(self);
                }
                end(self, "success");
            }
            AgentOutcome::LeaveDirty => {
                make_dirty(self);
                end(self, "success");
            }
            AgentOutcome::NoCommit => end(self, "success"),
            AgentOutcome::Fail(subtype) => end(self, &subtype),
            AgentOutcome::Exit => {
                self.emit(
                    session,
                    at,
                    RunnerEventKind::CliExited {
                        code: Some(1),
                        signal: None,
                    },
                );
            }
            AgentOutcome::Hang => {}
        }
    }

    fn session_mut(&mut self, session: &SessionRef) -> PortResult<&mut FakeSession> {
        match self.sessions.get_mut(&session.session_id) {
            Some(s) if s.running && s.connected && s.silent_since.is_none() => Ok(s),
            _ => Err(PortError::retryable(
                "runner_unreachable",
                json!({ "session_id": session.session_id }),
            )),
        }
    }
}

#[async_trait]
impl Environments for FakeWorld {
    async fn prepare(&self, request: &PrepareRequest) -> PortResult<PreparedEnvironment> {
        let mut s = self.state();
        let root = format!("/sim/envs/{}", request.environment_id);
        let path = format!("{root}/worktree");
        if let Some(w) = s
            .worktrees
            .get(&request.environment_id)
            .filter(|w| !w.torn_down)
        {
            return Ok(PreparedEnvironment {
                root_path: root,
                worktree_path: w.path.clone(),
                base_sha: w.base_sha.clone(),
            });
        }
        let base = s
            .refs(&request.repo.full_name)
            .get(&request.base_branch)
            .cloned()
            .ok_or_else(|| PortError::permanent("base_branch_missing", json!({})))?;
        s.prepared.push(Prepared {
            environment_id: request.environment_id,
            repo: request.repo.full_name.clone(),
            base_branch: request.base_branch.clone(),
            base_sha: base.clone(),
        });
        s.worktrees.insert(
            request.environment_id,
            Worktree {
                path: path.clone(),
                base_sha: base.clone(),
                head: base.clone(),
                dirty: false,
                torn_down: false,
                files: BTreeMap::new(),
                links: BTreeSet::new(),
            },
        );
        s.stats.prepares += 1;
        Ok(PreparedEnvironment {
            root_path: root,
            worktree_path: path,
            base_sha: base,
        })
    }

    async fn prepare_docs(&self, request: &DocsRequest) -> PortResult<PreparedDocs> {
        let mut s = self.state();
        let root = format!("/sim/envs/{}/docs", request.environment_id);
        let path = if request.project_path == "." {
            root.clone()
        } else {
            format!("{root}/{}", request.project_path)
        };
        let prepared = PreparedDocs {
            worktree_path: root,
            path,
            base_sha: request.commit.clone(),
        };
        if s.docs_checkouts
            .get(&request.environment_id)
            .is_some_and(|d| !d.torn_down && d.commit == request.commit)
        {
            return Ok(prepared);
        }
        if let Some(refusal) = s.docs_refusal.clone() {
            return Err(refusal);
        }
        if !s.parents.contains_key(&request.commit) {
            return Err(PortError::permanent(
                "docs_commit_missing",
                json!({ "commit": request.commit }),
            ));
        }
        s.docs_checkouts.insert(
            request.environment_id,
            DocsCheckoutState {
                repo: request.repo.full_name.clone(),
                commit: request.commit.clone(),
                project_path: request.project_path.clone(),
                torn_down: false,
                branch: request.branch.clone(),
                head: request.commit.clone(),
                dirty: false,
            },
        );
        Ok(prepared)
    }

    async fn teardown_docs(
        &self,
        environment_id: EnvironmentId,
        _repo: &RepoRef,
    ) -> PortResult<()> {
        let mut s = self.state();
        if s.refuse_teardowns {
            return Err(PortError::retryable(
                "env_io_failed",
                json!({ "op": "remove_docs", "error": "Permission denied (os error 13)" }),
            ));
        }
        if let Some(d) = s.docs_checkouts.get_mut(&environment_id) {
            d.torn_down = true;
        }
        Ok(())
    }

    async fn inspect_docs(
        &self,
        environment_id: EnvironmentId,
        base_sha: &str,
    ) -> PortResult<WorktreeState> {
        let s = self.state();
        let d = s
            .docs_checkouts
            .get(&environment_id)
            .filter(|d| !d.torn_down)
            .ok_or_else(|| PortError::permanent("worktree_missing", json!({})))?;
        Ok(WorktreeState {
            dirty: d.dirty,
            commits_ahead: s.commits_ahead(base_sha, &d.head),
            head_sha: d.head.clone(),
        })
    }

    async fn inspect(&self, env: &EnvRef, base_sha: &str) -> PortResult<WorktreeState> {
        let s = self.state();
        let w = s
            .worktrees
            .get(&env.environment_id)
            .filter(|w| !w.torn_down)
            .ok_or_else(|| PortError::permanent("worktree_missing", json!({})))?;
        Ok(WorktreeState {
            dirty: w.dirty,
            commits_ahead: s.commits_ahead(base_sha, &w.head),
            head_sha: w.head.clone(),
        })
    }

    async fn contains_commit(
        &self,
        repo: &RepoRef,
        commit: &str,
        base_branch: &str,
    ) -> PortResult<Option<String>> {
        let mut s = self.state();
        s.merge_checks.push((
            repo.full_name.clone(),
            commit.to_string(),
            base_branch.to_string(),
        ));
        let base = s
            .refs(&repo.full_name)
            .get(base_branch)
            .cloned()
            .unwrap_or_default();
        Ok(s.is_ancestor(commit, &base).then_some(base))
    }

    async fn changed_paths(
        &self,
        repo: &RepoRef,
        base: &str,
        head_sha: &str,
        fetch: bool,
    ) -> PortResult<ChangedPaths> {
        let mut s = self.state();
        s.scope_checks.push(ScopeCheck {
            repo: repo.full_name.clone(),
            base: base.to_string(),
            head_sha: head_sha.to_string(),
            fetch,
        });
        if !s.parents.contains_key(head_sha) {
            return Err(PortError::permanent(
                "commit_missing",
                json!({ "commit": head_sha }),
            ));
        }
        let base = match base.strip_prefix("refs/remotes/origin/") {
            Some(branch) => s.refs(&repo.full_name).get(branch).cloned(),
            None => Some(base.to_string()),
        }
        .ok_or_else(|| PortError::retryable("git_failed", json!({ "op": "merge_base" })))?;
        let fork = s
            .merge_base(&base, head_sha)
            .ok_or_else(|| PortError::retryable("git_failed", json!({ "op": "merge_base" })))?;
        Ok(ChangedPaths {
            paths: s.diff(&fork, head_sha),
            truncated: false,
        })
    }

    async fn check_out(&self, env: &EnvRef, commit: &str) -> PortResult<()> {
        let mut s = self.state();
        if !s.parents.contains_key(commit) {
            return Err(PortError::permanent(
                "commit_missing",
                json!({ "commit": commit }),
            ));
        }
        let w = s
            .worktrees
            .get_mut(&env.environment_id)
            .filter(|w| !w.torn_down)
            .ok_or_else(|| PortError::permanent("worktree_missing", json!({})))?;
        w.head = commit.to_string();
        w.dirty = false;
        s.checkouts.push((env.environment_id, commit.to_string()));
        Ok(())
    }

    async fn teardown(&self, env: &EnvRef) -> PortResult<()> {
        let mut s = self.state();
        if s.refuse_teardowns {
            return Err(PortError::retryable(
                "env_io_failed",
                json!({ "op": "remove_env_root", "error": "Permission denied (os error 13)" }),
            ));
        }
        if let Some(w) = s.worktrees.get_mut(&env.environment_id)
            && !w.torn_down
        {
            w.torn_down = true;
            s.stats.teardowns += 1;
        }
        Ok(())
    }

    async fn read_recipe(&self, repo: &RepoRef, commit: &str) -> PortResult<Option<String>> {
        let mut s = self.state();
        s.recipe_reads
            .push((repo.full_name.clone(), commit.to_string()));
        Ok(s.recipe.clone())
    }

    async fn branch_tip(&self, repo: &RepoRef, branch: &str) -> PortResult<Option<String>> {
        Ok(self.state().refs(&repo.full_name).get(branch).cloned())
    }

    async fn list_repo_dir(
        &self,
        _repo: &RepoRef,
        _commit: &str,
        dir: &str,
    ) -> PortResult<Vec<String>> {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        Ok(self
            .state()
            .repo_files
            .keys()
            .filter(|p| {
                p.strip_prefix(&prefix)
                    .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'))
            })
            .cloned()
            .collect())
    }

    async fn read_repo_file(
        &self,
        _repo: &RepoRef,
        _commit: &str,
        path: &str,
    ) -> PortResult<Option<String>> {
        Ok(self.state().repo_files.get(path).cloned())
    }

    async fn run_recipe_command(
        &self,
        env: &EnvRef,
        command: &RecipeCommand,
    ) -> PortResult<RecipeCommandOutcome> {
        let mut s = self.state();
        let live = s
            .worktrees
            .get(&env.environment_id)
            .is_some_and(|w| !w.torn_down);
        if !live {
            return Err(PortError::permanent("worktree_missing", json!({})));
        }
        let script = s
            .command_scripts
            .get_mut(&command.command)
            .and_then(VecDeque::pop_front)
            .unwrap_or(CommandScript::Exit(0));
        if script == CommandScript::Crash {
            return Err(PortError::retryable(
                "supervisor_run_failed",
                json!({ "detail": "simulated crash" }),
            ));
        }
        s.recipe_runs.push(RecipeRun {
            environment_id: env.environment_id,
            phase: command.phase.as_str(),
            index: command.index,
            command: command.command.clone(),
            vars: env.vars.clone(),
            path_prefix: env.path_prefix.clone(),
            secrets: injected(&env.secrets, SecretPurpose::Setup),
            redacted: redacted(&env.secrets),
        });
        Ok(RecipeCommandOutcome {
            exit_code: match script {
                CommandScript::Exit(code) => Some(code),
                _ => None,
            },
            timed_out: script == CommandScript::Timeout,
            duration_ms: 5,
            log_path: format!(
                "/sim/logs/environments/{}/{}.log",
                env.environment_id,
                command.label()
            ),
            output_tail: s
                .command_output
                .get(&command.command)
                .cloned()
                .unwrap_or_default(),
        })
    }

    async fn read_worktree_file(
        &self,
        env: &EnvRef,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<WorktreeFile> {
        let s = self.state();
        let w = s
            .worktrees
            .get(&env.environment_id)
            .filter(|w| !w.torn_down)
            .ok_or_else(|| PortError::permanent("worktree_missing", json!({})))?;
        let path = worktree_relative(path)?;
        if w.links
            .iter()
            .any(|l| path == *l || path.starts_with(&format!("{l}/")))
        {
            return Ok(WorktreeFile::NotRegular);
        }
        Ok(match w.files.get(&path) {
            None if w.files.keys().any(|f| f.starts_with(&format!("{path}/"))) => {
                WorktreeFile::NotRegular
            }
            None => WorktreeFile::Missing,
            Some(b) if b.len() as u64 > max_bytes => WorktreeFile::TooLarge {
                bytes: b.len() as u64,
            },
            Some(b) => WorktreeFile::Bytes(b.clone()),
        })
    }

    async fn write_worktree_file(&self, env: &EnvRef, path: &str, bytes: &[u8]) -> PortResult<()> {
        let mut s = self.state();
        let w = s
            .worktrees
            .get_mut(&env.environment_id)
            .filter(|w| !w.torn_down)
            .ok_or_else(|| PortError::permanent("worktree_missing", json!({})))?;
        let path = worktree_relative(path)?;
        let blocked = w
            .links
            .iter()
            .any(|l| path == *l || path.starts_with(&format!("{l}/")))
            || w.files.keys().any(|f| path.starts_with(&format!("{f}/")))
            || w.files.keys().any(|f| f.starts_with(&format!("{path}/")));
        if blocked {
            return Err(PortError::permanent(
                "worktree_path_conflict",
                json!({ "path": path }),
            ));
        }
        w.files.insert(path, bytes.to_vec());
        Ok(())
    }

    fn resolve_path(&self, env: &EnvRef, cwd: &str, raw: &str) -> RequestPath {
        let root = env.worktree_path.trim_end_matches('/');
        let relative = if let Some(rest) = raw.strip_prefix(&format!("{root}/")) {
            rest.to_string()
        } else if raw.starts_with('/') {
            return RequestPath {
                path: raw.to_string(),
                inside_worktree: false,
                through_symlink: false,
            };
        } else if cwd.is_empty() {
            raw.to_string()
        } else {
            format!("{cwd}/{raw}")
        };
        let mut parts: Vec<&str> = Vec::new();
        for part in relative.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return RequestPath {
                            path: raw.to_string(),
                            inside_worktree: false,
                            through_symlink: false,
                        };
                    }
                }
                p => parts.push(p),
            }
        }
        let path = parts.join("/");
        let through_symlink = self
            .state()
            .worktrees
            .get(&env.environment_id)
            .is_some_and(|w| {
                w.links
                    .iter()
                    .any(|l| path == *l || path.starts_with(&format!("{l}/")))
            });
        RequestPath {
            path,
            inside_worktree: true,
            through_symlink,
        }
    }
}

fn worktree_relative(path: &str) -> PortResult<String> {
    let valid = !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..");
    if valid {
        Ok(path.to_string())
    } else {
        Err(PortError::permanent(
            "worktree_path_invalid",
            json!({ "path": path }),
        ))
    }
}

#[async_trait]
impl DocSource for FakeWorld {
    async fn branch_tip(&self, repo: &RepoRef, branch: &str) -> PortResult<Option<String>> {
        let mut s = self.state();
        s.stats.mirror_reads += 1;
        Ok(s.refs(&repo.full_name).get(branch).cloned())
    }

    async fn read_at_tip(
        &self,
        repo: &RepoRef,
        branch: &str,
        path: &str,
    ) -> PortResult<Option<DocFile>> {
        let mut s = self.state();
        s.stats.mirror_reads += 1;
        let commit = s
            .refs(&repo.full_name)
            .get(branch)
            .cloned()
            .ok_or_else(|| {
                PortError::permanent("base_branch_missing", json!({ "base_branch": branch }))
            })?;
        if !s.is_home(repo) {
            return Ok(None);
        }
        Ok(s.docs.get(path).map(|bytes| DocFile {
            path: path.to_string(),
            commit,
            bytes: bytes.clone(),
        }))
    }

    async fn read_at(
        &self,
        repo: &RepoRef,
        commit: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<Blob> {
        let mut s = self.state();
        s.stats.mirror_reads += 1;
        if !s.is_home(repo) {
            return Ok(Blob::Missing);
        }
        let docs = s.doc_history.get(commit).unwrap_or(&s.docs);
        Ok(match docs.get(path) {
            None => Blob::Missing,
            Some(b) if b.len() as u64 > max_bytes => Blob::TooLarge {
                bytes: b.len() as u64,
            },
            Some(b) => Blob::Bytes(b.clone()),
        })
    }

    async fn tree(
        &self,
        repo: &RepoRef,
        branch: &str,
        project_path: &str,
        max_files: usize,
    ) -> PortResult<DocTree> {
        let mut s = self.state();
        let commit = s
            .refs(&repo.full_name)
            .get(branch)
            .cloned()
            .ok_or_else(|| {
                PortError::permanent("base_branch_missing", json!({ "base_branch": branch }))
            })?;
        let mut files: Vec<DocEntry> = s
            .docs
            .iter()
            .filter(|(path, _)| {
                inside_folder(project_path, path)
                    && markdown_name(path)
                    && !s.doc_links.contains(*path)
            })
            .map(|(path, bytes)| DocEntry {
                path: path.clone(),
                blob: git_blob(bytes),
                bytes: bytes.len() as u64,
            })
            .collect();
        let truncated = files.len() > max_files;
        files.truncate(max_files);
        Ok(DocTree {
            commit,
            files,
            truncated,
        })
    }

    async fn tree_at(
        &self,
        _repo: &RepoRef,
        commit: &str,
        project_path: &str,
    ) -> PortResult<Vec<DocEntry>> {
        let s = self.state();
        let docs = s.doc_history.get(commit).unwrap_or(&s.docs);
        Ok(docs
            .iter()
            .filter(|(path, _)| {
                inside_folder(project_path, path)
                    && markdown_name(path)
                    && !s.doc_links.contains(*path)
            })
            .map(|(path, bytes)| DocEntry {
                path: path.clone(),
                blob: git_blob(bytes),
                bytes: bytes.len() as u64,
            })
            .collect())
    }

    async fn open(
        &self,
        repo: &RepoRef,
        branch: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<DocOpened> {
        let mut s = self.state();
        if s.doc_opens_fail {
            return Err(PortError::retryable(
                "scm_unavailable",
                json!({ "status": 503 }),
            ));
        }
        let commit = s
            .refs(&repo.full_name)
            .get(branch)
            .cloned()
            .ok_or_else(|| {
                PortError::permanent("base_branch_missing", json!({ "base_branch": branch }))
            })?;
        let blob = match s.docs.get(path) {
            None => DocBlob::Missing,
            Some(_) if s.doc_links.contains(path) => DocBlob::NotRegular,
            Some(b) if b.len() as u64 > max_bytes => DocBlob::TooLarge {
                blob: git_blob(b),
                bytes: b.len() as u64,
            },
            Some(b) => DocBlob::Bytes {
                blob: git_blob(b),
                bytes: b.clone(),
            },
        };
        Ok(DocOpened { commit, blob })
    }

    async fn commit_file(&self, request: &DocCommit) -> PortResult<DocWrite> {
        let mut s = self.state();
        s.doc_commits.push(request.clone());
        if let Some(outcome) = s.doc_writes.pop_front() {
            return Ok(outcome);
        }
        let tip = s
            .refs(&request.repo.full_name)
            .get(&request.branch)
            .cloned()
            .ok_or_else(|| {
                PortError::permanent(
                    "base_branch_missing",
                    json!({ "base_branch": request.branch }),
                )
            })?;
        let blob = git_blob(&request.bytes);
        let current = s.docs.get(&request.path).map(|b| git_blob(b));
        if current.as_deref() == Some(blob.as_str()) {
            return Ok(DocWrite::Unchanged { commit: tip, blob });
        }
        let holds = match &request.expect {
            DocExpect::Absent => current.is_none(),
            DocExpect::Exists => current.is_some(),
            DocExpect::Blob(b) => current.as_deref() == Some(b.as_str()),
        };
        if !holds {
            return Ok(DocWrite::Stale {
                commit: tip,
                blob: current,
            });
        }
        let commit = s.new_commit(vec![tip.clone()]);
        s.refs(&request.repo.full_name)
            .insert(request.branch.clone(), commit.clone());
        s.docs.insert(request.path.clone(), request.bytes.clone());
        let snapshot = s.docs.clone();
        s.doc_history.insert(commit.clone(), snapshot);
        Ok(DocWrite::Committed {
            commit,
            parent: tip,
            blob,
            created: current.is_none(),
            attempts: 1,
        })
    }

    async fn find_always(
        &self,
        _repo: &RepoRef,
        commit: &str,
        project_path: &str,
    ) -> PortResult<Vec<String>> {
        let s = self.state();
        let docs = s.doc_history.get(commit).unwrap_or(&s.docs);
        let prefix = match project_path.trim_matches('/') {
            "" | "." => String::new(),
            dir => format!("{dir}/"),
        };
        Ok(docs
            .iter()
            .filter(|(path, _)| {
                path.starts_with(&prefix) && (path.ends_with(".md") || path.ends_with(".markdown"))
            })
            .filter(|(_, bytes)| {
                String::from_utf8_lossy(bytes).lines().any(|line| {
                    line.strip_prefix("context:")
                        .is_some_and(|rest| rest.trim() == "always")
                })
            })
            .map(|(path, _)| path.clone())
            .collect())
    }
}

#[async_trait]
impl Scm for FakeWorld {
    async fn repository(&self, _integration_id: &str, full_name: &str) -> PortResult<RepoInfo> {
        if self.state().provider_reads_fail {
            return Err(PortError::retryable(
                "scm_unavailable",
                json!({ "status": 500, "detail": "server_error" }),
            ));
        }
        if self.state().inaccessible.contains(full_name) {
            return Err(PortError::permanent(
                "scm_repo_inaccessible",
                json!({ "repo": full_name, "status": 404 }),
            ));
        }
        Ok(RepoInfo {
            full_name: full_name.to_string(),
            url: format!("https://github.com/{full_name}"),
            default_branch: BASE_BRANCH.to_string(),
        })
    }

    async fn branch_head(
        &self,
        _integration_id: &str,
        full_name: &str,
        branch: &str,
    ) -> PortResult<Option<String>> {
        let mut s = self.state();
        s.call("branch_head");
        if s.provider_reads_fail {
            return Err(PortError::retryable(
                "scm_unavailable",
                json!({ "status": 500, "detail": "server_error" }),
            ));
        }
        Ok(s.refs(full_name).get(branch).cloned())
    }

    async fn file_at(
        &self,
        _integration_id: &str,
        full_name: &str,
        commit: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<Option<Vec<u8>>> {
        let mut s = self.state();
        s.call("file_at");
        if s.provider_reads_fail {
            return Err(PortError::retryable(
                "scm_unavailable",
                json!({ "status": 500, "detail": "server_error" }),
            ));
        }
        if full_name != s.home {
            return Ok(None);
        }
        let docs = s.doc_history.get(commit).unwrap_or(&s.docs);
        match docs.get(path) {
            Some(b) if b.len() as u64 > max_bytes => Err(PortError::permanent(
                "file_too_large",
                json!({ "path": path, "bytes": b.len(), "max_bytes": max_bytes }),
            )),
            other => Ok(other.cloned()),
        }
    }

    async fn push_branch(&self, request: &PushRequest) -> PortResult<PushOutcome> {
        let mut s = self.state();
        s.call("push");
        if s.push_outages > 0 {
            s.push_outages -= 1;
            return Err(PortError::retryable("scm_unavailable", json!({})));
        }
        let local = request.local_sha.clone();
        let repo = request.repo.full_name.clone();
        match s.refs(&repo).get(&request.branch).cloned() {
            Some(remote) if remote == local => Ok(PushOutcome::Pushed { sha: local }),
            Some(remote) if Some(&remote) != request.expected_remote_sha.as_ref() => {
                Ok(PushOutcome::Rejected { remote_sha: remote })
            }
            _ => {
                s.refs(&repo).insert(request.branch.clone(), local.clone());
                s.stats.pushes += 1;
                Ok(PushOutcome::Pushed { sha: local })
            }
        }
    }

    async fn open_change_request(&self, request: &OpenChangeRequest) -> PortResult<ChangeOpening> {
        let credential = match &request.credential {
            ChangeCredential::App => APP_CREDENTIAL.to_string(),
            ChangeCredential::User(token) => token.expose().to_string(),
        };
        let refused = match &request.credential {
            ChangeCredential::App => None,
            ChangeCredential::User(token) => self.accounts.user_pull(token),
        };
        let mut s = self.state();
        s.scm_calls.push(ScmCall {
            op: "open_change_request",
            credential: credential.clone(),
        });
        match refused {
            Some(401) => return Ok(ChangeOpening::CredentialRejected),
            Some(status) => return Ok(ChangeOpening::AccessDenied { status }),
            None => {}
        }
        let repo = request.repo.full_name.clone();
        let head = s
            .refs(&repo)
            .get(&request.head_branch)
            .cloned()
            .ok_or_else(|| PortError::permanent("head_branch_missing", json!({})))?;
        let url = |n: u64| format!("https://scm.test/{}/pull/{n}", request.repo.full_name);
        if let Some(pr) = s
            .prs
            .iter()
            .find(|p| p.repo == repo && p.head_branch == request.head_branch)
        {
            return Ok(ChangeOpening::Opened(OpenedChange {
                number: pr.number,
                url: url(pr.number),
                head_sha: head,
            }));
        }
        let number = s.prs.len() as u64 + 1;
        s.prs.push(Pr {
            repo,
            number,
            head_branch: request.head_branch.clone(),
            base_branch: request.base_branch.clone(),
            title: request.title.clone(),
            body: request.body.clone(),
            state: ChangeState::Open,
            merge_commit_sha: None,
            merged_head: None,
            opened_with: credential,
            meta: PrMeta::by_saka(self.clock.now()),
        });
        s.stats.prs_opened += 1;
        Ok(ChangeOpening::Opened(OpenedChange {
            number,
            url: url(number),
            head_sha: head,
        }))
    }

    async fn commit_checks(&self, _repo: &RepoRef, _sha: &str) -> PortResult<Vec<Check>> {
        let mut s = self.state();
        s.call("commit_checks");
        if s.commit_reads_fail {
            return Err(PortError::retryable("scm_unavailable", json!({})));
        }
        Ok(script_checks(&s.commit_ci))
    }

    async fn change_request(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSnapshot> {
        let mut s = self.state();
        s.call("change_request");
        if s.change_reads_fail {
            return Err(PortError::retryable("scm_unavailable", json!({})));
        }
        let pr = s
            .prs
            .iter()
            .find(|p| p.number == number && p.repo == repo.full_name)
            .cloned()
            .ok_or_else(|| PortError::permanent("not_found", json!({ "status": 404 })))?;
        let head = match pr.merged_head.clone() {
            Some(h) => h,
            None => s
                .refs(&pr.repo)
                .get(&pr.head_branch)
                .cloned()
                .unwrap_or_default(),
        };
        Ok(ChangeSnapshot {
            state: pr.state,
            head_sha: head,
            merge_commit_sha: pr.merge_commit_sha.clone(),
            merged_at: None,
            checks: if pr.state == ChangeState::Open {
                s.checks()
            } else {
                Vec::new()
            },
        })
    }

    async fn list_open_changes(
        &self,
        repo: &RepoRef,
        max: usize,
    ) -> PortResult<Vec<ChangeSummary>> {
        self.listed(repo, max)
    }

    async fn change_summary(&self, repo: &RepoRef, number: u64) -> PortResult<ChangeSummary> {
        self.summarized(repo, number)
    }

    async fn change_files(
        &self,
        repo: &RepoRef,
        number: u64,
        max: usize,
    ) -> PortResult<Vec<String>> {
        self.files_changed(repo, number, max)
    }

    async fn change_signals(
        &self,
        repo: &RepoRef,
        number: u64,
        since: Timestamp,
    ) -> PortResult<Vec<ChangeSignal>> {
        self.signals_since(repo, number, since)
    }

    async fn can_write(&self, _repo: &RepoRef, login: &str) -> PortResult<bool> {
        self.writes(login)
    }

    async fn merge(&self, request: &MergeRequest) -> PortResult<MergeOutcome> {
        let mut s = self.state();
        s.call("merge");
        s.merge_calls
            .push((request.repo.full_name.clone(), request.number));
        if let Some(reason) = s
            .merge_scripts
            .get_mut(&request.number)
            .and_then(VecDeque::pop_front)
        {
            return Ok(MergeOutcome::Rejected { reason });
        }
        Ok(s.merge(request.number, Some(&request.head_sha)))
    }

    async fn delete_branch(&self, repo: &RepoRef, branch: &str) -> PortResult<()> {
        let mut s = self.state();
        s.call("delete_branch");
        if s.refs(&repo.full_name).remove(branch).is_some() {
            s.stats.branches_deleted += 1;
        }
        Ok(())
    }

    async fn post_review(&self, post: &ReviewPost) -> PortResult<PostedReview> {
        post.check()?;
        let mut s = self.state();
        s.call("post_review");
        if s.review_outages > 0 {
            s.review_outages -= 1;
            return Err(PortError::retryable("scm_unavailable", json!({})));
        }
        let known = s
            .prs
            .iter()
            .any(|p| p.repo == post.repo.full_name && p.number == post.number);
        if s.reviews_refused || !known {
            return Err(PortError::permanent(
                "review_refused",
                json!({ "status": 422 }),
            ));
        }
        s.reviews.push(post.clone());
        Ok(PostedReview {
            url: Some(format!(
                "https://scm.test/{}/pull/{}#review-{}",
                post.repo.full_name,
                post.number,
                s.reviews.len()
            )),
        })
    }

    async fn report_status(
        &self,
        _repo: &RepoRef,
        sha: &str,
        report: &StatusReport,
    ) -> PortResult<()> {
        let mut s = self.state();
        s.call("report_status");
        if s.statuses_refused {
            return Err(PortError::permanent(
                "scm_status_refused",
                json!({ "status": 403 }),
            ));
        }
        s.statuses.push((sha.to_string(), report.clone()));
        Ok(())
    }

    fn capabilities(&self, integration_id: &str) -> Option<ScmCapabilities> {
        (integration_id == FAKE_INTEGRATION).then(|| self.state().capabilities.clone())
    }

    async fn app_status(&self, integration_id: &str) -> PortResult<AppStatus> {
        known_integration(integration_id)?;
        self.host.app_status().await
    }

    async fn app_installation(&self, repo: &RepoRef) -> PortResult<Installation> {
        known_integration(&repo.integration_id)?;
        self.host.installation(&repo.full_name).await
    }

    async fn webhook_status(&self, integration_id: &str) -> PortResult<WebhookStatus> {
        known_integration(integration_id)?;
        self.host.webhook().await
    }

    fn webhook_configured(&self, integration_id: &str) -> PortResult<bool> {
        if integration_id != FAKE_INTEGRATION {
            return Err(PortError::permanent(
                "scm_integration_unknown",
                json!({ "integration_id": integration_id }),
            ));
        }
        Ok(self.state().webhook_secret.is_some())
    }

    fn parse_webhook(
        &self,
        integration_id: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> PortResult<WebhookDelivery> {
        if !self.webhook_configured(integration_id)? {
            return Err(PortError::permanent("webhook_not_configured", json!({})));
        }
        let secret = self.state().webhook_secret.clone().unwrap_or_default();
        let signed = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(FAKE_SIGNATURE_HEADER))
            .map(|(_, v)| v.as_str());
        if signed != Some(fake_signature(&secret, body).as_str()) {
            return Err(PortError::permanent("webhook_signature_invalid", json!({})));
        }
        let malformed = || PortError::permanent("webhook_malformed", json!({ "field": "body" }));
        let v: Value = serde_json::from_slice(body).map_err(|_| malformed())?;
        let targets = v["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| match (t["change"].as_u64(), t["head"].as_str()) {
                (Some(number), _) => Some(WebhookTarget::Change { number }),
                (None, Some(sha)) => Some(WebhookTarget::Head {
                    sha: sha.to_string(),
                }),
                _ if t["intake"].as_bool() == Some(true) => Some(WebhookTarget::Intake),
                _ => None,
            })
            .collect();
        Ok(WebhookDelivery {
            delivery_id: v["delivery_id"].as_str().ok_or_else(malformed)?.to_string(),
            event: v["event"].as_str().unwrap_or("ping").to_string(),
            action: v["action"].as_str().map(String::from),
            repo_full_name: v["repo"].as_str().map(String::from),
            targets,
        })
    }
    fn device_flow_configured(&self, _integration_id: &str) -> PortResult<bool> {
        Ok(self.accounts.configured())
    }

    async fn start_device_flow(&self, _integration_id: &str) -> PortResult<DeviceCode> {
        self.accounts.device_code()
    }

    async fn poll_device_flow(
        &self,
        _integration_id: &str,
        device_code: &SecretText,
    ) -> PortResult<DevicePoll> {
        self.accounts.poll(device_code)
    }

    async fn refresh_user_token(
        &self,
        _integration_id: &str,
        refresh_token: &SecretText,
    ) -> PortResult<TokenRefresh> {
        self.accounts.refresh(refresh_token).await
    }

    async fn user_identity(
        &self,
        _integration_id: &str,
        token: &SecretText,
    ) -> PortResult<IdentityLookup> {
        self.accounts.identity(token)
    }

    async fn user_repository_access(
        &self,
        _integration_id: &str,
        full_name: &str,
        token: &SecretText,
    ) -> PortResult<RepoAccess> {
        Ok(self.accounts.repository_access(full_name, token))
    }
}

pub const FAKE_INTEGRATION: &str = "github";

fn known_integration(integration_id: &str) -> PortResult<()> {
    if integration_id == FAKE_INTEGRATION {
        Ok(())
    } else {
        Err(PortError::permanent(
            "scm_integration_unknown",
            json!({ "integration_id": integration_id }),
        ))
    }
}
pub const FAKE_CAPABILITIES: ScmCapabilities = ScmCapabilities {
    provider: "fake",
    merge_methods: &[MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase],
    webhook_signature: Some(WebhookSignature::HmacSha256),
    check_kinds: &[CheckKind::CheckRun, CheckKind::CommitStatus],
    draft_change_requests: true,
    delete_branch_after_merge: true,
    commit_status: true,
    user_authorship: true,
    change_head_ref: "refs/pull/{number}/head",
    diagnostics: Some(FAKE_APP_REQUIREMENTS),
};

pub const FAKE_APP_REQUIREMENTS: saka_core::diagnostics::AppRequirements =
    saka_core::diagnostics::AppRequirements {
        permissions: &[
            ("contents", "write"),
            ("pull_requests", "write"),
            ("checks", "read"),
            ("statuses", "read"),
            ("metadata", "read"),
        ],
        recommended: &[("statuses", "write")],
        events: &["pull_request", "check_run", "check_suite", "status"],
    };
pub const FAKE_SIGNATURE_HEADER: &str = "x-fake-signature";

pub fn fake_signature(secret: &[u8], body: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(secret);
    digest.update(body);
    digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[async_trait]
impl Agents for FakeWorld {
    async fn start(&self, launch: &SessionLaunch) -> PortResult<LaunchedSession> {
        let mut s = self.state();
        s.launches.push(Launch {
            owner: launch.owner,
            profile: launch.profile.clone(),
            model: launch.model.clone(),
            author: launch.author.clone(),
            vars: launch.environment.vars.clone(),
            path_prefix: launch.environment.path_prefix.clone(),
            secrets: injected(&launch.environment.secrets, SecretPurpose::Session),
            redacted: redacted(&launch.environment.secrets),
            extra_dirs: launch
                .environment
                .extra_dirs
                .iter()
                .map(|d| d.path.clone())
                .collect(),
        });
        let id = launch.session_id;
        if s.sessions.get(&id).is_some_and(|old| old.running) {
            s.stats.stale_runners_stopped += 1;
            s.events.retain(|e| e.session_id != id);
        }
        s.sessions.insert(
            id,
            FakeSession {
                env: launch.environment.environment_id,
                running: true,
                connected: true,
                silent_since: None,
                other_version: false,
                generation: 0,
                log: Vec::new(),
                turns: BTreeSet::new(),
                open_turn: None,
                current_turn: 0,
                asked: false,
                waiting_turn: None,
                answered: BTreeSet::new(),
                steers: BTreeSet::new(),
                architect: matches!(launch.profile, LaunchProfile::Architect { .. }),
            },
        );
        s.stats.session_starts += 1;
        s.resumes.push(launch.resume_cli_session_id.clone());
        let at = self.clock.now();
        s.emit(
            id,
            at,
            RunnerEventKind::CliStarted {
                pid: Some(4242),
                cli_version: Some("fake".into()),
            },
        );
        Ok(LaunchedSession {
            cli: "claude".into(),
            cli_version: Some("fake".into()),
            model: None,
            account: "default".into(),
            runner_unit: format!("saka-session-{id}"),
            socket_path: format!("/sim/run/{id}.sock"),
            log_path: format!("/sim/sessions/{id}/events.ndjson"),
        })
    }

    async fn send_turn(&self, session: &SessionRef, turn: u32, text: &str) -> PortResult<()> {
        let mut s = self.state();
        s.prompts.push(text.to_string());
        let ask = s.script.ask.clone();
        let fs = s.session_mut(session)?;
        if !fs.turns.insert(turn) {
            return Ok(());
        }
        fs.open_turn = Some(turn);
        fs.current_turn = turn;
        let first = turn == 1;
        let ask_now = ask.filter(|_| !fs.asked);
        if ask_now.is_some() {
            fs.asked = true;
            fs.waiting_turn = Some(turn);
        }
        let at = self.clock.now();
        let id = session.session_id;
        if first {
            s.emit(
                id,
                at,
                RunnerEventKind::SessionInit {
                    cli_session_id: format!("cli-{id}"),
                    model: Some("fake-model".into()),
                },
            );
        }
        if let Some((tool, input)) = ask_now {
            s.next_request += 1;
            let request_id = format!("req-{}", s.next_request);
            s.emit(
                id,
                at,
                RunnerEventKind::PermissionRequest {
                    request_id,
                    tool,
                    input,
                },
            );
            return Ok(());
        }
        s.work(id, turn, at);
        Ok(())
    }

    async fn answer_permission(
        &self,
        session: &SessionRef,
        request_id: &str,
        allow: bool,
        _message: Option<&str>,
    ) -> PortResult<()> {
        let mut s = self.state();
        let fs = s.session_mut(session)?;
        let first = fs.answered.insert(request_id.to_string());
        let resume = if first { fs.waiting_turn.take() } else { None };
        s.stats.answer_frames += 1;
        if first {
            s.stats.answers_to_cli += 1;
        }
        let at = self.clock.now();
        let id = session.session_id;
        s.emit(
            id,
            at,
            RunnerEventKind::PermissionDelivered {
                request_id: request_id.to_string(),
                behavior: if allow { "allow" } else { "deny" }.to_string(),
            },
        );
        if let Some(turn) = resume {
            s.work(id, turn, at);
        }
        Ok(())
    }

    async fn interrupt(&self, session: &SessionRef) -> PortResult<()> {
        let mut s = self.state();
        let open = s.session_mut(session)?.open_turn;
        s.stats.interrupts += 1;
        if let Some(turn) = open {
            let at = self.clock.now();
            s.emit(
                session.session_id,
                at,
                RunnerEventKind::TurnEnded {
                    turn,
                    status: "error_during_execution".into(),
                    cost_usd_micros: Some(10),
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    result_summary: None,
                },
            );
        }
        Ok(())
    }

    async fn steer(&self, session: &SessionRef, n: u32, text: &str) -> PortResult<()> {
        let mut s = self.state();
        let fs = s.session_mut(session)?;
        if !fs.steers.insert(n) {
            return Ok(());
        }
        let turn = fs.current_turn;
        s.stats.steers_to_cli += 1;
        s.prompts.push(text.to_string());
        let at = self.clock.now();
        s.emit(
            session.session_id,
            at,
            RunnerEventKind::Steer {
                n,
                turn,
                text: text.to_string(),
            },
        );
        Ok(())
    }

    async fn reattach(&self, session: &SessionRef, from_offset: u64) -> PortResult<Reattached> {
        let mut s = self.state();
        let unreachable = || {
            PortError::retryable(
                "runner_unreachable",
                json!({ "session_id": session.session_id }),
            )
        };
        let fs = s
            .sessions
            .get_mut(&session.session_id)
            .ok_or_else(unreachable)?;
        if !fs.running || fs.silent_since.is_some() {
            return Err(unreachable());
        }
        if fs.other_version {
            fs.running = false;
            return Err(PortError::permanent(
                "version_mismatch",
                json!({ "session_id": session.session_id }),
            ));
        }
        fs.connected = true;
        let replay: Vec<RunnerEvent> = fs
            .log
            .iter()
            .filter(|e| e.offset >= from_offset)
            .cloned()
            .collect();
        let next_offset = fs.log.len() as u64;
        s.events.retain(|e| e.session_id != session.session_id);
        s.events.extend(replay);
        s.stats.reattaches += 1;
        Ok(Reattached {
            runner_version: SIM_RUNNER_VERSION.into(),
            next_offset,
        })
    }

    async fn unit_active(&self, session: &SessionRef) -> PortResult<bool> {
        Ok(self
            .state()
            .sessions
            .get(&session.session_id)
            .is_some_and(|s| s.running))
    }

    fn link(&self, session: SessionId) -> Link {
        let now = self.clock.now();
        match self.state().sessions.get(&session) {
            Some(s) if s.connected => Link::Open {
                silent_ms: s.silent_since.map_or(0, |since| now.since(since)),
            },
            Some(s) => Link::Closed {
                generation: s.generation,
            },
            None => Link::Closed { generation: 0 },
        }
    }

    fn disconnect(&self, session: SessionId) {
        let mut s = self.state();
        if let Some(fs) = s.sessions.get_mut(&session).filter(|f| f.connected) {
            fs.connected = false;
            fs.generation += 1;
        }
        s.events.retain(|e| e.session_id != session);
    }

    async fn stop(&self, session_id: SessionId, _runner_unit: Option<&str>) -> PortResult<()> {
        let mut s = self.state();
        if let Some(fs) = s.sessions.get_mut(&session_id)
            && fs.running
        {
            fs.running = false;
            fs.connected = false;
            s.stats.sessions_stopped += 1;
            s.events.retain(|e| e.session_id != session_id);
        }
        Ok(())
    }

    async fn agent_cli(&self) -> PortResult<AgentCli> {
        self.host.agent_cli().await
    }

    async fn agent_login(&self) -> PortResult<AgentLogin> {
        self.host.agent_login().await
    }
}

#[async_trait]
impl Toolchains for FakeWorld {
    async fn install(&self, tool: Tool, version: &str) -> PortResult<ToolchainUse> {
        let mut s = self.state();
        let params = |cause: &str| json!({ "tool": tool, "version": version, "cause": cause, "detail": null });
        match s
            .toolchain_scripts
            .get_mut(&tool)
            .and_then(VecDeque::pop_front)
        {
            Some(ToolchainScript::Missing) => {
                return Err(PortError::permanent(
                    "toolchain_unavailable",
                    params("not_found"),
                ));
            }
            Some(ToolchainScript::DownloadFails) => {
                return Err(PortError::retryable(
                    "toolchain_unavailable",
                    params("download_failed"),
                ));
            }
            None => {}
        }
        let downloaded = !s
            .toolchain_installs
            .iter()
            .any(|(t, v, _)| *t == tool && v == version);
        s.toolchain_installs
            .push((tool, version.to_string(), downloaded));
        let env = match tool {
            Tool::Go => BTreeMap::from([
                ("GOTOOLCHAIN".to_string(), "local".to_string()),
                ("GOMODCACHE".to_string(), "/sim/caches/go/mod".to_string()),
                ("GOCACHE".to_string(), "/sim/caches/go/build".to_string()),
            ]),
            Tool::Pnpm => BTreeMap::from([
                (
                    "npm_config_store_dir".to_string(),
                    "/sim/caches/pnpm-store".to_string(),
                ),
                (
                    "COREPACK_ENABLE_DOWNLOAD_PROMPT".to_string(),
                    "0".to_string(),
                ),
            ]),
            Tool::Node => BTreeMap::new(),
        };
        Ok(ToolchainUse {
            tool,
            version: version.to_string(),
            bin_dir: format!("/sim/toolchains/{tool}/{version}/bin"),
            env,
            downloaded,
        })
    }

    async fn cache(&self) -> PortResult<ToolchainCache> {
        self.host.cache().await
    }

    async fn host_version(&self, tool: HostTool) -> PortResult<Option<String>> {
        self.host.host_version(tool).await
    }
}

#[async_trait]
impl Supervisor for FakeWorld {
    async fn start(&self, _unit: &RunnerUnit) -> PortResult<()> {
        Err(PortError::permanent("supervisor_unsupported", json!({})))
    }

    async fn stop(&self, _name: &str) -> PortResult<()> {
        Err(PortError::permanent("supervisor_unsupported", json!({})))
    }

    async fn is_active(&self, _name: &str) -> PortResult<bool> {
        Err(PortError::permanent("supervisor_unsupported", json!({})))
    }

    async fn run(&self, _unit: &CommandUnit) -> PortResult<CommandOutcome> {
        Err(PortError::permanent("supervisor_unsupported", json!({})))
    }

    async fn probe(&self) -> PortResult<SupervisorProbe> {
        self.host.probe().await
    }
}

#[async_trait]
impl ReleaseIndex for FakeWorld {
    async fn releases(&self, tool: Tool) -> PortResult<Vec<Release>> {
        self.host.releases(tool).await
    }
}

// Not encryption: the AAD followed by the value XOR a constant, so a row moved to another
// scope fails to open and the plain value never lands in the database.
impl SecretStore for FakeWorld {
    fn seal(&self, scope: &SecretScope<'_>, value: &SecretText) -> PortResult<Sealed> {
        let mut ciphertext = scope.aad();
        ciphertext.push(0xff);
        ciphertext.extend(value.expose().bytes().map(|b| b ^ 0x5a));
        Ok(Sealed {
            nonce: vec![0; 24],
            ciphertext,
        })
    }

    fn open(&self, scope: &SecretScope<'_>, sealed: &Sealed) -> PortResult<SecretText> {
        let unreadable =
            || PortError::permanent("secret_unreadable", json!({ "name": scope.label() }));
        if self.state().secrets_unreadable {
            return Err(unreadable());
        }
        let mut prefix = scope.aad();
        prefix.push(0xff);
        let value = sealed
            .ciphertext
            .strip_prefix(prefix.as_slice())
            .ok_or_else(unreadable)?;
        String::from_utf8(value.iter().map(|b| b ^ 0x5a).collect())
            .map(SecretText::new)
            .map_err(|_| unreadable())
    }

    fn fingerprint(&self, scope: &SecretScope<'_>, value: &SecretText) -> String {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (scope.aad(), value.expose()).hash(&mut h);
        format!("fake:{:016x}", h.finish())
    }
}
