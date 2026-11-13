use saka_core::draft::DraftInput;
use saka_core::ids::{PermissionRequestId, RepoId};
use saka_core::model::{Draft, Environment, PermissionRequest, RunOwner, Session, Task};
use saka_core::policy::{Effect, ToolRequest};
use saka_core::session::SessionInput;
use saka_core::task::TaskInput;
use saka_core::vocab::{DraftMode, Stage, StagePhase};
use saka_ports::{EnvRef, RunnerEvent, RunnerEventKind};
use saka_store::read;
use serde_json::Value;

use crate::{Engine, Extra, Mode, Result, timeline};

pub(crate) const FILE_TOOLS: &[(&str, &str)] = &[
    ("Read", "file_path"),
    ("Write", "file_path"),
    ("Edit", "file_path"),
    ("MultiEdit", "file_path"),
    ("NotebookEdit", "notebook_path"),
    ("Glob", "path"),
    ("Grep", "path"),
    ("LS", "path"),
];
const SUMMARY_MAX: usize = 200;

enum Run {
    Task(Box<Task>),
    Draft(Box<Draft>),
}

impl Run {
    fn session(&self) -> Option<&Session> {
        match self {
            Run::Task(t) => t.session.as_ref(),
            Run::Draft(d) => d.session.as_ref(),
        }
    }

    fn permissions(&self) -> &[PermissionRequest] {
        match self {
            Run::Task(t) => &t.permissions,
            Run::Draft(d) => &d.permissions,
        }
    }
}

impl Engine {
    pub fn on_runner_event(&self, event: RunnerEvent) -> Result<()> {
        let Some(owner) = self
            .store
            .read(|c| read::session_owner(c, event.session_id))?
        else {
            return Ok(());
        };
        let run = match owner {
            RunOwner::StageRun { work_item_id, .. } => {
                Run::Task(Box::new(self.load_task(work_item_id)?))
            }
            RunOwner::DraftAttempt { spec_draft_id, .. } => {
                match self
                    .store
                    .read(|c| saka_store::draft::load_draft(c, spec_draft_id))?
                {
                    Some(d) => Run::Draft(Box::new(d)),
                    None => return Ok(()),
                }
            }
        };
        let Some(session) = run.session().filter(|s| s.id == event.session_id) else {
            return Ok(());
        };
        let replayed =
            event.offset <= session.last_offset && !(event.offset == 0 && session.last_offset == 0);
        if replayed {
            return Ok(());
        }
        let live = session.state.is_live();
        let mut permission = None;
        let mut inputs: Vec<SessionInput> = match &event.kind {
            RunnerEventKind::SessionInit {
                cli_session_id,
                model,
            } => vec![SessionInput::Initialized {
                cli_session_id: cli_session_id.clone(),
                model: model.clone(),
            }],
            RunnerEventKind::PermissionRequest {
                request_id,
                tool,
                input,
            } if live => {
                let next =
                    self.permission_request(&run, request_id.clone(), tool.clone(), input.clone())?;
                if let SessionInput::PermissionRequested {
                    permission_request_id,
                    summary,
                    decision,
                    tool,
                    ..
                } = &next
                {
                    let state = match decision.effect {
                        Effect::Allow => "allowed",
                        Effect::Deny => "denied",
                        Effect::Ask => "pending",
                    };
                    self.counters.permission_request(tool, state);
                    permission = Some((*permission_request_id, summary.clone(), state));
                }
                vec![next]
            }
            RunnerEventKind::PermissionDelivered {
                request_id,
                behavior,
            } => {
                let found = run
                    .permissions()
                    .iter()
                    .find(|p| &p.cli_request_id == request_id);
                if let Some(p) = found {
                    let state = if behavior == "allow" {
                        "allowed"
                    } else {
                        "denied"
                    };
                    permission = Some((p.id, p.summary.clone(), state));
                }
                found
                    .map(|p| SessionInput::PermissionDelivered {
                        permission_request_id: p.id,
                    })
                    .into_iter()
                    .collect()
            }
            RunnerEventKind::TurnEnded {
                turn,
                status,
                cost_usd_micros,
                input_tokens,
                output_tokens,
                ..
            } => vec![SessionInput::TurnEnded {
                turn: *turn,
                status: status.clone(),
                cost_usd_micros: *cost_usd_micros,
                input_tokens: *input_tokens,
                output_tokens: *output_tokens,
            }],
            RunnerEventKind::CliExited { code, signal } => vec![SessionInput::CliExited {
                code: *code,
                signal: *signal,
            }],
            RunnerEventKind::RateLimit { info } => timeline::quota_windows(info),
            RunnerEventKind::RunnerError { code, .. } => {
                tracing::warn!(session_id = %event.session_id, code = %code, "runner error");
                vec![]
            }
            RunnerEventKind::PermissionRequest { .. }
            | RunnerEventKind::CliStarted { .. }
            | RunnerEventKind::Activity { .. }
            | RunnerEventKind::Steer { .. } => vec![],
        };
        let row = timeline::activity(
            &event,
            session.owner,
            permission
                .as_ref()
                .map(|(id, summary, state)| (*id, summary.as_str(), *state)),
        );
        let cause = self.cause("system:runner");
        let first = if inputs.is_empty() {
            None
        } else {
            Some(inputs.remove(0))
        };
        let extra = Extra {
            activity: row,
            offset: Some((event.session_id, event.offset)),
            ..Extra::default()
        };
        let decide = |input: Option<SessionInput>, extra: Extra<'_>| match &run {
            Run::Task(t) => self
                .decide(
                    t.item.id,
                    input.map(TaskInput::Session),
                    Mode::Fact,
                    &cause,
                    extra,
                )
                .map(|_| ()),
            Run::Draft(d) => self
                .decide_draft(
                    d.draft.id,
                    input.map(DraftInput::Session),
                    Mode::Fact,
                    &cause,
                    extra,
                )
                .map(|_| ()),
        };
        decide(first, extra)?;
        for input in inputs {
            decide(Some(input), Extra::default())?;
        }
        if let (
            Run::Task(t),
            RunnerEventKind::TurnEnded {
                turn,
                status,
                result_text,
                ..
            },
        ) = (&run, &event.kind)
            && status == "success"
            && t.is_at(Stage::Review, StagePhase::RunningSession)
        {
            let report = TaskInput::ReviewReported {
                session_id: event.session_id,
                turn: *turn,
                text: result_text.clone().unwrap_or_default(),
            };
            self.decide(
                t.item.id,
                Some(report),
                Mode::Fact,
                &cause,
                Extra::default(),
            )?;
        }
        Ok(())
    }

    fn permission_request(
        &self,
        run: &Run,
        request_id: String,
        tool: String,
        input: Value,
    ) -> Result<SessionInput> {
        let decision = match run {
            Run::Task(task) => {
                let request = self.tool_request(
                    task.environment.as_ref(),
                    task.item.repo_id,
                    "",
                    &tool,
                    &input,
                    Vec::new(),
                );
                if task
                    .stage_run
                    .as_ref()
                    .is_some_and(|r| r.stage == Stage::Review)
                {
                    saka_core::policy::reviewer(&request, &self.cfg.policy)
                } else {
                    let rules = self
                        .store
                        .read(|c| read::active_permission_rules(c, task.item.project_id))?;
                    self.cfg.policy.evaluate_with(&request, &rules)
                }
            }
            Run::Draft(d) => {
                let (project, _) = self.workspace_of(d.draft.project_id)?;
                let cwd = match project.project_path.trim_matches('/') {
                    "." => String::new(),
                    dir => dir.to_string(),
                };
                let request = self.tool_request(
                    d.environment.as_ref(),
                    project.workspace_repo_id,
                    &cwd,
                    &tool,
                    &input,
                    self.code_extra_dirs(d),
                );
                match d.draft.mode {
                    DraftMode::Draft => {
                        saka_core::policy::architect(&request, &d.draft.path, &self.cfg.policy)
                    }
                    DraftMode::Session => saka_core::policy::architect_session(
                        &request,
                        &project.project_path,
                        &self.cfg.policy,
                    ),
                }
            }
        };
        Ok(SessionInput::PermissionRequested {
            permission_request_id: PermissionRequestId(self.new_id()),
            cli_request_id: request_id,
            summary: summary(&tool, &input),
            tool,
            input,
            decision,
        })
    }

    fn tool_request(
        &self,
        environment: Option<&Environment>,
        repo_id: RepoId,
        cwd: &str,
        tool: &str,
        input: &Value,
        reference: Vec<saka_ports::ExtraDir>,
    ) -> ToolRequest {
        let command = (tool == "Bash")
            .then(|| {
                input
                    .get("command")
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .flatten();
        let raw = FILE_TOOLS
            .iter()
            .find(|(name, _)| *name == tool)
            .and_then(|(_, key)| input.get(*key).and_then(Value::as_str));
        let path = match (raw, environment, self.repo_for_paths(repo_id)) {
            (Some(raw), Some(env), Some(repo)) => Some(
                self.ports.env.resolve_path(
                    &EnvRef {
                        environment_id: env.id,
                        repo,
                        branch: env.branch.clone(),
                        root_path: env.root_path.clone(),
                        worktree_path: env.worktree_path.clone(),
                        vars: std::collections::BTreeMap::new(),
                        path_prefix: Vec::new(),
                        secrets: Vec::new(),
                        extra_dirs: env
                            .docs
                            .iter()
                            .map(|d| saka_ports::ExtraDir {
                                path: d.path.clone(),
                                writable: d.mode == saka_core::vocab::DocsMode::Writable,
                            })
                            .chain(reference)
                            .collect(),
                    },
                    cwd,
                    raw,
                ),
            ),
            (Some(raw), _, _) => Some(saka_core::policy::RequestPath {
                path: raw.to_string(),
                inside_worktree: false,
                through_symlink: false,
                in_reference: false,
            }),
            (None, _, _) => None,
        };
        ToolRequest {
            tool: tool.to_string(),
            command,
            path,
        }
    }

    fn repo_for_paths(&self, repo_id: RepoId) -> Option<saka_ports::RepoRef> {
        let repo = self
            .store
            .read(|c| read::load_repo(c, repo_id))
            .ok()
            .flatten()?;
        Some(saka_ports::RepoRef {
            repo_id: repo.id,
            integration_id: repo.integration_id,
            full_name: repo.full_name,
            url: repo.url,
        })
    }
}

fn summary(tool: &str, input: &Value) -> String {
    let detail = if tool == "Bash" {
        input.get("command").and_then(Value::as_str)
    } else {
        FILE_TOOLS
            .iter()
            .find(|(name, _)| *name == tool)
            .and_then(|(_, key)| input.get(*key).and_then(Value::as_str))
    };
    let line = match detail {
        Some(d) => format!("{tool}: {d}"),
        None => tool.to_string(),
    };
    if line.chars().count() <= SUMMARY_MAX {
        line
    } else {
        let cut: String = line.chars().take(SUMMARY_MAX - 1).collect();
        format!("{cut}…")
    }
}
