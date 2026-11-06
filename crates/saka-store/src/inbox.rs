use std::collections::BTreeMap;

use rusqlite::{Connection, Row, ToSql, Transaction, params};
use saka_core::ids::{InboxItemId, ProjectId, SpecId, WorkItemId};
use saka_core::inbox::{InboxAction, InboxDraft, Scope};
use saka_core::time::Timestamp;
use saka_core::vocab::{InboxKind, InboxState, InboxSubject};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::codec::{json, opt_uuid, uuid, value_json, vocab};
use crate::error::Result;
use crate::list::{Key, Page};
use crate::read::{opt_ts, ts};

#[derive(Debug, Clone, PartialEq)]
pub struct InboxSync {
    pub scope: Scope,
    pub desired: Vec<InboxDraft>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenedItem {
    pub id: InboxItemId,
    pub project_id: ProjectId,
    pub kind: InboxKind,
    pub subject: InboxSubject,
    pub work_item_id: Option<WorkItemId>,
    pub task_key: Option<String>,
    pub spec_id: Option<SpecId>,
    pub spec_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboxDismissal {
    pub id: InboxItemId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboxItem {
    pub id: InboxItemId,
    pub project_id: ProjectId,
    pub project_name: String,
    pub kind: InboxKind,
    pub subject: InboxSubject,
    pub source_id: String,
    pub state: InboxState,
    pub work_item_id: Option<WorkItemId>,
    pub spec_id: Option<SpecId>,
    pub task_key: Option<String>,
    pub stage: Option<String>,
    pub spec_slug: Option<String>,
    pub spec_title: Option<String>,
    pub spec_path: Option<String>,
    pub draft_path: Option<String>,
    pub params: Value,
    pub action: InboxAction,
    pub opened_at: Timestamp,
    pub closed_at: Option<Timestamp>,
}

impl InboxItem {
    pub fn dismissible(&self) -> bool {
        saka_core::inbox::dismissible(self.kind, self.subject)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct InboxFilter {
    pub project: Option<ProjectId>,
    pub kind: Option<InboxKind>,
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxSummary {
    pub open: i64,
    pub by_kind: BTreeMap<InboxKind, i64>,
    pub running: i64,
    pub merged_today: i64,
}

const SELECT: &str = "SELECT i.*, p.name AS project_name, w.task_key, w.stage AS stage,
    s.slug AS spec_slug, s.title AS spec_title, s.path AS spec_path, d.path AS draft_path
  FROM inbox_item i
  JOIN project p ON p.id = i.project_id
  LEFT JOIN work_item w ON w.id = i.work_item_id
  LEFT JOIN spec s ON s.id = i.spec_id
  LEFT JOIN permission_request pr ON i.subject = 'permission' AND i.work_item_id IS NULL AND pr.id = i.source_id
  LEFT JOIN spec_draft d ON d.id = pr.spec_draft_id";

fn item_row(row: &Row<'_>) -> Result<InboxItem> {
    let action: Value = json(row, "action")?;
    Ok(InboxItem {
        id: InboxItemId(uuid(row, "id")?),
        project_id: ProjectId(uuid(row, "project_id")?),
        project_name: row.get("project_name")?,
        kind: vocab(row, "kind")?,
        subject: vocab(row, "subject")?,
        source_id: row.get("source_id")?,
        state: vocab(row, "state")?,
        work_item_id: opt_uuid(row, "work_item_id")?.map(WorkItemId),
        spec_id: opt_uuid(row, "spec_id")?.map(SpecId),
        task_key: row.get("task_key")?,
        stage: row.get("stage")?,
        spec_slug: row.get("spec_slug")?,
        spec_title: row.get("spec_title")?,
        spec_path: row.get("spec_path")?,
        draft_path: row.get("draft_path")?,
        params: json(row, "params")?,
        action: InboxAction {
            kind: serde_json::from_value(action.get("kind").cloned().unwrap_or(Value::Null))?,
            params: action.get("params").cloned().unwrap_or(Value::Null),
        },
        opened_at: ts(row, "opened_at")?,
        closed_at: opt_ts(row, "closed_at")?,
    })
}

pub fn load(conn: &Connection, id: InboxItemId) -> Result<Option<InboxItem>> {
    let mut stmt = conn.prepare(&format!("{SELECT} WHERE i.id = ?1"))?;
    let mut rows = stmt.query([id.to_string()])?;
    match rows.next()? {
        Some(row) => Ok(Some(item_row(row)?)),
        None => Ok(None),
    }
}

pub fn list(conn: &Connection, filter: &InboxFilter, page: &Page) -> Result<Vec<(InboxItem, Key)>> {
    let mut clauses: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn ToSql>> = Vec::new();
    clauses.push(
        if filter.closed {
            "i.state IN ('resolved', 'dismissed')"
        } else {
            "i.state = 'open'"
        }
        .to_string(),
    );
    if let Some(p) = filter.project {
        binds.push(Box::new(p.to_string()));
        clauses.push(format!("i.project_id = ?{}", binds.len()));
    }
    if let Some(k) = filter.kind {
        binds.push(Box::new(k.as_str().to_string()));
        clauses.push(format!("i.kind = ?{}", binds.len()));
    }
    if let Some((key, id)) = page.after {
        binds.push(Box::new(key));
        let k = binds.len();
        binds.push(Box::new(id.to_string()));
        let i = binds.len();
        clauses.push(format!(
            "(i.opened_at < ?{k} OR (i.opened_at = ?{k} AND i.id < ?{i}))"
        ));
    }
    binds.push(Box::new(i64::from(page.limit)));
    let limit = binds.len();
    let sql = format!(
        "{SELECT} WHERE {} ORDER BY i.opened_at DESC, i.id DESC LIMIT ?{limit}",
        clauses.join(" AND ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let mut rows = stmt.query(refs.as_slice())?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let item = item_row(row)?;
        let key = (item.opened_at.0, item.id.0);
        out.push((item, key));
    }
    Ok(out)
}

pub fn summary(conn: &Connection, day_start: Timestamp) -> Result<InboxSummary> {
    let mut by_kind: BTreeMap<InboxKind, i64> = InboxKind::ALL.iter().map(|k| (*k, 0)).collect();
    let mut stmt =
        conn.prepare("SELECT kind, COUNT(*) FROM inbox_item WHERE state = 'open' GROUP BY kind")?;
    let mut rows = stmt.query([])?;
    let mut open = 0;
    while let Some(row) = rows.next()? {
        let kind: String = row.get(0)?;
        let count: i64 = row.get(1)?;
        open += count;
        if let Ok(kind) = kind.parse::<InboxKind>() {
            by_kind.insert(kind, count);
        }
    }
    let running = conn.query_row(
        "SELECT COUNT(*) FROM work_item WHERE state = 'in_stage'",
        [],
        |r| r.get(0),
    )?;
    let merged_today = conn.query_row(
        "SELECT COUNT(*) FROM change_request WHERE role = 'code' AND state = 'merged' AND merged_at >= ?1",
        [day_start.0],
        |r| r.get(0),
    )?;
    Ok(InboxSummary {
        open,
        by_kind,
        running,
        merged_today,
    })
}

pub(crate) fn dismiss(tx: &Transaction<'_>, d: &InboxDismissal, now: Timestamp) -> Result<()> {
    tx.execute(
        "UPDATE inbox_item SET state = 'dismissed', closed_at = ?2 WHERE id = ?1 AND state = 'open'",
        params![d.id.to_string(), now.0],
    )?;
    Ok(())
}

struct Live {
    id: String,
    subject: String,
    source_id: String,
    params: String,
    action: String,
}

fn live_items(tx: &Transaction<'_>, scope: Scope) -> Result<Vec<Live>> {
    let (clause, bind) = match scope {
        Scope::WorkItem(id) => ("work_item_id = ?1", id.to_string()),
        Scope::Spec(id) => ("subject = 'spec' AND spec_id = ?1", id.to_string()),
        Scope::Draft(id) => (
            "subject = 'permission' AND work_item_id IS NULL
             AND source_id IN (SELECT id FROM permission_request WHERE spec_draft_id = ?1)",
            id.to_string(),
        ),
    };
    let mut stmt = tx.prepare(&format!(
        "SELECT id, subject, source_id, params, action FROM inbox_item
         WHERE state = 'open' AND subject != 'decision' AND {clause}"
    ))?;
    let rows = stmt.query_map([bind], |r| {
        Ok(Live {
            id: r.get(0)?,
            subject: r.get(1)?,
            source_id: r.get(2)?,
            params: r.get(3)?,
            action: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn action_json(a: &InboxAction) -> Value {
    serde_json::json!({ "kind": a.kind, "params": a.params })
}

fn names(tx: &Transaction<'_>, d: &InboxDraft) -> Result<(Option<String>, Option<String>)> {
    let task_key = match d.work_item_id {
        Some(id) => tx
            .query_row(
                "SELECT task_key FROM work_item WHERE id = ?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .ok(),
        None => None,
    };
    let spec_path = match d.spec_id {
        Some(id) => tx
            .query_row(
                "SELECT path FROM spec WHERE id = ?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .ok(),
        None => None,
    };
    Ok((task_key, spec_path))
}

fn item_id(subject: &str, source_id: &str, opened_at: Timestamp, ordinal: i64) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"inbox_item\0");
    hash.update(subject.as_bytes());
    hash.update(b"\0");
    hash.update(source_id.as_bytes());
    hash.update(b"\0");
    hash.update(opened_at.0.to_be_bytes());
    hash.update(ordinal.to_be_bytes());
    let digest = hash.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

pub(crate) fn sync(
    tx: &Transaction<'_>,
    s: &InboxSync,
    now: Timestamp,
    opened: &mut Vec<OpenedItem>,
) -> Result<()> {
    let live = live_items(tx, s.scope)?;
    for item in &live {
        let wanted = s
            .desired
            .iter()
            .find(|d| d.subject.as_str() == item.subject && d.source_id == item.source_id);
        match wanted {
            None => {
                tx.execute(
                    "UPDATE inbox_item SET state = 'resolved', closed_at = ?2 WHERE id = ?1",
                    params![item.id, now.0],
                )?;
            }
            Some(d) => {
                let params_json = value_json(&d.params)?;
                let action = value_json(&action_json(&d.action))?;
                if params_json != item.params || action != item.action {
                    tx.execute(
                        "UPDATE inbox_item SET params = ?2, action = ?3 WHERE id = ?1",
                        params![item.id, params_json, action],
                    )?;
                }
            }
        }
    }
    for d in &s.desired {
        if live
            .iter()
            .any(|l| l.subject == d.subject.as_str() && l.source_id == d.source_id)
        {
            continue;
        }
        let (dismissed, prior): (i64, i64) = tx.query_row(
            "SELECT COUNT(*) FILTER (WHERE state = 'dismissed'), COUNT(*) FROM inbox_item
             WHERE subject = ?1 AND source_id = ?2",
            params![d.subject.as_str(), d.source_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if dismissed > 0 {
            continue;
        }
        let id = item_id(d.subject.as_str(), &d.source_id, now, prior);
        tx.execute(
            "INSERT INTO inbox_item (id, project_id, kind, subject, source_id, work_item_id, spec_id, state,
               params, action, opened_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'open', ?8, ?9, ?10)",
            params![
                id.to_string(),
                d.project_id.to_string(),
                d.kind.as_str(),
                d.subject.as_str(),
                d.source_id,
                d.work_item_id.map(|x| x.to_string()),
                d.spec_id.map(|x| x.to_string()),
                value_json(&d.params)?,
                value_json(&action_json(&d.action))?,
                now.0,
            ],
        )?;
        let (task_key, spec_path) = names(tx, d)?;
        opened.push(OpenedItem {
            id: InboxItemId(id),
            project_id: d.project_id,
            kind: d.kind,
            subject: d.subject,
            work_item_id: d.work_item_id,
            task_key,
            spec_id: d.spec_id,
            spec_path,
        });
    }
    Ok(())
}
