//! Operator actions from the control API. Each one is stored as an `action_request` with
//! the response it got, in the same transaction as the events it caused, so repeating the
//! request with the same idempotency key replays the response.

use saka_core::ids::ProjectId;
use saka_core::model::Task;
use saka_store::{ActionRecord, Batch, read};
use serde_json::Value;
use uuid::Uuid;

use crate::views::{PermissionView, ProjectView, SpecView};
use crate::{Cause, Engine, Result};

/// A response: status and JSON body.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
}

/// What a command produced, for the response stored with it.
#[derive(Debug)]
pub enum Produced<'a> {
    Project(&'a ProjectView),
    /// `true` when the spec is new.
    Spec(&'a SpecView, bool),
    Task(&'a Task),
    Permission(&'a PermissionView),
}

/// The response builder of an action.
pub type Respond<'a> = dyn Fn(Produced<'_>) -> Reply + Send + Sync + 'a;

/// One API mutation.
pub struct Action<'a> {
    pub idempotency_key: String,
    /// `SHA-256(method ‖ path ‖ body)`.
    pub request_hash: String,
    /// For example `spec.approve`.
    pub name: String,
    pub target_type: String,
    pub target_id: Option<Uuid>,
    pub reason: Option<String>,
    pub respond: &'a Respond<'a>,
}

/// A response stored for an idempotency key.
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
    /// The response stored for `key`, if the actor used it before.
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

    /// Stores the response of an action the engine refused (nothing else is written).
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
