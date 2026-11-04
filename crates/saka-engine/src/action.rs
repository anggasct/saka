use saka_core::ids::ProjectId;
use saka_core::model::{Draft, PermissionRule, Session, Task};
use saka_store::{ActionRecord, Batch, read};
use serde_json::Value;
use uuid::Uuid;

use crate::docs::DocWriteView;
use crate::views::{PermissionView, ProjectView, SpecView};
use crate::{Cause, Engine, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
}

#[derive(Debug)]
pub enum Produced<'a> {
    Project(&'a ProjectView),
    Reload(&'a crate::reload::ReloadView),
    ConfigCheck(&'a crate::config_check::ConfigCheckView),
    Spec(&'a SpecView, bool),
    Task(&'a Task),
    Draft(&'a Draft),
    Permission(&'a PermissionView),
    Session(&'a Session),
    Rule(&'a PermissionRule),
    Doc(&'a DocWriteView),
    UserLinkAuthorization(&'a crate::accounts::AuthorizationView),
    Authorship(&'a crate::authorship::AuthorshipView),
    Nothing,
}

pub type Respond<'a> = dyn Fn(Produced<'_>) -> Reply + Send + Sync + 'a;

pub struct Action<'a> {
    pub idempotency_key: String,
    pub request_hash: String,
    pub name: String,
    pub target_type: String,
    pub target_id: Option<Uuid>,
    pub reason: Option<String>,
    pub respond: &'a Respond<'a>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredAction {
    pub request_hash: String,
    pub reply: Reply,
}

impl Action<'_> {
    pub(crate) fn record(
        &self,
        engine: &Engine,
        cause: &Cause,
        project: Option<ProjectId>,
        target_id: Option<Uuid>,
        reply: Reply,
    ) -> ActionRecord {
        ActionRecord {
            id: engine.new_id(),
            actor: cause.actor.clone(),
            idempotency_key: self.idempotency_key.clone(),
            request_hash: self.request_hash.clone(),
            action: self.name.clone(),
            target_type: self.target_type.clone(),
            target_id: target_id.or(self.target_id),
            project_id: project,
            reason: self.reason.clone(),
            response_status: reply.status,
            response_body: reply.body,
            created_at: engine.now(),
        }
    }
}

impl Engine {
    pub fn find_action(&self, actor: &str, key: &str) -> Result<Option<StoredAction>> {
        Ok(self
            .store
            .read(|c| read::find_action(c, actor, key))?
            .map(|a| StoredAction {
                request_hash: a.request_hash,
                reply: Reply {
                    status: a.response_status,
                    body: a.response_body,
                },
            }))
    }

    pub fn record_refusal(
        &self,
        action: &Action<'_>,
        cause: &Cause,
        project: Option<ProjectId>,
        reply: Reply,
    ) -> Result<()> {
        let mut batch = Batch::new(self.now());
        batch.action = Some(action.record(self, cause, project, None, reply));
        self.store.commit(&batch)?;
        Ok(())
    }
}
