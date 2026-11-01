use serde_json::Value;

use crate::event::Event;
use crate::model::{PermissionRequest, PermissionRule};
use crate::rejection::Rejection;
use crate::time::Timestamp;

pub const REDACTION_MARKER: &str = "«secret:";

pub fn matches(rule: &PermissionRule, tool: &str, command: Option<&str>) -> bool {
    rule.revoked_at.is_none()
        && rule.tool == tool
        && (tool != "Bash" || rule.command.as_deref() == command)
}

pub fn remembered(request: &PermissionRequest) -> Result<(String, Option<String>), Rejection> {
    if request.tool != "Bash" {
        return Ok((request.tool.clone(), None));
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
    Ok((request.tool.clone(), Some(command.to_string())))
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
            tool,
            command,
            permission_request_id,
            created_by,
            ..
        } => Some(PermissionRule {
            id: *rule_id,
            project_id: *project_id,
            tool: tool.clone(),
            command: command.clone(),
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
