use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, Row, params};
use saka_core::ids::{ProjectId, SessionId, SpecDraftAttemptId, SpecDraftId, SpecId};
use saka_core::model::{Draft, LintRound, RetryRequest, RoundMark, SpecDraft, SpecDraftAttempt};
use saka_core::time::Timestamp;
use saka_core::vocab::{EnvironmentState, PermissionState, ResumeCause, SessionState};
use serde_json::Value;

use crate::codec::{json, opt_json, opt_uuid, u32_col, u64_col, uuid, vocab};
use crate::error::Result;
use crate::read::{
    DECISION_SELECT, ENVIRONMENT_SELECT, PERMISSION_SELECT, SESSION_SELECT, decision, environment,
    one, opt_ts, permission, rows_of, running_since, session, ts,
};

#[derive(Debug, Clone, Copy)]
pub enum DraftSet<'a> {
    Ids(&'a [SpecDraftId]),
    Live,
    Project(ProjectId),
}

const LIVE: &str = "SELECT id FROM spec_draft WHERE state NOT IN ('succeeded', 'cancelled')
    OR id IN (SELECT spec_draft_id FROM environment
              WHERE spec_draft_id IS NOT NULL AND state IN ('preparing', 'ready', 'teardown_failed'))
    OR id IN (SELECT spec_draft_id FROM session
              WHERE spec_draft_id IS NOT NULL AND state IN ('starting', 'running', 'waiting', 'idle', 'interrupted'))";

fn spec_draft(row: &Row<'_>) -> Result<SpecDraft> {
    Ok(SpecDraft {
        id: SpecDraftId(uuid(row, "id")?),
        project_id: ProjectId(uuid(row, "project_id")?),
        path: row.get("path")?,
        slug: row.get("slug")?,
        idea: row.get("idea")?,
        locale: vocab(row, "locale")?,
        mode: vocab(row, "mode")?,
        target_kind: vocab(row, "target_kind")?,
        kind: vocab(row, "kind")?,
        context_paths: json(row, "context_paths")?,
        deployment_id: opt_uuid(row, "deployment_id")?.map(saka_core::ids::DeploymentId),
        incident_id: opt_uuid(row, "incident_id")?.map(saka_core::ids::IncidentId),
        state: vocab(row, "state")?,
        attempt: u32_col(row, "attempt")?,
        current_attempt_id: opt_uuid(row, "current_attempt_id")?.map(SpecDraftAttemptId),
        retries: u32_col(row, "retries")?,
        queued_at: ts(row, "queued_at")?,
        awaiting_since: opt_ts(row, "awaiting_since")?,
        failure: opt_json(row, "failure")?,
        spec_id: opt_uuid(row, "spec_id")?.map(SpecId),
        commit_sha: row.get("commit_sha")?,
        version: u64_col(row, "version")?,
        created_by: row.get("created_by")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
        finished_at: opt_ts(row, "finished_at")?,
    })
}

fn attempt(row: &Row<'_>) -> Result<SpecDraftAttempt> {
    let resumed: i64 = row.get("resumed_lost")?;
    Ok(SpecDraftAttempt {
        id: SpecDraftAttemptId(uuid(row, "id")?),
        spec_draft_id: SpecDraftId(uuid(row, "spec_draft_id")?),
        attempt: u32_col(row, "attempt")?,
        trigger: vocab(row, "trigger")?,
        reason: row.get("reason")?,
        note: row.get("note")?,
        state: vocab(row, "state")?,
        base_sha: row.get("base_sha")?,
        budget: json(row, "budget")?,
        budget_warnings: json(row, "budget_warnings")?,
        resumed_lost: resumed != 0,
        rounds: u32_col(row, "rounds")?,
        packet_sha256: row.get("packet_sha256")?,
        failure: opt_json(row, "failure")?,
        started_at: ts(row, "started_at")?,
        ended_at: opt_ts(row, "ended_at")?,
    })
}

const ROUND_COLUMNS: &str =
    "r.attempt_id, r.round, r.spec_draft_id, r.session_id, r.turn, r.outcome, r.bytes,
    r.content, r.content_hash, r.parser_version, r.issues, r.decisions_open, r.created_at";

fn round(row: &Row<'_>) -> Result<LintRound> {
    let bytes: Option<i64> = row.get("bytes")?;
    Ok(LintRound {
        spec_draft_id: SpecDraftId(uuid(row, "spec_draft_id")?),
        attempt_id: SpecDraftAttemptId(uuid(row, "attempt_id")?),
        round: u32_col(row, "round")?,
        session_id: SessionId(uuid(row, "session_id")?),
        turn: u32_col(row, "turn")?,
        outcome: vocab(row, "outcome")?,
        bytes: bytes.map(|b| u64::try_from(b).unwrap_or(0)),
        content: row.get("content")?,
        content_hash: row.get("content_hash")?,
        parser_version: u32_col(row, "parser_version")?,
        issues: json(row, "issues")?,
        decisions_open: u32_col(row, "decisions_open")?,
        created_at: ts(row, "created_at")?,
    })
}

pub fn load_drafts(conn: &Connection, set: DraftSet<'_>) -> Result<Vec<Draft>> {
    let ids_json;
    let project;
    let (selection, bind): (&str, Vec<&dyn rusqlite::ToSql>) = match set {
        DraftSet::Ids(ids) => {
            ids_json =
                serde_json::to_string(&ids.iter().map(|i| i.0.to_string()).collect::<Vec<_>>())?;
            ("SELECT value FROM json_each(?1)", vec![&ids_json])
        }
        DraftSet::Live => (LIVE, vec![]),
        DraftSet::Project(id) => {
            project = id.0.to_string();
            (
                "SELECT id FROM spec_draft WHERE project_id = ?1",
                vec![&project],
            )
        }
    };
    let b = bind.as_slice();
    let mut drafts: BTreeMap<SpecDraftId, Draft> = rows_of(
        conn,
        concat!("SELECT ", spec_draft_columns!(), " FROM spec_draft"),
        "id",
        selection,
        "created_at, id",
        b,
        spec_draft,
    )?
    .into_iter()
    .map(|d| (d.id, Draft::new(d)))
    .collect();
    if drafts.is_empty() {
        return Ok(Vec::new());
    }
    for a in rows_of(
        conn,
        concat!(
            "SELECT ",
            spec_draft_attempt_columns!(),
            " FROM spec_draft_attempt"
        ),
        "spec_draft_id",
        selection,
        "attempt DESC",
        b,
        attempt,
    )? {
        if let Some(d) = drafts.get_mut(&a.spec_draft_id)
            && d.attempt.is_none()
        {
            d.attempt = Some(a);
        }
    }
    for e in rows_of(
        conn,
        ENVIRONMENT_SELECT,
        "spec_draft_id",
        selection,
        "created_at DESC, id DESC",
        b,
        environment,
    )? {
        let Some(d) = e.owner.spec_draft_id().and_then(|id| drafts.get_mut(&id)) else {
            continue;
        };
        if d.environment.is_none() {
            d.environment = Some(e);
        } else if matches!(
            e.state,
            EnvironmentState::Preparing
                | EnvironmentState::Ready
                | EnvironmentState::TeardownFailed
        ) {
            d.retired_environments.push(e);
        }
    }
    for s in rows_of(
        conn,
        SESSION_SELECT,
        "spec_draft_id",
        selection,
        "created_at DESC, id DESC",
        b,
        session,
    )? {
        let Some(d) = s.owner.spec_draft_id().and_then(|id| drafts.get_mut(&id)) else {
            continue;
        };
        let current = d
            .attempt
            .as_ref()
            .is_some_and(|a| s.owner.attempt_id() == Some(a.id));
        if current
            && s.resume_cause == Some(ResumeCause::Lost)
            && let Some(a) = d.attempt.as_mut()
        {
            a.resumed_lost = true;
        }
        match &d.session {
            None => d.session = Some(s),
            Some(_) if current => d.prior_usage.add(s.usage_at(Timestamp(0))),
            Some(_) => {}
        }
    }
    let running: HashMap<SessionId, Timestamp> =
        running_since(conn, "spec_draft_id", selection, b)?;
    let latest: HashMap<SessionId, SpecDraftId> = drafts
        .values()
        .filter_map(|d| d.session.as_ref().map(|s| (s.id, d.draft.id)))
        .collect();
    for d in drafts.values_mut() {
        if let Some(s) = d
            .session
            .as_mut()
            .filter(|s| matches!(s.state, SessionState::Running | SessionState::Interrupted))
        {
            s.running_since = running.get(&s.id).copied();
        }
    }
    for p in rows_of(
        conn,
        PERMISSION_SELECT,
        "spec_draft_id",
        selection,
        "requested_at, id",
        b,
        permission,
    )? {
        let wanted = p.state == PermissionState::Pending
            || (p.delivered_at.is_none() && saka_core::session::awaits_delivery(&p));
        let owner = p.owner.spec_draft_id();
        if wanted
            && latest.get(&p.session_id).copied() == owner
            && let Some(d) = owner.and_then(|id| drafts.get_mut(&id))
        {
            d.permissions.push(p);
        }
    }
    for r in rows_of(
        conn,
        DECISION_SELECT,
        "json_extract(owner, '$.spec_draft_id')",
        selection,
        "asked_at, id",
        b,
        decision,
    )? {
        if let Some(d) = r.owner.spec_draft_id().and_then(|id| drafts.get_mut(&id)) {
            d.decisions.push(r);
        }
    }
    let marks = "SELECT r.attempt_id, r.round, r.session_id, r.turn, r.outcome, r.spec_draft_id
         FROM spec_draft_lint_round r JOIN spec_draft_attempt a ON a.id = r.attempt_id";
    let sql = format!(
        "{marks} WHERE r.spec_draft_id IN ({selection}) ORDER BY a.attempt DESC, r.round DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut q = stmt.query(b)?;
    while let Some(row) = q.next()? {
        let id = SpecDraftId(uuid(row, "spec_draft_id")?);
        if let Some(d) = drafts.get_mut(&id)
            && d.last_round.is_none()
        {
            d.last_round = Some(RoundMark {
                attempt_id: SpecDraftAttemptId(uuid(row, "attempt_id")?),
                round: u32_col(row, "round")?,
                session_id: SessionId(uuid(row, "session_id")?),
                turn: u32_col(row, "turn")?,
                outcome: vocab(row, "outcome")?,
            });
        }
    }
    let sql = format!(
        "SELECT DISTINCT spec_draft_id FROM spec_draft_lint_round
         WHERE content IS NOT NULL AND spec_draft_id IN ({selection})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut q = stmt.query(b)?;
    while let Some(row) = q.next()? {
        if let Some(d) = drafts.get_mut(&SpecDraftId(uuid(row, "spec_draft_id")?)) {
            d.has_content = true;
        }
    }
    for d in drafts.values_mut().filter(|d| d.draft.retries > 0) {
        d.retry_request = retry_request(conn, d.draft.id)?;
    }
    Ok(drafts.into_values().collect())
}

fn retry_request(conn: &Connection, id: SpecDraftId) -> Result<Option<RetryRequest>> {
    let payload: Option<Value> = one(
        conn,
        "SELECT payload FROM event WHERE stream_type = 'spec_draft' AND stream_id = ?1
           AND kind = 'spec.draft.retried' ORDER BY seq DESC LIMIT 1",
        &[&id.0.to_string()],
        |row| json(row, "payload"),
    )?;
    Ok(payload.map(|p| RetryRequest {
        reason: p
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        note: p.get("note").and_then(Value::as_str).map(String::from),
    }))
}

pub fn load_draft(conn: &Connection, id: SpecDraftId) -> Result<Option<Draft>> {
    Ok(load_drafts(conn, DraftSet::Ids(&[id]))?.into_iter().next())
}

pub fn lint_round(
    conn: &Connection,
    attempt: SpecDraftAttemptId,
    number: u32,
) -> Result<Option<LintRound>> {
    one(
        conn,
        &format!(
            "SELECT {ROUND_COLUMNS} FROM spec_draft_lint_round r WHERE r.attempt_id = ?1 AND r.round = ?2"
        ),
        params![attempt.0.to_string(), number],
        round,
    )
}

pub fn lint_rounds(conn: &Connection, id: SpecDraftId) -> Result<Vec<(u32, LintRound)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT a.attempt AS attempt_number, {ROUND_COLUMNS} FROM spec_draft_lint_round r
         JOIN spec_draft_attempt a ON a.id = r.attempt_id
         WHERE r.spec_draft_id = ?1 ORDER BY a.attempt, r.round"
    ))?;
    let mut q = stmt.query([id.0.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push((u32_col(row, "attempt_number")?, round(row)?));
    }
    Ok(out)
}

pub fn last_written(conn: &Connection, id: SpecDraftId) -> Result<Option<(u32, LintRound)>> {
    one(
        conn,
        &format!(
            "SELECT a.attempt AS attempt_number, {ROUND_COLUMNS} FROM spec_draft_lint_round r
             JOIN spec_draft_attempt a ON a.id = r.attempt_id
             WHERE r.spec_draft_id = ?1 AND r.content IS NOT NULL
             ORDER BY a.attempt DESC, r.round DESC LIMIT 1"
        ),
        &[&id.0.to_string()],
        |row| Ok((u32_col(row, "attempt_number")?, round(row)?)),
    )
}

pub fn last_round(conn: &Connection, id: SpecDraftId) -> Result<Option<(u32, LintRound)>> {
    one(
        conn,
        &format!(
            "SELECT a.attempt AS attempt_number, {ROUND_COLUMNS} FROM spec_draft_lint_round r
             JOIN spec_draft_attempt a ON a.id = r.attempt_id
             WHERE r.spec_draft_id = ?1
             ORDER BY a.attempt DESC, r.round DESC LIMIT 1"
        ),
        &[&id.0.to_string()],
        |row| Ok((u32_col(row, "attempt_number")?, round(row)?)),
    )
}

pub fn packet_rendered(
    conn: &Connection,
    id: SpecDraftId,
    attempt: SpecDraftAttemptId,
) -> Result<Option<Value>> {
    one(
        conn,
        "SELECT payload FROM event
         WHERE stream_type = 'spec_draft' AND stream_id = ?1
           AND kind = 'spec.draft.packet_rendered'
           AND json_extract(payload, '$.attempt_id') = ?2
         ORDER BY seq DESC LIMIT 1",
        params![id.0.to_string(), attempt.0.to_string()],
        |row| json(row, "payload"),
    )
}

pub fn last_round_of(conn: &Connection, attempt: SpecDraftAttemptId) -> Result<Option<LintRound>> {
    one(
        conn,
        &format!(
            "SELECT {ROUND_COLUMNS} FROM spec_draft_lint_round r WHERE r.attempt_id = ?1
             ORDER BY r.round DESC LIMIT 1"
        ),
        &[&attempt.0.to_string()],
        round,
    )
}

pub fn previous_attempt(
    conn: &Connection,
    id: SpecDraftId,
    number: u32,
) -> Result<Option<SpecDraftAttempt>> {
    one(
        conn,
        concat!(
            "SELECT ",
            spec_draft_attempt_columns!(),
            " FROM spec_draft_attempt WHERE spec_draft_id = ?1 AND attempt = ?2"
        ),
        params![id.0.to_string(), number],
        attempt,
    )
}

pub fn packet(conn: &Connection, attempt: SpecDraftAttemptId) -> Result<Option<String>> {
    Ok(one(
        conn,
        "SELECT packet FROM spec_draft_attempt WHERE id = ?1",
        &[&attempt.0.to_string()],
        |row| Ok(row.get::<_, Option<String>>("packet")?),
    )?
    .flatten())
}

pub fn live_draft_at(
    conn: &Connection,
    project: ProjectId,
    path: &str,
) -> Result<Option<SpecDraftId>> {
    one(
        conn,
        "SELECT id FROM spec_draft WHERE project_id = ?1 AND path = ?2
           AND state IN ('queued', 'preparing', 'drafting', 'awaiting_operator', 'committing', 'failed')",
        params![project.0.to_string(), path],
        |row| Ok(SpecDraftId(uuid(row, "id")?)),
    )
}

pub fn active_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM spec_draft WHERE state IN ('preparing', 'drafting', 'committing')",
        [],
        |row| row.get(0),
    )?)
}

pub fn fix_draft_of(
    conn: &Connection,
    deployment: saka_core::ids::DeploymentId,
) -> Result<Option<(SpecDraftId, saka_core::vocab::SpecDraftState)>> {
    let mut stmt = conn.prepare(
        "SELECT id, state FROM spec_draft WHERE deployment_id = ?1
         ORDER BY (state NOT IN ('succeeded','cancelled')) DESC, created_at DESC, id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([deployment.to_string()])?;
    match rows.next()? {
        Some(row) => Ok(Some((SpecDraftId(uuid(row, "id")?), vocab(row, "state")?))),
        None => Ok(None),
    }
}
