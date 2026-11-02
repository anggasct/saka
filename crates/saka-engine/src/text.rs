use saka_core::event::Prompt;
use saka_core::ids::{SpecRevisionId, WorkItemId};
use saka_core::model::{Coded, Environment, Task};
use saka_core::rejection::Rejection;
use saka_core::vocab::SecretPurpose;
use saka_spec::{SpecModel, Step};
use saka_store::{SpecRevisionRow, read};
use serde_json::Value;

use crate::packet::{self, EnvironmentFacts, Packet, PacketInput, PacketSpec, PacketStep};
use crate::recipe::Recipe;
use crate::{Engine, EngineError, Result};

const TITLE_MAX: usize = 72;

pub(crate) fn change_title(work: &str) -> String {
    let work = work.trim();
    if work.chars().count() <= TITLE_MAX {
        return work.to_string();
    }
    let cut: String = work.chars().take(TITLE_MAX - 1).collect();
    format!("{}…", cut.trim_end())
}

pub fn task_marker(task_key: &str) -> String {
    format!("<!-- saka-task: {task_key} -->")
}

pub(crate) struct Pinned {
    revision: SpecRevisionRow,
    path: String,
    model: SpecModel,
}

impl Pinned {
    pub(crate) fn step(&self, task: &Task) -> Option<&Step> {
        let id = task.item.step_id.as_deref()?;
        self.model.steps.iter().find(|s| s.id == id)
    }
}

impl Engine {
    pub(crate) fn pinned(&self, task: &Task) -> Result<Pinned> {
        let missing =
            || EngineError::Internal(format!("task {} has no pinned revision", task.item.id));
        let revision_id = task.item.spec_revision_id.ok_or_else(missing)?;
        let spec_id = task.item.spec_id.ok_or_else(missing)?;
        let (revision, spec) = self.store.read(|c| {
            Ok((
                read::load_revision(c, revision_id)?,
                read::load_spec(c, spec_id)?,
            ))
        })?;
        let (revision, spec) = revision.zip(spec).ok_or_else(missing)?;
        let model: SpecModel = serde_json::from_value(revision.model.clone())
            .map_err(|e| EngineError::Internal(format!("revision model: {e}")))?;
        Ok(Pinned {
            revision,
            path: spec.path,
            model,
        })
    }

    pub(crate) fn turn_text(&self, task: &Task, prompt: &Prompt) -> Result<String> {
        match prompt {
            Prompt::Uncommitted {
                dirty,
                commits_ahead,
            } => Ok(format!(
                "Your changes are not committed yet (dirty: {dirty}, commits ahead of base: {commits_ahead}). \
                 Commit all changes now with a Conventional Commits message. Do not push."
            )),
            Prompt::WorkPacket { retry_of } => {
                let retry = retry_of.as_ref().and_then(ci_failure);
                Ok(self.packet_of(task, retry)?.text)
            }
            Prompt::Resume { cause, retry_of } => {
                let text = saka_core::resume::text(*cause);
                Ok(match retry_of.as_ref().and_then(ci_failure) {
                    Some(ci) => format!("{text}\n\n{ci}"),
                    None => text,
                })
            }
            Prompt::Steer { text } => Ok(text.clone()),
        }
    }

    pub(crate) fn packet_of(&self, task: &Task, retry: Option<String>) -> Result<Packet> {
        let pinned = self.pinned(task)?;
        let revision = SpecRevisionId(pinned.revision.id);
        let context = self.store.read(|c| read::context_contents(c, revision))?;
        let environment = task
            .ready_environment()
            .map(|env| self.environment_facts(env))
            .unwrap_or_default();
        Ok(packet::render(&PacketInput {
            step: packet_step(task, &pinned),
            spec: PacketSpec {
                revision_id: revision.to_string(),
                path: pinned.path.clone(),
                source_commit: pinned.revision.source_commit.clone(),
                content: pinned.revision.content.clone(),
            },
            context,
            environment,
            retry,
        }))
    }

    fn environment_facts(&self, env: &Environment) -> EnvironmentFacts {
        let recipe = env
            .recipe
            .as_ref()
            .map(Recipe::from_json)
            .unwrap_or_default();
        let count = recipe.ports_or(self.cfg.env.ports_per_env);
        EnvironmentFacts {
            ports: env
                .port_start
                .filter(|_| count > 0)
                .map(|start| (start, count)),
            toolchains: env
                .toolchains
                .iter()
                .map(|t| (t.tool, t.version.clone()))
                .collect(),
            session_secrets: recipe
                .secrets
                .iter()
                .filter(|(_, purposes)| purposes.contains(&SecretPurpose::Session))
                .map(|(name, _)| name.clone())
                .collect(),
        }
    }

    pub fn work_packet(&self, id: WorkItemId) -> Result<(Task, Packet)> {
        let task = self
            .store
            .read(|c| read::load_task(c, id))?
            .ok_or(Rejection::NotFound)?;
        if task.item.spec_revision_id.is_none() {
            return Err(Rejection::WorkPacketUnavailable {
                reason: "not_a_spec_task",
            }
            .into());
        }
        if task.ready_environment().is_none() {
            return Err(Rejection::WorkPacketUnavailable {
                reason: "environment_not_ready",
            }
            .into());
        }
        let packet = self.packet_of(&task, None)?;
        Ok((task, packet))
    }

    pub(crate) fn change_text(&self, task: &Task) -> Result<(String, String)> {
        let pinned = self.pinned(task)?;
        let step = pinned.step(task);
        let short: String = pinned.revision.source_commit.chars().take(7).collect();
        let acs = step.map(|s| s.ac.join(", ")).unwrap_or_default();
        let commands: String = step
            .map(|s| s.commands.iter().map(|c| format!("- `{c}`\n")).collect())
            .unwrap_or_default();
        let body = format!(
            "{work}\n\n**Spec:** `{path}` @ `{short}`, step {step_id}\n**Acceptance criteria:** {acs}\n**Done when:**\n{commands}\n{marker}\n",
            work = task.item.work,
            path = pinned.path,
            step_id = task.item.step_id.as_deref().unwrap_or_default(),
            marker = task_marker(&task.item.task_key),
        );
        Ok((change_title(&task.item.work), body))
    }
}

fn packet_step(task: &Task, pinned: &Pinned) -> PacketStep {
    let step = pinned.step(task);
    let criteria = step
        .map(|s| {
            s.ac.iter()
                .filter_map(|id| {
                    pinned
                        .model
                        .acceptance_criteria
                        .iter()
                        .find(|a| &a.id == id)
                })
                .map(|a| (a.id.clone(), a.text.clone()))
                .collect()
        })
        .unwrap_or_default();
    PacketStep {
        task_key: task.item.task_key.clone(),
        work: task.item.work.clone(),
        criteria,
        commands: step.map(|s| s.commands.clone()).unwrap_or_default(),
    }
}

pub(crate) fn ci_failure(retry_of: &Coded) -> Option<String> {
    if retry_of.code != "ci_red" {
        return None;
    }
    if retry_of.params.get("source").and_then(Value::as_str) == Some("saka") {
        return saka_failure(&retry_of.params);
    }
    let head = retry_of
        .params
        .get("head_sha")
        .and_then(Value::as_str)
        .unwrap_or("the head");
    let names: Vec<&str> = match retry_of.params.get("checks") {
        Some(Value::Array(checks)) => checks
            .iter()
            .filter_map(|c| c.as_str().or_else(|| c.get("name").and_then(Value::as_str)))
            .collect(),
        _ => Vec::new(),
    };
    let names = if names.is_empty() {
        "no checks reported".to_string()
    } else {
        names.join(", ")
    };
    Some(format!(
        "CI failed on {head}: {names}. Fix the failures, run the Done when commands again, and commit. Do not push."
    ))
}

fn saka_failure(params: &Value) -> Option<String> {
    let head = params
        .get("head_sha")
        .and_then(Value::as_str)
        .unwrap_or("the head");
    let command = params.get("command").and_then(Value::as_str)?;
    let how = if params.get("timed_out").and_then(Value::as_bool) == Some(true) {
        "timed out".to_string()
    } else {
        match params.get("exit_code").and_then(Value::as_i64) {
            Some(code) => format!("exit code {code}"),
            None => "no exit code".to_string(),
        }
    };
    let tail = params.get("log_tail").and_then(Value::as_str).unwrap_or("");
    Some(format!(
        "Saka ran the Done when commands on {head}: `{command}` failed ({how}). Last output:\n```\n{tail}\n```\nFix the failure, run the Done when commands again, and commit. Do not push."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_are_cut_at_72_characters() {
        assert_eq!(change_title("Add the done command"), "Add the done command");
        let long = "x".repeat(80);
        let title = change_title(&long);
        assert_eq!(title.chars().count(), 72);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn a_saka_gate_retry_quotes_the_failed_command_and_its_output() {
        let retry = Coded {
            code: "ci_red".into(),
            params: serde_json::json!({
                "source": "saka", "head_sha": "abc", "command": "make test",
                "exit_code": 2, "log_tail": "FAIL x_test.go:3", "checks": ["make test"],
            }),
        };
        assert_eq!(
            ci_failure(&retry).as_deref(),
            Some(
                "Saka ran the Done when commands on abc: `make test` failed (exit code 2). Last output:\n```\nFAIL x_test.go:3\n```\nFix the failure, run the Done when commands again, and commit. Do not push."
            )
        );
        let timed = Coded {
            code: "ci_red".into(),
            params: serde_json::json!({ "source": "saka", "head_sha": "abc", "command": "make e2e", "timed_out": true, "log_tail": "" }),
        };
        assert!(
            ci_failure(&timed)
                .unwrap()
                .contains("`make e2e` failed (timed out)")
        );
    }

    #[test]
    fn ci_retries_name_the_failing_checks() {
        let retry = Coded {
            code: "ci_red".into(),
            params: serde_json::json!({ "checks": ["test", "lint"], "head_sha": "abc" }),
        };
        assert_eq!(
            ci_failure(&retry).as_deref(),
            Some(
                "CI failed on abc: test, lint. Fix the failures, run the Done when commands again, and commit. Do not push."
            )
        );
        let other = Coded {
            code: "session_lost".into(),
            params: serde_json::json!({}),
        };
        assert_eq!(ci_failure(&other), None);
    }
}
