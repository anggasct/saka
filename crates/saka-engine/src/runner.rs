//! Runner events → facts for the core. Permission requests are evaluated against the
//! policy here, after the tool's path is resolved against the worktree.

use saka_core::ids::PermissionRequestId;
use saka_core::model::Task;
use saka_core::policy::ToolRequest;
use saka_core::task::TaskInput;
use saka_ports::{EnvRef, RunnerEvent, RunnerEventKind};
use saka_store::read;
use serde_json::Value;

use crate::{Engine, Extra, Mode, Result};

/// Tools whose input names a file.
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
    /// Applies one runner event. Events of a session that is no longer the task's live
    /// session, and repeated events, change nothing.
    pub fn on_runner_event(&self, event: RunnerEvent) -> Result<()> {
        let Some(work_item) = self
            .store
            .read(|c| read::session_work_item(c, event.session_id))?
        else {
            return Ok(());
        };
        let task = self.load_task(work_item)?;
        if task.live_session().map(|s| s.id) != Some(event.session_id) {
            return Ok(());
        }
        let input = match event.kind {
            RunnerEventKind::SessionInit {
                cli_session_id,
                model,
            } => Some(TaskInput::SessionInitialized {
                cli_session_id,
                model,
            }),
            RunnerEventKind::PermissionRequest {
                request_id,
                tool,
                input,
            } => Some(self.permission_request(&task, request_id, tool, input)),
            RunnerEventKind::PermissionDelivered { request_id, .. } => task
                .permissions
                .iter()
                .find(|p| p.cli_request_id == request_id)
                .map(|p| TaskInput::PermissionDelivered {
                    permission_request_id: p.id,
                }),
            RunnerEventKind::TurnEnded {
                turn,
                status,
                cost_usd_micros,
                input_tokens,
                output_tokens,
                ..
            } => Some(TaskInput::TurnEnded {
                turn,
                status,
                cost_usd_micros,
                input_tokens,
                output_tokens,
            }),
            RunnerEventKind::CliExited { code, signal } => {
                Some(TaskInput::CliExited { code, signal })
            }
            RunnerEventKind::RunnerError { code, .. } => {
                tracing::warn!(session_id = %event.session_id, code = %code, "runner error");
                None
            }
            RunnerEventKind::CliStarted { .. }
            | RunnerEventKind::Activity { .. }
            | RunnerEventKind::RateLimit { .. } => None,
        };
        if let Some(input) = input {
            let cause = self.cause("system:runner");
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
    ) -> TaskInput {
        let request = self.tool_request(task, &tool, &input);
        let decision = self.cfg.policy.evaluate(&request);
        TaskInput::PermissionRequested {
            permission_request_id: PermissionRequestId(self.new_id()),
            cli_request_id: request_id,
            summary: summary(&tool, &input),
            tool,
            input,
            decision,
        }
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
                },
                raw,
            )),
            // Without a worktree nothing is inside it.
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

/// A short line for people: the command for `Bash`, the path for file tools.
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
