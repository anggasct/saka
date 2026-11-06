use serde_json::Value;

use crate::Ctx;
use crate::event::Event;
use crate::ids::{PermissionRuleId, ProjectId};
use crate::model::{PermissionRequest, PermissionRule};
use crate::policy::{self, Effect, ToolRequest};
use crate::rejection::Rejection;
use crate::time::Timestamp;
use crate::vocab::MatchKind;

pub const REDACTION_MARKER: &str = "«secret:";

const TOOL_MAX: usize = 100;
const NOTE_MAX: usize = 500;
const COMMAND_MAX: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRule {
    pub effect: Effect,
    pub tool: String,
    pub match_kind: MatchKind,
    pub command: Option<String>,
    pub path: Option<String>,
    pub note: Option<String>,
}

pub fn matches(rule: &PermissionRule, request: &ToolRequest, glob_allow: bool) -> bool {
    if rule.revoked_at.is_some() || rule.tool != request.tool {
        return false;
    }
    let glob = rule.match_kind == MatchKind::Glob;
    if glob && rule.effect == Effect::Allow && !glob_allow {
        return false;
    }
    let command_ok = match (&rule.command, request.command.as_deref()) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(pattern), Some(command)) if glob => {
            policy::glob_matches(pattern, false, command.trim())
        }
        (Some(exact), Some(command)) => exact == command,
    };
    let path_ok = match (&rule.path, &request.path) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(pattern), Some(p)) => {
            p.inside_worktree
                && !(rule.effect == Effect::Allow && p.through_symlink)
                && if glob {
                    policy::glob_matches(pattern, true, &p.path)
                } else {
                    *pattern == p.path
                }
        }
    };
    command_ok && path_ok
}

pub fn add(
    project_id: ProjectId,
    new: NewRule,
    active: &[PermissionRule],
    created_by: &str,
    ctx: &mut Ctx<'_>,
) -> Result<Event, Rejection> {
    let NewRule {
        effect,
        tool,
        match_kind,
        command,
        path,
        note,
    } = new;
    let tool = tool.trim().to_string();
    if tool.is_empty() {
        return Err(invalid("tool", "required"));
    }
    if tool.chars().count() > TOOL_MAX {
        return Err(invalid("tool", "length"));
    }
    if note.as_ref().is_some_and(|n| n.chars().count() > NOTE_MAX) {
        return Err(invalid("note", "length"));
    }
    let command = command.map(|c| c.trim().to_string());
    if command
        .as_ref()
        .is_some_and(|c| c.chars().count() > COMMAND_MAX)
    {
        return Err(invalid("command", "length"));
    }
    if command
        .as_ref()
        .is_some_and(|c| c.contains(REDACTION_MARKER))
    {
        return Err(invalid("command", "contains_secret"));
    }
    let bash = tool == "Bash";
    match (&command, bash) {
        (None, true) => return Err(rule_invalid("command", "required")),
        (Some(c), true) if c.is_empty() => return Err(rule_invalid("command", "required")),
        (Some(_), false) => return Err(rule_invalid("command", "bash_only")),
        _ => {}
    }
    if bash && path.is_some() {
        return Err(rule_invalid("path", "not_for_bash"));
    }
    if path.as_ref().is_some_and(|p| p.trim().is_empty()) {
        return Err(rule_invalid("path", "required"));
    }
    if match_kind == MatchKind::Glob {
        if command
            .as_deref()
            .is_some_and(|c| !policy::glob_compiles(c, false))
        {
            return Err(rule_invalid("command", "glob"));
        }
        if path
            .as_deref()
            .is_some_and(|p| !policy::glob_compiles(p, true))
        {
            return Err(rule_invalid("path", "glob"));
        }
        if effect == Effect::Allow
            && bash
            && command
                .as_deref()
                .is_some_and(|c| c.chars().all(|c| c == '*' || c == '?'))
        {
            return Err(Rejection::RuleTooBroad);
        }
    }
    let same = active.iter().find(|r| {
        r.revoked_at.is_none()
            && r.project_id == project_id
            && r.effect == effect
            && r.tool == tool
            && r.match_kind == match_kind
            && r.command == command
            && r.path == path
    });
    if let Some(r) = same {
        return Err(Rejection::RuleDuplicate {
            rule_id: r.id.to_string(),
        });
    }
    Ok(Event::PermissionRuleAdded {
        rule_id: PermissionRuleId::new(ctx.ids),
        project_id,
        owner: None,
        effect,
        tool,
        match_kind,
        command,
        path,
        note,
        permission_request_id: None,
        created_by: created_by.to_string(),
    })
}

fn invalid(field: &'static str, code: &'static str) -> Rejection {
    Rejection::Invalid { field, code }
}

fn rule_invalid(field: &'static str, reason: &'static str) -> Rejection {
    Rejection::RuleInvalid { field, reason }
}

pub fn remembered(
    request: &PermissionRequest,
    allowed_parts: &[String],
) -> Result<Vec<(String, Option<String>)>, Rejection> {
    if request.tool != "Bash" {
        return Ok(vec![(request.tool.clone(), None)]);
    }
    let command =
        request
            .input
            .get("command")
            .and_then(Value::as_str)
            .ok_or(Rejection::Invalid {
                field: "remember",
                code: "no_command",
            })?;
    if command.contains(REDACTION_MARKER) {
        return Err(Rejection::Invalid {
            field: "remember",
            code: "contains_secret",
        });
    }
    let commands = match crate::shell::split(command) {
        Some(parts) => {
            let mut wanted: Vec<String> = Vec::new();
            for part in parts {
                if !allowed_parts.contains(&part) && !wanted.contains(&part) {
                    wanted.push(part);
                }
            }
            wanted
        }
        None => vec![command.to_string()],
    };
    Ok(commands
        .into_iter()
        .map(|c| (request.tool.clone(), Some(c)))
        .collect())
}

pub fn revoke(rule: &PermissionRule, reason: String) -> Result<Vec<Event>, Rejection> {
    if rule.revoked_at.is_some() {
        return Err(Rejection::InvalidState {
            state: "revoked".to_string(),
        });
    }
    Ok(vec![Event::PermissionRuleRevoked {
        rule_id: rule.id,
        project_id: rule.project_id,
        reason,
    }])
}

pub fn evolve(
    rule: Option<PermissionRule>,
    event: &Event,
    now: Timestamp,
) -> Option<PermissionRule> {
    match event {
        Event::PermissionRuleAdded {
            rule_id,
            project_id,
            effect,
            tool,
            match_kind,
            command,
            path,
            note,
            permission_request_id,
            created_by,
            ..
        } => Some(PermissionRule {
            id: *rule_id,
            project_id: *project_id,
            effect: *effect,
            tool: tool.clone(),
            match_kind: *match_kind,
            command: command.clone(),
            path: path.clone(),
            note: note.clone(),
            permission_request_id: *permission_request_id,
            created_by: created_by.clone(),
            created_at: now,
            revoked_at: None,
            revoke_reason: None,
        }),
        Event::PermissionRuleRevoked {
            rule_id, reason, ..
        } => rule.filter(|r| r.id == *rule_id).map(|mut r| {
            r.revoked_at = Some(now);
            r.revoke_reason = Some(reason.clone());
            r
        }),
        _ => rule,
    }
}
