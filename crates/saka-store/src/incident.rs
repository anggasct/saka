use rusqlite::{Connection, Row, Transaction, params};
use saka_core::ids::{DeploymentId, IncidentId, ProjectId, SpecDraftId, WorkItemId};
use saka_core::incident::Incident;
use saka_core::vocab::{IncidentSeverity, IncidentState};

use crate::codec::{json, opt_uuid, opt_vocab, to_json, u32_col, uuid, vocab};
use crate::error::Result;
use crate::list::{Key, Page};
use crate::read::{opt_ts, ts};

const SELECT: &str = concat!("SELECT ", incident_columns!(), " FROM incident");

fn incident_row(row: &Row<'_>) -> Result<Incident> {
    Ok(Incident {
        id: IncidentId(uuid(row, "id")?),
        project_id: ProjectId(uuid(row, "project_id")?),
        source: row.get("source")?,
        fingerprint: row.get("fingerprint")?,
        state: vocab(row, "state")?,
        resolution: opt_vocab(row, "resolution")?,
        signal_status: vocab(row, "signal_status")?,
        severity: vocab(row, "severity")?,
        title: row.get("title")?,
        summary: row.get("summary")?,
        environment: row.get("environment")?,
        url: row.get("url")?,
        labels: json(row, "labels")?,
        occurrences: u32_col(row, "occurrences")?,
        started_at: opt_ts(row, "started_at")?,
        first_seen: ts(row, "first_seen")?,
        last_seen: ts(row, "last_seen")?,
        deployment_id: opt_uuid(row, "deployment_id")?.map(DeploymentId),
        draft_id: opt_uuid(row, "draft_id")?.map(SpecDraftId),
        revert_id: opt_uuid(row, "revert_id")?.map(WorkItemId),
        released: opt_vocab(row, "released")?,
        dismiss_reason: row.get("dismiss_reason")?,
        actor: row.get("actor")?,
        resolved_at: opt_ts(row, "resolved_at")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

pub(crate) fn upsert(tx: &Transaction<'_>, i: &Incident) -> Result<()> {
    tx.execute(
        "INSERT INTO incident (id, project_id, source, fingerprint, state, resolution, signal_status, severity,
           title, summary, environment, url, labels, occurrences, started_at, first_seen, last_seen,
           deployment_id, draft_id, revert_id, dismiss_reason, actor, resolved_at, created_at, updated_at, released)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
           ?21, ?22, ?23, ?24, ?25, ?26)
         ON CONFLICT (id) DO UPDATE SET state = excluded.state, resolution = excluded.resolution,
           signal_status = excluded.signal_status, severity = excluded.severity,
           occurrences = excluded.occurrences, last_seen = excluded.last_seen,
           draft_id = excluded.draft_id, revert_id = excluded.revert_id,
           dismiss_reason = excluded.dismiss_reason, actor = excluded.actor,
           resolved_at = excluded.resolved_at, updated_at = excluded.updated_at,
           released = excluded.released",
        params![
            i.id.to_string(),
            i.project_id.to_string(),
            i.source,
            i.fingerprint,
            i.state.as_str(),
            i.resolution.map(|r| r.as_str()),
            i.signal_status.as_str(),
            i.severity.as_str(),
            i.title,
            i.summary,
            i.environment,
            i.url,
            to_json(&i.labels)?,
            i.occurrences,
            i.started_at.map(|t| t.0),
            i.first_seen.0,
            i.last_seen.0,
            i.deployment_id.map(|id| id.to_string()),
            i.draft_id.map(|id| id.to_string()),
            i.revert_id.map(|id| id.to_string()),
            i.dismiss_reason,
            i.actor,
            i.resolved_at.map(|t| t.0),
            i.created_at.0,
            i.updated_at.0,
            i.released.map(|r| r.as_str()),
        ],
    )?;
    Ok(())
}

fn query(conn: &Connection, tail: &str, binds: &[&dyn rusqlite::ToSql]) -> Result<Vec<Incident>> {
    let mut stmt = conn.prepare(&format!("{SELECT} {tail}"))?;
    let mut rows = stmt.query(binds)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(incident_row(row)?);
    }
    Ok(out)
}

pub fn load(conn: &Connection, id: IncidentId) -> Result<Option<Incident>> {
    let id = id.to_string();
    Ok(query(conn, "WHERE id = ?1", &[&id])?.into_iter().next())
}

pub fn live_counts(conn: &Connection, project: ProjectId) -> Result<Vec<(String, u32)>> {
    let mut stmt = conn.prepare(
        "SELECT source, count(*) AS live FROM incident
         WHERE project_id = ?1 AND state IN ('open','handling') GROUP BY source ORDER BY source",
    )?;
    let mut rows = stmt.query([project.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push((row.get("source")?, u32_col(row, "live")?));
    }
    Ok(out)
}

#[derive(Debug, Clone, Default)]
pub struct IncidentFilter {
    pub project: Option<ProjectId>,
    pub state: Option<IncidentState>,
    pub severity: Option<IncidentSeverity>,
    pub environment: Option<String>,
    pub source: Option<String>,
}

pub fn list(
    conn: &Connection,
    filter: &IncidentFilter,
    page: &Page,
) -> Result<Vec<(Incident, Key)>> {
    let mut clauses: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut add = |clause: &str,
                   value: Box<dyn rusqlite::ToSql>,
                   binds: &mut Vec<Box<dyn rusqlite::ToSql>>| {
        binds.push(value);
        clauses.push(clause.replace('?', &format!("?{}", binds.len())));
    };
    if let Some(p) = filter.project {
        add("project_id = ?", Box::new(p.to_string()), &mut binds);
    }
    if let Some(s) = filter.state {
        add("state = ?", Box::new(s.as_str().to_string()), &mut binds);
    }
    if let Some(s) = filter.severity {
        add("severity = ?", Box::new(s.as_str().to_string()), &mut binds);
    }
    if let Some(e) = &filter.environment {
        add("environment = ?", Box::new(e.clone()), &mut binds);
    }
    if let Some(s) = &filter.source {
        add("source = ?", Box::new(s.clone()), &mut binds);
    }
    if let Some((key, id)) = page.after {
        binds.push(Box::new(key));
        let k = binds.len();
        binds.push(Box::new(id.to_string()));
        let i = binds.len();
        clauses.push(format!(
            "(last_seen < ?{k} OR (last_seen = ?{k} AND id < ?{i}))"
        ));
    }
    binds.push(Box::new(i64::from(page.limit)));
    let limit = binds.len();
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    let refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let found = query(
        conn,
        &format!("{where_sql} ORDER BY last_seen DESC, id DESC LIMIT ?{limit}"),
        refs.as_slice(),
    )?;
    Ok(found
        .into_iter()
        .map(|i| {
            let key = (i.last_seen.0, i.id.0);
            (i, key)
        })
        .collect())
}

pub fn latest_of(conn: &Connection, source: &str, fingerprint: &str) -> Result<Option<Incident>> {
    let found = query(
        conn,
        "WHERE source = ?1 AND fingerprint = ?2 ORDER BY created_at DESC, id DESC LIMIT 1",
        &[&source, &fingerprint],
    )?;
    Ok(found.into_iter().next())
}

pub fn live_count(conn: &Connection, source: &str) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM incident WHERE source = ?1 AND state IN ('open','handling')",
        [source],
        |row| row.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

pub fn handling(conn: &Connection) -> Result<Vec<Incident>> {
    query(
        conn,
        "WHERE state = 'handling' ORDER BY created_at, id",
        &[],
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRow {
    pub id: uuid::Uuid,
    pub source: String,
    pub delivery_key: String,
    pub received_at: saka_core::time::Timestamp,
    pub outcome: &'static str,
    pub signals: u32,
}

pub fn delivery_seen(
    conn: &Connection,
    source: &str,
    delivery_key: &str,
) -> Result<Option<saka_core::time::Timestamp>> {
    let found: Option<i64> = conn
        .query_row(
            concat!(
                "SELECT ",
                incident_delivery_columns!(),
                " FROM incident_delivery WHERE source = ?1 AND delivery_key = ?2"
            ),
            params![source, delivery_key],
            |row| row.get("received_at"),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    Ok(found.map(saka_core::time::Timestamp))
}

pub(crate) fn upsert_delivery(tx: &Transaction<'_>, d: &DeliveryRow) -> Result<()> {
    tx.execute(
        "INSERT INTO incident_delivery (id, source, delivery_key, received_at, outcome, signals)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (source, delivery_key) DO UPDATE SET received_at = excluded.received_at,
           outcome = excluded.outcome, signals = excluded.signals",
        params![
            d.id.to_string(),
            d.source,
            d.delivery_key,
            d.received_at.0,
            d.outcome,
            d.signals
        ],
    )?;
    Ok(())
}

pub fn revert_brief(
    conn: &Connection,
    id: WorkItemId,
) -> Result<Option<(String, saka_core::vocab::WorkItemState)>> {
    let mut stmt = conn.prepare("SELECT task_key, state FROM work_item WHERE id = ?1")?;
    let mut found = stmt.query([id.to_string()])?;
    match found.next()? {
        Some(row) => Ok(Some((row.get("task_key")?, vocab(row, "state")?))),
        None => Ok(None),
    }
}
