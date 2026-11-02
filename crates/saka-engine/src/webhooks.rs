use saka_core::job::{Job, JobKind};
use saka_core::time::Timestamp;
use saka_ports::{PortResult, WebhookDelivery, WebhookTarget};
use saka_store::{Batch, WebhookDeliveryRow, read};
use serde_json::json;

use crate::{Engine, Result};

pub const WEBHOOK_RETENTION_MS: i64 = 7 * 24 * 3_600_000;
const PRUNE_PERIOD_MS: i64 = 3_600_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookOutcome {
    Refreshed,
    Ignored,
    Duplicate,
}

impl WebhookOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            WebhookOutcome::Refreshed => "refreshed",
            WebhookOutcome::Ignored => "ignored",
            WebhookOutcome::Duplicate => "duplicate",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedWebhook {
    pub delivery_id: String,
    pub outcome: WebhookOutcome,
    pub change_requests: u32,
}

pub(crate) fn prune_job(now: Timestamp) -> Job {
    Job::new(
        JobKind::ScmWebhookPrune,
        now.0.div_euclid(PRUNE_PERIOD_MS),
        json!({}),
        now,
    )
}

impl Engine {
    pub fn accept_webhook(
        &self,
        integration_id: &str,
        delivery: &WebhookDelivery,
    ) -> Result<AcceptedWebhook> {
        let _guard = self.locked()?;
        let now = self.now();
        let accepted = |outcome, change_requests| AcceptedWebhook {
            delivery_id: delivery.delivery_id.clone(),
            outcome,
            change_requests,
        };
        let (seen, repo) = self.store.read(|c| {
            let seen = read::webhook_delivery_seen(c, integration_id, &delivery.delivery_id)?;
            let repo = match &delivery.repo_full_name {
                Some(name) => read::repo_by_name(c, integration_id, name)?,
                None => None,
            };
            Ok((seen, repo))
        })?;
        if let Some(n) = seen {
            self.counters.webhook(
                integration_id,
                &delivery.event,
                WebhookOutcome::Duplicate.as_str(),
            );
            return Ok(accepted(WebhookOutcome::Duplicate, n));
        }
        let candidates = match &repo {
            Some(r) if !delivery.targets.is_empty() => {
                self.store.read(|c| read::hook_candidates(c, r.id))?
            }
            _ => Vec::new(),
        };
        let hit: Vec<_> = candidates
            .into_iter()
            .filter(|c| {
                delivery.targets.iter().any(|t| match t {
                    WebhookTarget::Change { number } => *number == c.number,
                    WebhookTarget::Head { sha } => sha.eq_ignore_ascii_case(&c.head_sha),
                })
            })
            .collect();
        let row_id = self.new_id();
        let cause = self.cause("system:webhook");
        let mut batch = Batch::new(now);
        for c in &hit {
            let job = Job::new(
                JobKind::ScmPollChangeRequest,
                format!("{}:hook:{row_id}", c.change_request_id),
                json!({
                    "work_item_id": c.work_item_id,
                    "change_request_id": c.change_request_id,
                    "repo_id": c.repo_id,
                    "number": c.number,
                }),
                now,
            );
            self.push_job(&mut batch, job, Some(c.project_id), &cause);
        }
        let refreshed = u32::try_from(hit.len()).unwrap_or(u32::MAX);
        batch.webhook_deliveries.push(WebhookDeliveryRow {
            id: row_id,
            integration_id: integration_id.to_string(),
            delivery_id: delivery.delivery_id.clone(),
            event: delivery.event.clone(),
            action: delivery.action.clone(),
            repo_id: repo.map(|r| r.id),
            refreshed,
            received_at: now,
        });
        self.commit(&batch)?;
        let outcome = if refreshed > 0 {
            WebhookOutcome::Refreshed
        } else {
            WebhookOutcome::Ignored
        };
        self.counters
            .webhook(integration_id, &delivery.event, outcome.as_str());
        Ok(accepted(outcome, refreshed))
    }

    pub fn webhook_configured(&self, integration_id: &str) -> PortResult<bool> {
        self.ports.scm.webhook_configured(integration_id)
    }

    pub fn parse_webhook(
        &self,
        integration_id: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> PortResult<WebhookDelivery> {
        self.ports.scm.parse_webhook(integration_id, headers, body)
    }

    pub fn webhook_rejected(&self, integration_id: &str) {
        self.counters.webhook(integration_id, "unknown", "rejected");
    }

    pub(crate) fn prune_webhooks(&self) -> Result<()> {
        let now = self.now();
        let mut batch = Batch::new(now);
        batch.prune_webhook_deliveries_before = Some(Timestamp(now.0 - WEBHOOK_RETENTION_MS));
        self.store.commit(&batch)?;
        Ok(())
    }
}
