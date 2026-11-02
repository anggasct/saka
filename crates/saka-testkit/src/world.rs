use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use saka_core::gate::Check;
use saka_core::ids::ProjectId;
use saka_core::ids::{EnvironmentId, SessionId};
use saka_core::model::ToolchainUse;
use saka_core::policy::RequestPath;
use saka_core::secret::{SecretText, SecretVar, aad};
use saka_core::time::Timestamp;
use saka_core::vocab::SecretPurpose;
use saka_core::vocab::Tool;
use saka_core::vocab::{ChangeState, CheckConclusion, CheckKind, CheckStatus, MergeMethod};
use saka_ports::{
    Agents, Blob, ChangeSnapshot, Clock, DocFile, DocSource, EnvRef, Environments, LaunchedSession,
    Link, MergeOutcome, MergeRequest, OpenChangeRequest, OpenedChange, PortError, PortResult,
    PrepareRequest, PreparedEnvironment, PushOutcome, PushRequest, Reattached, RecipeCommand,
    RecipeCommandOutcome, RepoInfo, RepoRef, RunnerEvent, RunnerEventKind, Scm, ScmCapabilities,
    Sealed, SecretStore, SessionLaunch, SessionRef, Toolchains, WebhookDelivery, WebhookSignature,
    WebhookTarget, WorktreeState,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::clock::FakeClock;

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
}

#[derive(Debug, Clone)]
struct Worktree {
    path: String,
    base_sha: String,
    head: String,
    dirty: bool,
    torn_down: bool,
}

#[derive(Debug, Clone)]
pub struct Pr {
    pub number: u64,
    pub head_branch: String,
    pub base_branch: String,
    pub title: String,
    pub body: String,
    pub state: ChangeState,
    pub merge_commit_sha: Option<String>,
    pub merged_head: Option<String>,
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
}

struct State {
    script: AgentScript,
    ci: CiScript,
    lose_merges: bool,
    refuse_teardowns: bool,
    next_sha: u64,
    parents: HashMap<String, Vec<String>>,
    remote: BTreeMap<String, String>,
    worktrees: BTreeMap<EnvironmentId, Worktree>,
    prs: Vec<Pr>,
    sessions: BTreeMap<SessionId, FakeSession>,
    events: VecDeque<RunnerEvent>,
    stats: Stats,
    next_request: u32,
    docs: BTreeMap<String, Vec<u8>>,
    doc_history: HashMap<String, BTreeMap<String, Vec<u8>>>,
    inaccessible: BTreeSet<String>,
    prompts: Vec<String>,
    resumes: Vec<Option<String>>,
    recipe: Option<String>,
    recipe_runs: Vec<RecipeRun>,
    command_scripts: HashMap<String, VecDeque<CommandScript>>,
    launches: Vec<Launch>,
    repo_files: BTreeMap<String, String>,
    toolchain_scripts: HashMap<Tool, VecDeque<ToolchainScript>>,
    toolchain_installs: Vec<(Tool, String, bool)>,
    secrets_unreadable: bool,
    webhook_secret: Option<Vec<u8>>,
    capabilities: ScmCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub vars: BTreeMap<String, String>,
    pub path_prefix: Vec<String>,
    pub secrets: BTreeMap<String, String>,
    pub redacted: Vec<String>,
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

pub struct FakeWorld {
    clock: Arc<FakeClock>,
    state: Mutex<State>,
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

impl FakeWorld {
    pub fn new(clock: Arc<FakeClock>, script: AgentScript, ci: CiScript) -> FakeWorld {
        let root = sha(0);
        let mut parents = HashMap::new();
        parents.insert(root.clone(), Vec::new());
        let mut remote = BTreeMap::new();
        remote.insert(BASE_BRANCH.to_string(), root);
        FakeWorld {
            clock,
            state: Mutex::new(State {
                script,
                ci,
                lose_merges: false,
                refuse_teardowns: false,
                recipe: None,
                recipe_runs: Vec::new(),
                command_scripts: HashMap::new(),
                launches: Vec::new(),
                repo_files: BTreeMap::new(),
                toolchain_scripts: HashMap::new(),
                toolchain_installs: Vec::new(),
                secrets_unreadable: false,
                webhook_secret: None,
                capabilities: FAKE_CAPABILITIES,
                next_sha: 1,
                parents,
                remote,
                worktrees: BTreeMap::new(),
                prs: Vec::new(),
                sessions: BTreeMap::new(),
                events: VecDeque::new(),
                stats: Stats::default(),
                next_request: 0,
                docs: BTreeMap::new(),
                doc_history: HashMap::new(),
                inaccessible: BTreeSet::new(),
                prompts: Vec::new(),
                resumes: Vec::new(),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub fn stats(&self) -> Stats {
        self.state().stats.clone()
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
        self.state().remote.clone()
    }

    pub fn base_head(&self) -> String {
        self.state().remote[BASE_BRANCH].clone()
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
        let base = s.remote[BASE_BRANCH].clone();
        let commit = s.new_commit(vec![base]);
        s.remote.insert(BASE_BRANCH.to_string(), commit.clone());
        s.docs.insert(path.to_string(), bytes.to_vec());
        let snapshot = s.docs.clone();
        s.doc_history.insert(commit.clone(), snapshot);
        commit
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

fn sha(n: u64) -> String {
    format!("{n:040x}")
}

impl State {
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
        let check = |name: &str, conclusion| Check {
            name: name.to_string(),
            kind: CheckKind::CheckRun,
            status: CheckStatus::Completed,
            conclusion: Some(conclusion),
            url: None,
        };
        match &self.ci {
            CiScript::Pass => vec![check("test", CheckConclusion::Success)],
            CiScript::Fail(names) => names
                .iter()
                .map(|n| check(n, CheckConclusion::Failure))
                .collect(),
            CiScript::Silent => Vec::new(),
        }
    }

    fn merge(&mut self, number: u64, expected_head: Option<&str>) -> MergeOutcome {
        let Some(i) = self.prs.iter().position(|p| p.number == number) else {
            return MergeOutcome::Rejected {
                reason: "rejected".into(),
            };
        };
        let pr = self.prs[i].clone();
        let head = self
            .remote
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
        let base = self.remote[&pr.base_branch].clone();
        let merge = self.new_commit(vec![base, head.clone()]);
        if !self.lose_merges {
            self.remote.insert(pr.base_branch.clone(), merge.clone());
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
        let Some(env) = self.sessions.get(&session).map(|s| s.env) else {
            return;
        };
        let outcome = self.script.outcome.clone();
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
            if s.commits_ahead(&w.base_sha, &w.head) == 0 {
                let c = s.new_commit(vec![w.head.clone()]);
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
            .remote
            .get(&request.base_branch)
            .cloned()
            .ok_or_else(|| PortError::permanent("base_branch_missing", json!({})))?;
        s.worktrees.insert(
            request.environment_id,
            Worktree {
                path: path.clone(),
                base_sha: base.clone(),
                head: base.clone(),
                dirty: false,
                torn_down: false,
            },
        );
        s.stats.prepares += 1;
        Ok(PreparedEnvironment {
            root_path: root,
            worktree_path: path,
            base_sha: base,
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

    async fn is_merged(
        &self,
        _repo: &RepoRef,
        commit: &str,
        base_branch: &str,
    ) -> PortResult<bool> {
        let s = self.state();
        let base = s.remote.get(base_branch).cloned().unwrap_or_default();
        Ok(s.is_ancestor(commit, &base))
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

    async fn read_recipe(&self, _repo: &RepoRef, _commit: &str) -> PortResult<Option<String>> {
        Ok(self.state().recipe.clone())
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
                "/sim/logs/environments/{}/{}-{}.log",
                env.environment_id,
                command.phase.as_str(),
                command.index
            ),
        })
    }

    fn resolve_path(&self, env: &EnvRef, raw: &str) -> RequestPath {
        let root = env.worktree_path.trim_end_matches('/');
        let relative = if let Some(rest) = raw.strip_prefix(&format!("{root}/")) {
            rest.to_string()
        } else if raw.starts_with('/') {
            return RequestPath {
                path: raw.to_string(),
                inside_worktree: false,
            };
        } else {
            raw.to_string()
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
                        };
                    }
                }
                p => parts.push(p),
            }
        }
        RequestPath {
            path: parts.join("/"),
            inside_worktree: true,
        }
    }
}

#[async_trait]
impl DocSource for FakeWorld {
    async fn read_at_tip(
        &self,
        _repo: &RepoRef,
        branch: &str,
        path: &str,
    ) -> PortResult<Option<DocFile>> {
        let s = self.state();
        let commit = s.remote.get(branch).cloned().ok_or_else(|| {
            PortError::permanent("base_branch_missing", json!({ "base_branch": branch }))
        })?;
        Ok(s.docs.get(path).map(|bytes| DocFile {
            path: path.to_string(),
            commit,
            bytes: bytes.clone(),
        }))
    }

    async fn read_at(
        &self,
        _repo: &RepoRef,
        commit: &str,
        path: &str,
        max_bytes: u64,
    ) -> PortResult<Blob> {
        let s = self.state();
        let docs = s.doc_history.get(commit).unwrap_or(&s.docs);
        Ok(match docs.get(path) {
            None => Blob::Missing,
            Some(b) if b.len() as u64 > max_bytes => Blob::TooLarge {
                bytes: b.len() as u64,
            },
            Some(b) => Blob::Bytes(b.clone()),
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

    async fn push_branch(&self, request: &PushRequest) -> PortResult<PushOutcome> {
        let mut s = self.state();
        let local = request.local_sha.clone();
        match s.remote.get(&request.branch).cloned() {
            Some(remote) if remote == local => Ok(PushOutcome::Pushed { sha: local }),
            Some(remote) if Some(&remote) != request.expected_remote_sha.as_ref() => {
                Ok(PushOutcome::Rejected { remote_sha: remote })
            }
            _ => {
                s.remote.insert(request.branch.clone(), local.clone());
                s.stats.pushes += 1;
                Ok(PushOutcome::Pushed { sha: local })
            }
        }
    }

    async fn open_change_request(&self, request: &OpenChangeRequest) -> PortResult<OpenedChange> {
        let mut s = self.state();
        let head = s
            .remote
            .get(&request.head_branch)
            .cloned()
            .ok_or_else(|| PortError::permanent("head_branch_missing", json!({})))?;
        let url = |n: u64| format!("https://scm.test/{}/pull/{n}", request.repo.full_name);
        if let Some(pr) = s.prs.iter().find(|p| p.head_branch == request.head_branch) {
            return Ok(OpenedChange {
                number: pr.number,
                url: url(pr.number),
                head_sha: head,
            });
        }
        let number = s.prs.len() as u64 + 1;
        s.prs.push(Pr {
            number,
            head_branch: request.head_branch.clone(),
            base_branch: request.base_branch.clone(),
            title: request.title.clone(),
            body: request.body.clone(),
            state: ChangeState::Open,
            merge_commit_sha: None,
            merged_head: None,
        });
        s.stats.prs_opened += 1;
        Ok(OpenedChange {
            number,
            url: url(number),
            head_sha: head,
        })
    }

    async fn change_request(&self, _repo: &RepoRef, number: u64) -> PortResult<ChangeSnapshot> {
        let s = self.state();
        let pr = s
            .prs
            .iter()
            .find(|p| p.number == number)
            .ok_or_else(|| PortError::permanent("not_found", json!({ "status": 404 })))?;
        let head = pr
            .merged_head
            .clone()
            .or_else(|| s.remote.get(&pr.head_branch).cloned())
            .unwrap_or_default();
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

    async fn merge(&self, request: &MergeRequest) -> PortResult<MergeOutcome> {
        let mut s = self.state();
        Ok(s.merge(request.number, Some(&request.head_sha)))
    }

    async fn delete_branch(&self, _repo: &RepoRef, branch: &str) -> PortResult<()> {
        let mut s = self.state();
        if s.remote.remove(branch).is_some() {
            s.stats.branches_deleted += 1;
        }
        Ok(())
    }

    fn capabilities(&self, integration_id: &str) -> Option<ScmCapabilities> {
        (integration_id == FAKE_INTEGRATION).then(|| self.state().capabilities.clone())
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
}

pub const FAKE_INTEGRATION: &str = "github";
pub const FAKE_CAPABILITIES: ScmCapabilities = ScmCapabilities {
    provider: "fake",
    merge_methods: &[MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase],
    webhook_signature: Some(WebhookSignature::HmacSha256),
    check_kinds: &[CheckKind::CheckRun, CheckKind::CommitStatus],
    draft_change_requests: true,
    delete_branch_after_merge: true,
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
            vars: launch.environment.vars.clone(),
            path_prefix: launch.environment.path_prefix.clone(),
            secrets: injected(&launch.environment.secrets, SecretPurpose::Session),
            redacted: redacted(&launch.environment.secrets),
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
}

// Not encryption: the AAD followed by the value XOR a constant, so a row moved to another
// project fails to open and the plain value never lands in the database.
impl SecretStore for FakeWorld {
    fn seal(&self, project: ProjectId, name: &str, value: &SecretText) -> PortResult<Sealed> {
        let mut ciphertext = aad(project.0, name);
        ciphertext.push(0xff);
        ciphertext.extend(value.expose().bytes().map(|b| b ^ 0x5a));
        Ok(Sealed {
            nonce: vec![0; 24],
            ciphertext,
        })
    }

    fn open(&self, project: ProjectId, name: &str, sealed: &Sealed) -> PortResult<SecretText> {
        let unreadable = || PortError::permanent("secret_unreadable", json!({ "name": name }));
        if self.state().secrets_unreadable {
            return Err(unreadable());
        }
        let mut prefix = aad(project.0, name);
        prefix.push(0xff);
        let value = sealed
            .ciphertext
            .strip_prefix(prefix.as_slice())
            .ok_or_else(unreadable)?;
        String::from_utf8(value.iter().map(|b| b ^ 0x5a).collect())
            .map(SecretText::new)
            .map_err(|_| unreadable())
    }

    fn fingerprint(&self, project: ProjectId, name: &str, value: &SecretText) -> String {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (project.0, name, value.expose()).hash(&mut h);
        format!("fake:{:016x}", h.finish())
    }
}
