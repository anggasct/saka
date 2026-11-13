use saka_core::ids::{PermissionRequestId, ProjectId};
use saka_core::model::PermissionRequest;
use saka_core::policy::{Decision, Effect, RequestPath, ToolRequest};
use saka_core::rejection::Rejection;
use saka_core::rule::{self, NewRule};
use saka_ports::IdsSource;
use saka_store::list;
use saka_store::read;
use serde_json::Value;

use crate::runner::FILE_TOOLS;
use crate::{Engine, EngineError, Result};

pub const PREVIEW_SCAN: u32 = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewChange {
    pub permission_request_id: PermissionRequestId,
    pub tool: String,
    pub command: Option<String>,
    pub before: Effect,
    pub after: Effect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewView {
    pub scanned: usize,
    pub changed: Vec<PreviewChange>,
}

fn request_of(p: &PermissionRequest) -> ToolRequest {
    let command = (p.tool == "Bash")
        .then(|| p.input.get("command").and_then(Value::as_str))
        .flatten()
        .map(String::from);
    let path = FILE_TOOLS
        .iter()
        .find(|(name, _)| *name == p.tool)
        .and_then(|(_, key)| p.input.get(*key).and_then(Value::as_str))
        .map(|path| RequestPath {
            path: path.to_string(),
            inside_worktree: true,
            through_symlink: false,
            in_reference: false,
        });
    ToolRequest {
        tool: p.tool.clone(),
        command,
        path,
    }
}

impl Engine {
    pub fn evaluate_permission(
        &self,
        project: ProjectId,
        request: &ToolRequest,
    ) -> Result<Decision> {
        let rules = self.project_rules(project)?;
        Ok(self.cfg.policy.evaluate_with(request, &rules))
    }

    pub fn preview_rule(&self, project: ProjectId, new: NewRule) -> Result<PreviewView> {
        let rules = self.project_rules(project)?;
        let mut ids = IdsSource(&*self.ports.ids);
        let mut ctx = saka_core::Ctx {
            now: self.now(),
            ids: &mut ids,
        };
        let event = rule::add(project, new, &rules, "preview", &mut ctx)?;
        let candidate = rule::evolve(None, &event, self.now())
            .ok_or_else(|| EngineError::Internal("preview dropped the rule".into()))?;
        let mut with = rules.clone();
        with.push(candidate);
        let recent = self
            .store
            .read(|c| list::recent_task_permissions(c, project, PREVIEW_SCAN))?;
        let changed = recent
            .iter()
            .filter_map(|p| {
                let request = request_of(p);
                let before = self.cfg.policy.evaluate_with(&request, &rules).effect;
                let after = self.cfg.policy.evaluate_with(&request, &with).effect;
                (before != after).then(|| PreviewChange {
                    permission_request_id: p.id,
                    tool: p.tool.clone(),
                    command: request.command,
                    before,
                    after,
                })
            })
            .collect();
        Ok(PreviewView {
            scanned: recent.len(),
            changed,
        })
    }

    fn project_rules(&self, project: ProjectId) -> Result<Vec<saka_core::model::PermissionRule>> {
        if self
            .store
            .read(|c| read::load_project(c, project))?
            .is_none()
        {
            return Err(EngineError::Rejected(Rejection::NotFound));
        }
        Ok(self
            .store
            .read(|c| read::active_permission_rules(c, project))?)
    }
}
