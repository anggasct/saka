use rusqlite::{Connection, Row, Transaction, params};
use saka_core::deploy::{CheckRecord, Deployment};
use saka_core::ids::{ChangeRequestId, DeploymentId, ProjectId, RepoId, WorkItemId};
use saka_core::model::Coded;
use saka_core::vocab::{DeployGateState, DeploymentState};

use crate::codec::{opt_json, opt_vocab, to_json, u32_col, uuid, vocab};
use crate::error::Result;
use crate::list::{Key, Page};
use crate::read::{opt_ts, ts};

const SELECT: &str = concat!("SELECT ", deployment_columns!(), " FROM deployment");
const CHECK_SELECT: &str = concat!(
    "SELECT ",
    deployment_check_columns!(),
    " FROM deployment_check"
);

fn deployment_row(row: &Row<'_>) -> Result<Deployment> {
    Ok(Deployment {
        id: DeploymentId(uuid(row, "id")?),
        project_id: ProjectId(uuid(row, "project_id")?),
        repo_id: RepoId(uuid(row, "repo_id")?),
        environment: row.get("environment")?,
        branch: row.get("branch")?,
        sha: row.get("sha")?,
        previous_sha: row.get("previous_sha")?,
        cause: vocab(row, "cause")?,
        state: vocab(row, "state")?,
        phase: opt_vocab(row, "phase")?,
        error: opt_json::<Coded>(row, "error")?,
        action: vocab(row, "action")?,
        target_url: row.get("target_url")?,
        verification: opt_json(row, "verification")?,
        actor: row.get("actor")?,
        reason: row.get("reason")?,
        merges: Vec::new(),
        queued_at: ts(row, "queued_at")?,
        started_at: opt_ts(row, "started_at")?,
        finished_at: opt_ts(row, "finished_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

fn check_row(row: &Row<'_>) -> Result<CheckRecord> {
    Ok(CheckRecord {
        phase: vocab(row, "phase")?,
        ordinal: u32_col(row, "ordinal")?,
        command: row.get("command")?,
        state: vocab(row, "state")?,
        exit_code: row.get("exit_code")?,
        http_status: opt_u16(row, "http_status")?,
        attempts: u32_col(row, "attempts")?,
        duration_ms: row.get("duration_ms")?,
        output_tail: row.get("output_tail")?,
        log: row.get("log")?,
        started_at: ts(row, "started_at")?,
        finished_at: ts(row, "finished_at")?,
    })
}

fn opt_u16(row: &Row<'_>, col: &str) -> Result<Option<u16>> {
    let value: Option<i64> = row.get(col)?;
    value
        .map(|v| {
            u16::try_from(v)
                .map_err(|_| crate::error::StoreError::Corrupt(format!("{col}: out of range: {v}")))
        })
        .transpose()
}

pub(crate) fn upsert(tx: &Transaction<'_>, d: &Deployment) -> Result<()> {
    tx.execute(
        "INSERT INTO deployment (id, project_id, repo_id, environment, branch, sha, previous_sha, cause, state,
           phase, error, action, target_url, verification, actor, reason, queued_at, started_at, finished_at,
           updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
         ON CONFLICT (id) DO UPDATE SET sha = excluded.sha, previous_sha = excluded.previous_sha,
           state = excluded.state, phase = excluded.phase, error = excluded.error,
           verification = excluded.verification, actor = excluded.actor, reason = excluded.reason,
           started_at = excluded.started_at, finished_at = excluded.finished_at,
           updated_at = excluded.updated_at",
        params![
            d.id.to_string(),
            d.project_id.to_string(),
            d.repo_id.to_string(),
            d.environment,
            d.branch,
            d.sha,
            d.previous_sha,
            d.cause.as_str(),
            d.state.as_str(),
            d.phase.map(|p| p.as_str()),
            d.error.as_ref().map(to_json).transpose()?,
            d.action.as_str(),
            d.target_url,
            d.verification.as_ref().map(to_json).transpose()?,
            d.actor,
            d.reason,
            d.queued_at.0,
            d.started_at.map(|t| t.0),
            d.finished_at.map(|t| t.0),
            d.updated_at.0,
        ],
    )?;
    for merge in &d.merges {
        tx.execute(
            "INSERT OR IGNORE INTO deployment_merge (deployment_id, change_request_id) VALUES (?1, ?2)",
            params![d.id.to_string(), merge.to_string()],
        )?;
    }
    Ok(())
}

pub(crate) fn upsert_check(
    tx: &Transaction<'_>,
    deployment: DeploymentId,
    c: &CheckRecord,
) -> Result<()> {
    tx.execute(
        "INSERT INTO deployment_check (deployment_id, phase, ordinal, command, state, exit_code, http_status,
           attempts, duration_ms, output_tail, log, started_at, finished_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT (deployment_id, phase, ordinal) DO UPDATE SET command = excluded.command,
           state = excluded.state, exit_code = excluded.exit_code, http_status = excluded.http_status,
           attempts = excluded.attempts, duration_ms = excluded.duration_ms,
           output_tail = excluded.output_tail, log = excluded.log, started_at = excluded.started_at,
           finished_at = excluded.finished_at",
        params![
            deployment.to_string(),
            c.phase.as_str(),
            c.ordinal,
            c.command,
            c.state.as_str(),
            c.exit_code,
            c.http_status,
            c.attempts,
            c.duration_ms,
            c.output_tail,
            c.log,
            c.started_at.0,
            c.finished_at.0,
        ],
    )?;
    Ok(())
}

fn merges_of(conn: &Connection, id: DeploymentId) -> Result<Vec<ChangeRequestId>> {
    let mut stmt = conn.prepare(
        "SELECT m.change_request_id AS id FROM deployment_merge m
         JOIN change_request c ON c.id = m.change_request_id
         WHERE m.deployment_id = ?1 ORDER BY c.created_at, c.id",
    )?;
    let mut rows = stmt.query([id.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(ChangeRequestId(uuid(row, "id")?));
    }
    Ok(out)
}

fn query(conn: &Connection, tail: &str, binds: &[&dyn rusqlite::ToSql]) -> Result<Vec<Deployment>> {
    let mut stmt = conn.prepare(&format!("{SELECT} {tail}"))?;
    let mut rows = stmt.query(binds)?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(deployment_row(row)?);
    }
    for d in &mut out {
        d.merges = merges_of(conn, d.id)?;
    }
    Ok(out)
}

pub fn load(conn: &Connection, id: DeploymentId) -> Result<Option<Deployment>> {
    let id = id.to_string();
    Ok(query(conn, "WHERE id = ?1", &[&id])?.into_iter().next())
}

pub fn unfinished(conn: &Connection) -> Result<Vec<Deployment>> {
    query(
        conn,
        "WHERE state IN ('queued','deploying','awaiting_ready','verifying') ORDER BY queued_at, id",
        &[],
    )
}

pub fn queued_of(
    conn: &Connection,
    project: ProjectId,
    environment: &str,
) -> Result<Option<Deployment>> {
    let (project, state) = (project.to_string(), DeploymentState::Queued.as_str());
    let found = query(
        conn,
        "WHERE project_id = ?1 AND environment = ?2 AND state = ?3",
        &[&project, &environment, &state],
    )?;
    Ok(found.into_iter().next())
}

pub fn active_of(
    conn: &Connection,
    project: ProjectId,
    environment: &str,
) -> Result<Option<Deployment>> {
    let project = project.to_string();
    let found = query(
        conn,
        "WHERE project_id = ?1 AND environment = ?2 AND state IN ('deploying','awaiting_ready','verifying')",
        &[&project, &environment],
    )?;
    Ok(found.into_iter().next())
}

pub fn last_succeeded(
    conn: &Connection,
    project: ProjectId,
    environment: &str,
) -> Result<Option<Deployment>> {
    let project = project.to_string();
    let found = query(
        conn,
        "WHERE project_id = ?1 AND environment = ?2 AND state = 'succeeded'
         ORDER BY finished_at DESC, id DESC LIMIT 1",
        &[&project, &environment],
    )?;
    Ok(found.into_iter().next())
}

pub fn checks(conn: &Connection, id: DeploymentId) -> Result<Vec<CheckRecord>> {
    let mut stmt = conn.prepare(&format!(
        "{CHECK_SELECT} WHERE deployment_id = ?1 ORDER BY started_at, ordinal"
    ))?;
    let mut rows = stmt.query([id.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(check_row(row)?);
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRef {
    pub change_request_id: ChangeRequestId,
    pub number: u64,
    pub url: String,
    pub merge_commit_sha: Option<String>,
}

pub fn merge_refs(conn: &Connection, id: DeploymentId) -> Result<Vec<MergeRef>> {
    let mut stmt = conn.prepare(
        "SELECT c.id AS id, c.number AS number, c.url AS url, c.merge_commit_sha AS merge_commit_sha
         FROM deployment_merge m JOIN change_request c ON c.id = m.change_request_id
         WHERE m.deployment_id = ?1 ORDER BY c.created_at, c.id",
    )?;
    let mut rows = stmt.query([id.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(MergeRef {
            change_request_id: ChangeRequestId(uuid(row, "id")?),
            number: crate::codec::u64_col(row, "number")?,
            url: row.get("url")?,
            merge_commit_sha: row.get("merge_commit_sha")?,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRef {
    pub work_item_id: WorkItemId,
    pub task_key: String,
    pub gate: Option<DeployGateState>,
}

pub fn tasks_of(conn: &Connection, id: DeploymentId) -> Result<Vec<TaskRef>> {
    let mut stmt = conn.prepare(
        "SELECT w.id AS id, w.task_key AS task_key, g.state AS gate
         FROM deployment_task t
         JOIN deployment d ON d.id = t.deployment_id
         JOIN work_item w ON w.id = t.work_item_id
         LEFT JOIN deploy_gate g ON g.work_item_id = w.id AND g.environment = d.environment
         WHERE t.deployment_id = ?1 ORDER BY w.created_at, w.id",
    )?;
    let mut rows = stmt.query([id.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(TaskRef {
            work_item_id: WorkItemId(uuid(row, "id")?),
            task_key: row.get("task_key")?,
            gate: opt_vocab(row, "gate")?,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, Default)]
pub struct DeploymentFilter {
    pub project: Option<ProjectId>,
    pub environment: Option<String>,
    pub state: Option<DeploymentState>,
}

pub fn list(
    conn: &Connection,
    filter: &DeploymentFilter,
    page: &Page,
) -> Result<Vec<(Deployment, Key)>> {
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
    if let Some(e) = &filter.environment {
        add("environment = ?", Box::new(e.clone()), &mut binds);
    }
    if let Some(s) = filter.state {
        add("state = ?", Box::new(s.as_str().to_string()), &mut binds);
    }
    if let Some((key, id)) = page.after {
        binds.push(Box::new(key));
        let k = binds.len();
        binds.push(Box::new(id.to_string()));
        let i = binds.len();
        clauses.push(format!(
            "(queued_at < ?{k} OR (queued_at = ?{k} AND id < ?{i}))"
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
        &format!("{where_sql} ORDER BY queued_at DESC, id DESC LIMIT ?{limit}"),
        refs.as_slice(),
    )?;
    Ok(found
        .into_iter()
        .map(|d| {
            let key = (d.queued_at.0, d.id.0);
            (d, key)
        })
        .collect())
}
