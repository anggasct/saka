//! One in-memory world behind the environment, SCM, and agent ports.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use saka_core::gate::Check;
use saka_core::ids::{EnvironmentId, SessionId};
use saka_core::policy::RequestPath;
use saka_core::time::Timestamp;
use saka_core::vocab::{ChangeState, CheckConclusion, CheckKind, CheckStatus};
use saka_ports::{
    Agents, ChangeSnapshot, Clock, DocFile, DocSource, EnvRef, Environments, LaunchedSession,
    MergeOutcome, MergeRequest, OpenChangeRequest, OpenedChange, PortError, PortResult,
    PrepareRequest, PreparedEnvironment, PushOutcome, PushRequest, RepoInfo, RepoRef, RunnerEvent,
    RunnerEventKind, Scm, SessionLaunch, SessionRef, WorktreeState,
};
use serde_json::{Value, json};

use crate::clock::FakeClock;

/// What the fake agent does with a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentOutcome {
    /// Commits once (unless the branch already has a commit), then ends the turn.
    Commit,
    /// Leaves changes uncommitted on the first turn and commits on a follow-up.
    CommitOnFollowup,
    /// Leaves changes uncommitted on every turn.
    LeaveDirty,
    /// Ends every turn without any change.
    NoCommit,
    /// Ends the turn with this error subtype.
    Fail(String),
    /// The CLI exits without a result.
    Exit,
    /// Never ends the turn.
    Hang,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentScript {
    pub outcome: AgentOutcome,
    /// A tool request the agent makes before working on the first turn: `(tool, input)`.
    pub ask: Option<(String, Value)>,
}

impl Default for AgentScript {
    fn default() -> Self {
        AgentScript {
            outcome: AgentOutcome::Commit,
            ask: None,
        }
    }
}

/// What CI reports for every head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiScript {
    Pass,
    /// These checks fail.
    Fail(Vec<String>),
    /// No check ever appears.
    Silent,
}

/// Every effect the world saw.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub prepares: u32,
    pub teardowns: u32,
    /// Remote branch updates (a push to the same sha is not one).
    pub pushes: u32,
    pub prs_opened: u32,
    pub merges: u32,
    pub branches_deleted: u32,
    pub session_starts: u32,
    /// Runners left over from an attempt that never got recorded.
    pub stale_runners_stopped: u32,
    pub sessions_stopped: u32,
    pub interrupts: u32,
    /// Permission answers that reached the CLI (one per request at most).
    pub answers_to_cli: u32,
    /// Every answer frame the runner received, including repeats.
    pub answer_frames: u32,
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
    next_offset: u64,
    turns: BTreeSet<u32>,
    asked: bool,
    /// The turn waiting for a permission answer.
    waiting_turn: Option<u32>,
    answered: BTreeSet<String>,
}

struct State {
    script: AgentScript,
    ci: CiScript,
    /// Merges do not reach the base branch (to exercise sync verification).
    lose_merges: bool,
    next_sha: u64,
    parents: HashMap<String, Vec<String>>,
    remote: BTreeMap<String, String>,
    worktrees: BTreeMap<EnvironmentId, Worktree>,
    prs: Vec<Pr>,
    sessions: BTreeMap<SessionId, FakeSession>,
    events: VecDeque<RunnerEvent>,
    stats: Stats,
    next_request: u32,
    /// Files on the base branch, by path.
    docs: BTreeMap<String, Vec<u8>>,
    /// Repositories the integration cannot reach.
    inaccessible: BTreeSet<String>,
}

pub struct FakeWorld {
    clock: Arc<FakeClock>,
    state: Mutex<State>,
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
                inaccessible: BTreeSet::new(),
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

    /// The next runner event, in the order runners wrote them.
    pub fn pop_event(&self) -> Option<RunnerEvent> {
        self.state().events.pop_front()
    }

    pub fn has_events(&self) -> bool {
        !self.state().events.is_empty()
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

    /// `ancestor` is reachable from `descendant`.
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        self.state().is_ancestor(ancestor, descendant)
    }

    pub fn running_sessions(&self) -> Vec<SessionId> {
        self.state()
            .sessions
            .iter()
            .filter(|(_, s)| s.running)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Someone commits `bytes` at `path` on the base branch.
    pub fn put_doc(&self, path: &str, bytes: &[u8]) -> String {
        let mut s = self.state();
        let base = s.remote[BASE_BRANCH].clone();
        let commit = s.new_commit(vec![base]);
        s.remote.insert(BASE_BRANCH.to_string(), commit.clone());
        s.docs.insert(path.to_string(), bytes.to_vec());
        commit
    }

    /// The integration loses access to this repository.
    pub fn make_inaccessible(&self, full_name: &str) {
        self.state().inaccessible.insert(full_name.to_string());
    }

    /// Someone merges the change on the host, outside Saka.
    pub fn merge_outside(&self, number: u64) {
        let mut s = self.state();
        s.merge(number, None);
    }

    /// Someone closes the change without merging.
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

    /// Commits on `head` that are not on `base`, following first parents.
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
        let offset = s.next_offset;
        s.next_offset += 1;
        self.events.push_back(RunnerEvent {
            session_id: session,
            offset,
            at,
            kind,
        });
    }

    /// The agent works on `turn` after any permission answer.
    fn work(&mut self, session: SessionId, turn: u32, at: Timestamp) {
        let Some(env) = self.sessions.get(&session).map(|s| s.env) else {
            return;
        };
        let outcome = self.script.outcome.clone();
        let end = |s: &mut State, status: &str| {
            s.emit(
                session,
                at,
                RunnerEventKind::TurnEnded {
                    turn,
                    status: status.to_string(),
                    cost_usd_micros: Some(1_000),
                    input_tokens: Some(100),
                    output_tokens: Some(50),
                    result_summary: None,
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
            Some(s) if s.running => Ok(s),
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
        if let Some(w) = s.worktrees.get_mut(&env.environment_id)
            && !w.torn_down
        {
            w.torn_down = true;
            s.stats.teardowns += 1;
        }
        Ok(())
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
}

#[async_trait]
impl Agents for FakeWorld {
    async fn start(&self, launch: &SessionLaunch) -> PortResult<LaunchedSession> {
        let mut s = self.state();
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
                next_offset: 0,
                turns: BTreeSet::new(),
                asked: false,
                waiting_turn: None,
                answered: BTreeSet::new(),
            },
        );
        s.stats.session_starts += 1;
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

    async fn send_turn(&self, session: &SessionRef, turn: u32, _text: &str) -> PortResult<()> {
        let mut s = self.state();
        let ask = s.script.ask.clone();
        let fs = s.session_mut(session)?;
        if !fs.turns.insert(turn) {
            return Ok(());
        }
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
        s.session_mut(session)?;
        s.stats.interrupts += 1;
        Ok(())
    }

    async fn stop(&self, session_id: SessionId, _runner_unit: Option<&str>) -> PortResult<()> {
        let mut s = self.state();
        if let Some(fs) = s.sessions.get_mut(&session_id)
            && fs.running
        {
            fs.running = false;
            s.stats.sessions_stopped += 1;
            s.events.retain(|e| e.session_id != session_id);
        }
        Ok(())
    }
}
