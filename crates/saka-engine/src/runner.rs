use saka_core::ids::PermissionRequestId;
use saka_core::model::Task;
use saka_core::policy::{Effect, ToolRequest};
use saka_core::task::TaskInput;
use saka_ports::{EnvRef, RunnerEvent, RunnerEventKind};
use saka_store::read;
use serde_json::Value;

use crate::{Engine, Extra, Mode, Result, timeline};

const FILE_TOOLS: &[(&str, &str)] = &[
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

impl Engine {
    pub fn on_runner_event(&self, event: RunnerEvent) -> Result<()> {
        let Some(work_item) = self
            .store
            .read(|c| read::session_work_item(c, event.session_id))?
        else {
            return Ok(());
        };
        let task = self.load_task(work_item)?;
        let Some(session) = task.session.as_ref().filter(|s| s.id == event.session_id) else {
            return Ok(());
        };
        let replayed =
            event.offset <= session.last_offset && !(event.offset == 0 && session.last_offset == 0);
        if replayed {
            return Ok(());
        }
        let live = session.state.is_live();
        let mut permission = None;
        let mut inputs: Vec<TaskInput> = match &event.kind {
            RunnerEventKind::SessionInit {
                cli_session_id,
                model,
            } => vec![TaskInput::SessionInitialized {
                cli_session_id: cli_session_id.clone(),
                model: model.clone(),
            }],
            RunnerEventKind::PermissionRequest {
                request_id,
                tool,
                input,
            } if live => {
                let next = self.permission_request(
                    &task,
                    request_id.clone(),
                    tool.clone(),
                    input.clone(),
                )?;
                if let TaskInput::PermissionRequested {
                    permission_request_id,
                    summary,
                    decision,
                    ..
                } = &next
                {
                    let state = match decision.effect {
                        Effect::Allow => "allowed",
                        Effect::Deny => "denied",
                        Effect::Ask => "pending",
                    };
                    permission = Some((*permission_request_id, summary.clone(), state));
                }
                vec![next]
            }
            RunnerEventKind::PermissionDelivered {
                request_id,
                behavior,
            } => {
                let found = task
                    .permissions
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
                    .map(|p| TaskInput::PermissionDelivered {
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
            } => vec![TaskInput::TurnEnded {
                turn: *turn,
                status: status.clone(),
                cost_usd_micros: *cost_usd_micros,
                input_tokens: *input_tokens,
                output_tokens: *output_tokens,
            }],
            RunnerEventKind::CliExited { code, signal } => vec![TaskInput::CliExited {
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
            work_item,
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
        self.decide(
            work_item,
            first,
            Mode::Fact,
            &cause,
            Extra {
                activity: row,
                offset: Some((event.session_id, event.offset)),
                ..Extra::default()
            },
        )?;
        for input in inputs {
            self.decide(work_item, Some(input), Mode::Fact, &cause, Extra::default())?;
        }
        Ok(())
    }

    fn permission_request(
        &self,
        task: &Task,
        request_id: String,
        tool: String,
        input: Value,
    ) -> Result<TaskInput> {
        let request = self.tool_request(task, &tool, &input);
        let rules = self
            .store
            .read(|c| read::active_permission_rules(c, task.item.project_id))?;
        let decision = self.cfg.policy.evaluate_with(&request, &rules);
        Ok(TaskInput::PermissionRequested {
            permission_request_id: PermissionRequestId(self.new_id()),
            cli_request_id: request_id,
            summary: summary(&tool, &input),
            tool,
            input,
            decision,
        })
    }

    fn tool_request(&self, task: &Task, tool: &str, input: &Value) -> ToolRequest {
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
        let path = match (raw, task.environment.as_ref(), self.repo_for_paths(task)) {
            (Some(raw), Some(env), Some(repo)) => Some(self.ports.env.resolve_path(
                &EnvRef {
                    environment_id: env.id,
                    repo,
                    branch: env.branch.clone(),
                    root_path: env.root_path.clone(),
                    worktree_path: env.worktree_path.clone(),
                    vars: std::collections::BTreeMap::new(),
                    path_prefix: Vec::new(),
                    secrets: Vec::new(),
                },
                raw,
            )),
            (Some(raw), _, _) => Some(saka_core::policy::RequestPath {
                path: raw.to_string(),
                inside_worktree: false,
            }),
            (None, _, _) => None,
        };
        ToolRequest {
            tool: tool.to_string(),
            command,
            path,
        }
    }

    fn repo_for_paths(&self, task: &Task) -> Option<saka_ports::RepoRef> {
        let repo = self
            .store
            .read(|c| read::load_repo(c, task.item.repo_id))
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
