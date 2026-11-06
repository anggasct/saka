use saka_core::event::Event;
use saka_core::hook::{self, InboxRef, NamedRef, SpecRef, TaskRef};
use saka_core::ids::InboxItemId;
use saka_core::inbox::{self, Source};
use saka_core::rejection::Rejection;
use saka_core::time::Timestamp;
use saka_core::vocab::HookEvent;
use saka_core::vocab::InboxState;
use saka_store::Batch;
use saka_store::inbox::{
    InboxDismissal, InboxFilter, InboxItem, InboxSummary, InboxSync, OpenedItem,
};
use saka_store::list::Page;

use crate::action::{Action, Produced};
use crate::commands::reason;
use crate::views::{Paged, paged, probe};
use crate::{Cause, Engine, EngineError, Result};

const DAY_MS: i64 = 86_400_000;

impl Engine {
    pub(crate) fn inbox_syncs(&self, batch: &Batch) -> Vec<InboxSync> {
        let ceilings = &self.cfg.loop_cfg.autonomy;
        let sources = batch
            .tasks
            .iter()
            .map(Source::Task)
            .chain(batch.drafts.iter().map(Source::Draft))
            .chain(batch.specs.iter().map(Source::Spec));
        sources
            .map(|source| InboxSync {
                scope: inbox::scope(source),
                desired: inbox::desired(source, ceilings),
            })
            .collect()
    }

    pub(crate) fn plan_inbox_hooks(&self, opened: &[OpenedItem], now: Timestamp) -> Option<Batch> {
        if !self
            .cfg
            .hooks
            .iter()
            .any(|h| h.on == HookEvent::InboxItemOpened)
        {
            return None;
        }
        let mut extra = Batch::new(now);
        let cause = self.cause("system:inbox");
        for item in opened {
            let loaded = self
                .store
                .read(|c| saka_store::read::load_project(c, item.project_id))
                .ok()
                .flatten();
            let Some(project) = loaded else {
                continue;
            };
            let ctx = hook::inbox_context(
                InboxRef {
                    id: item.id,
                    kind: item.kind,
                    subject: item.subject,
                },
                NamedRef {
                    id: project.id.0,
                    name: project.name.clone(),
                },
                item.work_item_id
                    .zip(item.task_key.clone())
                    .map(|(id, key)| TaskRef { id, key }),
                item.spec_id
                    .zip(item.spec_path.clone())
                    .map(|(id, path)| SpecRef { id, path }),
                now,
            );
            self.queue_hooks(&mut extra, &ctx, &project.hook_skip, item.id.0, &cause, now);
        }
        (!extra.events.is_empty()).then_some(extra)
    }

    pub fn inbox(&self, filter: &InboxFilter, page: &Page) -> Result<Paged<InboxItem>> {
        let rows = self
            .store
            .read(|c| saka_store::inbox::list(c, filter, &probe(page)))?;
        let paged = paged(rows, page, |(_, key)| *key);
        Ok(Paged {
            items: paged.items.into_iter().map(|(item, _)| item).collect(),
            next: paged.next,
        })
    }

    pub fn inbox_item(&self, id: InboxItemId) -> Result<Option<InboxItem>> {
        Ok(self.store.read(|c| saka_store::inbox::load(c, id))?)
    }

    pub fn inbox_summary(&self) -> Result<InboxSummary> {
        let now = self.now();
        let day_start = Timestamp(now.0 - now.0.rem_euclid(DAY_MS));
        Ok(self
            .store
            .read(|c| saka_store::inbox::summary(c, day_start))?)
    }

    pub fn dismiss_inbox_item_with(
        &self,
        id: InboxItemId,
        why: &str,
        cause: &Cause,
        action: Option<&Action<'_>>,
    ) -> Result<InboxItem> {
        let why = reason(why)?;
        let _guard = self.locked()?;
        let now = self.now();
        let mut item = self
            .inbox_item(id)?
            .ok_or(EngineError::Rejected(Rejection::InboxItemNotFound))?;
        if !item.dismissible() {
            return Err(Rejection::InboxItemNotDismissible {
                subject: item.subject.as_str(),
            }
            .into());
        }
        if item.state != InboxState::Open {
            return Err(Rejection::InboxItemClosed {
                state: item.state.as_str(),
            }
            .into());
        }
        let event = Event::InboxItemDismissed {
            inbox_item_id: id,
            project_id: item.project_id,
            subject: item.subject,
            source_id: item.source_id.clone(),
            reason: why,
        };
        let mut batch = Batch::new(now);
        self.push_event(&mut batch, &event, Some(item.project_id), cause, now);
        item.state = InboxState::Dismissed;
        item.closed_at = Some(now);
        if let Some(a) = action {
            let reply = (a.respond)(Produced::Inbox(&item));
            batch.action = Some(a.record(self, cause, Some(item.project_id), Some(id.0), reply));
        }
        batch.inbox_dismissals.push(InboxDismissal { id });
        self.commit(&batch)?;
        Ok(item)
    }
}
