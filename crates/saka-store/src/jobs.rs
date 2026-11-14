use rusqlite::{Connection, params};
use saka_core::ids::ProjectId;
use saka_core::job::JobKind;
use saka_core::time::Timestamp;
use serde_json::Value;
use uuid::Uuid;

use crate::codec::{json, opt_json, opt_uuid, u32_col, uuid};
use crate::error::{Result, StoreError};

pub const LEASE_MS: i64 = 60_000;
pub const LEASE_EXTEND_MS: i64 = 20_000;

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedJob {
    pub id: Uuid,
    pub kind: JobKind,
    pub idempotency_key: String,
    pub project_id: Option<ProjectId>,
    pub payload: Value,
    pub attempts: u32,
    pub max_attempts: u32,
    pub last_error: Option<Value>,
    pub causation_id: Option<Uuid>,
    pub correlation_id: Uuid,
    pub lease_owner: String,
    pub lease_until: Timestamp,
}

impl ClaimedJob {
    pub fn is_over_limit(&self) -> bool {
        self.attempts > self.max_attempts
    }

    pub fn backoff(&self, now: Timestamp) -> Timestamp {
        let exp = self.attempts.min(10);
        let ms = (5_000_i64 << exp).min(600_000);
        now.plus_ms(ms)
    }
}

pub(crate) fn claim(conn: &Connection, owner: &str, now: Timestamp) -> Result<Option<ClaimedJob>> {
    let mut stmt = conn.prepare(
        "UPDATE job SET state = 'leased', lease_owner = ?1, lease_until = ?2, attempts = attempts + 1, updated_at = ?3
         WHERE id = (
           SELECT id FROM job
           WHERE ((state IN ('queued', 'failed') AND run_at <= ?3) OR (state = 'leased' AND lease_until < ?3))
             AND NOT (kind = ?4 AND EXISTS (
               SELECT 1 FROM job running
               WHERE running.kind = job.kind AND running.id <> job.id
                 AND running.state = 'leased' AND running.lease_until >= ?3
                 AND json_extract(running.payload, '$.work_item_id') = json_extract(job.payload, '$.work_item_id')))
           ORDER BY run_at, id LIMIT 1)
         RETURNING id, kind, idempotency_key, project_id, payload, attempts, max_attempts, last_error, causation_id,
           correlation_id, lease_owner, lease_until",
    )?;
    let mut q = stmt.query(params![
        owner,
        now.0 + LEASE_MS,
        now.0,
        JobKind::GateVerify.as_str()
    ])?;
    let Some(row) = q.next()? else {
        return Ok(None);
    };
    let kind: String = row.get("kind")?;
    Ok(Some(ClaimedJob {
        id: uuid(row, "id")?,
        kind: JobKind::parse(&kind)
            .ok_or_else(|| StoreError::Corrupt(format!("job.kind: {kind}")))?,
        idempotency_key: row.get("idempotency_key")?,
        project_id: opt_uuid(row, "project_id")?.map(ProjectId),
        payload: json(row, "payload")?,
        attempts: u32_col(row, "attempts")?,
        max_attempts: u32_col(row, "max_attempts")?,
        last_error: opt_json(row, "last_error")?,
        causation_id: opt_uuid(row, "causation_id")?,
        correlation_id: uuid(row, "correlation_id")?,
        lease_owner: row.get("lease_owner")?,
        lease_until: Timestamp(row.get("lease_until")?),
    }))
}

pub(crate) fn extend_lease(
    conn: &Connection,
    id: Uuid,
    owner: &str,
    now: Timestamp,
) -> Result<Timestamp> {
    let until = now.plus_ms(LEASE_MS);
    let changed = conn.execute(
        "UPDATE job SET lease_until = ?3, updated_at = ?4 WHERE id = ?1 AND state = 'leased' AND lease_owner = ?2",
        params![id.to_string(), owner, until.0, now.0],
    )?;
    if changed == 1 {
        Ok(until)
    } else {
        Err(StoreError::LeaseLost {
            job: id.to_string(),
        })
    }
}

pub(crate) fn counts(conn: &Connection) -> Result<Vec<(String, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT kind, state, COUNT(*) FROM job GROUP BY kind, state ORDER BY kind, state",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub(crate) fn oldest_ready_age(conn: &Connection, now: Timestamp) -> Result<Option<i64>> {
    let run_at: Option<i64> = conn.query_row(
        "SELECT MIN(run_at) FROM job WHERE state IN ('queued', 'failed') AND run_at <= ?1",
        params![now.0],
        |r| r.get(0),
    )?;
    Ok(run_at.map(|at| now.0 - at))
}
