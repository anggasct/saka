use saka_core::event::Prompt;
use saka_core::model::{Coded, Task};
use saka_spec::{SpecModel, Step};
use saka_store::{SpecRevisionRow, read};
use serde_json::Value;

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

struct Pinned {
    revision: SpecRevisionRow,
    path: String,
    model: SpecModel,
}

impl Pinned {
    fn step(&self, task: &Task) -> Option<&Step> {
        let id = task.item.step_id.as_deref()?;
        self.model.steps.iter().find(|s| s.id == id)
    }
}

impl Engine {
    fn pinned(&self, task: &Task) -> Result<Pinned> {
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
                let pinned = self.pinned(task)?;
                let mut text = work_packet(task, &pinned);
                if let Some(ci) = retry_of.as_ref().and_then(ci_failure) {
                    text = format!("{ci}\n\n{text}");
                }
                Ok(text)
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

fn work_packet(task: &Task, pinned: &Pinned) -> String {
    let step = pinned.step(task);
    let criteria: String = step
        .map(|s| {
            s.ac.iter()
                .filter_map(|id| {
                    pinned
                        .model
                        .acceptance_criteria
                        .iter()
                        .find(|a| &a.id == id)
                })
                .map(|a| format!("- {}: {}\n", a.id, a.text))
                .collect()
        })
        .unwrap_or_default();
    let commands: String = step
        .map(|s| s.commands.iter().map(|c| format!("- `{c}`\n")).collect())
        .unwrap_or_default();
    let content = String::from_utf8_lossy(&pinned.revision.content);
    format!(
        "You are implementing one step of an approved spec in this git repository.\n\n\
         Task: {key} — {work}\n\
         Acceptance criteria for this step:\n{criteria}\
         Done when (run these and make them pass before you finish):\n{commands}\n\
         Rules:\n\
         - Work only inside this repository.\n\
         - Commit all your changes with a Conventional Commits message before you finish.\n\
         - Do not push. Saka pushes the branch and opens the pull request.\n\n\
         The full spec (pinned revision {revision}) follows.\n---\n{content}",
        key = task.item.task_key,
        work = task.item.work,
        revision = pinned.revision.id,
    )
}

fn ci_failure(retry_of: &Coded) -> Option<String> {
    if retry_of.code != "ci_red" {
        return None;
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
