use rusqlite::{Connection, Row};
use saka_core::ids::{IssueIntakeId, ProjectId, RepoId, SpecDraftId, SpecId};
use saka_core::issue_intake::IssueIntake;
use saka_core::model::{Repo, Spec};
use saka_core::time::Timestamp;
use saka_core::vocab::SpecDraftState;
use uuid::Uuid;

use crate::codec::{json, opt_json, opt_uuid, u32_col, u64_col, uuid, vocab};
use crate::error::Result;
use crate::error::StoreError;
use crate::list::{Key, Page};
use crate::read::{load_repo, load_spec, one, ts};
use crate::write::ChangeCursorRow;

fn intake(row: &Row<'_>) -> Result<IssueIntake> {
    Ok(IssueIntake {
        id: IssueIntakeId(uuid(row, "id")?),
        project_id: ProjectId(uuid(row, "project_id")?),
        repo_id: RepoId(uuid(row, "repo_id")?),
        number: u64_col(row, "number")?,
        url: row.get("url")?,
        title: row.get("title")?,
        author: row.get("author")?,
        state: vocab(row, "state")?,
        lane: vocab(row, "lane")?,
        kind: vocab(row, "kind")?,
        cause: vocab(row, "cause")?,
        actor: row.get("actor")?,
        spec_id: opt_uuid(row, "spec_id")?.map(SpecId),
        draft_id: opt_uuid(row, "draft_id")?.map(SpecDraftId),
        failure: opt_json(row, "failure")?,
        retries: u32_col(row, "retries")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

const SELECT: &str = concat!("SELECT ", issue_intake_columns!(), " FROM issue_intake");

pub fn load_issue_intake(conn: &Connection, id: IssueIntakeId) -> Result<Option<IssueIntake>> {
    one(
        conn,
        &format!("{SELECT} WHERE id = ?1"),
        &[&id.to_string()],
        intake,
    )
}

pub fn issue_intakes(
    conn: &Connection,
    project_id: ProjectId,
    states: &[String],
    page: &Page,
) -> Result<Vec<(IssueIntake, Key)>> {
    let mut sql = format!("{SELECT} WHERE project_id = ?1");
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id.to_string())];
    if !states.is_empty() {
        binds.push(Box::new(
            serde_json::to_string(states).unwrap_or_else(|_| "[]".into()),
        ));
        sql.push_str(&format!(
            " AND state IN (SELECT value FROM json_each(?{}))",
            binds.len()
        ));
    }
    if let Some((key, id)) = page.after {
        binds.push(Box::new(key));
        binds.push(Box::new(id.to_string()));
        let (k, i) = (binds.len() - 1, binds.len());
        sql.push_str(&format!(
            " AND (created_at < ?{k} OR (created_at = ?{k} AND id < ?{i}))"
        ));
    }
    binds.push(Box::new(i64::from(page.limit)));
    sql.push_str(&format!(
        " ORDER BY created_at DESC, id DESC LIMIT ?{}",
        binds.len()
    ));
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let mut q = stmt.query(refs.as_slice())?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        let found = intake(row)?;
        let key: (i64, Uuid) = (found.created_at.0, found.id.0);
        out.push((found, key));
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub struct IntakeLinks {
    pub repo: Option<Repo>,
    pub spec: Option<Spec>,
    pub draft: Option<SpecDraftState>,
}

pub fn intake_links(conn: &Connection, intake: &IssueIntake) -> Result<IntakeLinks> {
    let spec = match intake.spec_id {
        Some(id) => load_spec(conn, id)?,
        None => None,
    };
    let draft = match intake.draft_id {
        Some(id) => one(
            conn,
            "SELECT state FROM spec_draft WHERE id = ?1",
            &[&id.to_string()],
            |row| vocab(row, "state"),
        )?,
        None => None,
    };
    Ok(IntakeLinks {
        repo: load_repo(conn, intake.repo_id)?,
        spec,
        draft,
    })
}

pub fn issue_cursors(conn: &Connection, repo: RepoId) -> Result<Vec<ChangeCursorRow>> {
    let mut stmt = conn.prepare(
        "SELECT number, updated_at, through, seen_ids FROM issue_signal_cursor WHERE repo_id = ?1",
    )?;
    let mut rows = stmt.query([repo.0.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(ChangeCursorRow {
            repo_id: repo,
            number: u64_col(row, "number")?,
            updated_at: ts(row, "updated_at")?,
            through: ts(row, "through")?,
            seen_ids: json(row, "seen_ids")?,
        });
    }
    Ok(out)
}

pub fn issue_scan_cursor(
    conn: &Connection,
    project_id: ProjectId,
    repo_id: RepoId,
) -> Result<Option<Timestamp>> {
    one(
        conn,
        "SELECT through FROM issue_scan_cursor WHERE project_id = ?1 AND repo_id = ?2",
        &[&project_id.to_string(), &repo_id.to_string()],
        |row| ts(row, "through"),
    )
}

pub fn live_issue_intake(
    conn: &Connection,
    repo_id: RepoId,
    number: u64,
) -> Result<Option<IssueIntake>> {
    let number = i64::try_from(number)
        .map_err(|_| StoreError::Corrupt(format!("number out of range: {number}")))?;
    one(
        conn,
        &format!(
            "{SELECT} WHERE repo_id = ?1 AND number = ?2 AND state NOT IN ('dismissed', 'closed', 'done')"
        ),
        &[&repo_id.to_string(), &number],
        intake,
    )
}

pub fn issue_intake_of_spec(conn: &Connection, spec_id: SpecId) -> Result<Option<IssueIntake>> {
    one(
        conn,
        &format!("{SELECT} WHERE spec_id = ?1 ORDER BY created_at DESC, id DESC LIMIT 1"),
        &[&spec_id.to_string()],
        intake,
    )
}

pub fn open_issue_intakes(conn: &Connection, project_id: ProjectId) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM issue_intake WHERE project_id = ?1
           AND state IN ('accepted', 'drafting', 'awaiting_approval', 'failed')",
        [project_id.to_string()],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

pub fn live_intakes_of_repo(
    conn: &Connection,
    project_id: ProjectId,
    repo_id: RepoId,
) -> Result<Vec<IssueIntake>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE project_id = ?1 AND repo_id = ?2
           AND state NOT IN ('dismissed', 'closed', 'done') ORDER BY number"
    ))?;
    let mut rows = stmt.query([project_id.to_string(), repo_id.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(intake(row)?);
    }
    Ok(out)
}

pub fn live_issue_intakes(conn: &Connection) -> Result<Vec<IssueIntake>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT} WHERE state NOT IN ('dismissed', 'closed', 'done') ORDER BY created_at, id"
    ))?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(intake(row)?);
    }
    Ok(out)
}

pub fn comment_posted(conn: &Connection, intake_id: IssueIntakeId, event: &str) -> Result<bool> {
    let found: i64 = conn.query_row(
        "SELECT count(*) FROM event WHERE kind = 'intake.issue.comment_posted'
           AND stream_id = ?1 AND json_extract(payload, '$.event') = ?2",
        rusqlite::params![intake_id.to_string(), event],
        |r| r.get(0),
    )?;
    Ok(found > 0)
}

pub fn issue_closed(conn: &Connection, intake_id: IssueIntakeId) -> Result<bool> {
    let found: i64 = conn.query_row(
        "SELECT count(*) FROM event WHERE kind = 'intake.issue.issue_closed' AND stream_id = ?1",
        rusqlite::params![intake_id.to_string()],
        |r| r.get(0),
    )?;
    Ok(found > 0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct DraftOutcome {
    pub state: SpecDraftState,
    pub spec_id: Option<SpecId>,
    pub failure: Option<saka_core::model::Coded>,
}

pub fn draft_outcome(conn: &Connection, id: SpecDraftId) -> Result<Option<DraftOutcome>> {
    one(
        conn,
        "SELECT state, spec_id, failure FROM spec_draft WHERE id = ?1",
        &[&id.to_string()],
        |row| {
            Ok(DraftOutcome {
                state: vocab(row, "state")?,
                spec_id: opt_uuid(row, "spec_id")?.map(SpecId),
                failure: opt_json(row, "failure")?,
            })
        },
    )
}

pub fn job_exists(conn: &Connection, idempotency_key: &str) -> Result<bool> {
    let found: i64 = conn.query_row(
        "SELECT count(*) FROM job WHERE idempotency_key = ?1",
        [idempotency_key],
        |r| r.get(0),
    )?;
    Ok(found > 0)
}

pub fn issue_intake_of_draft(
    conn: &Connection,
    draft_id: SpecDraftId,
) -> Result<Option<IssueIntake>> {
    one(
        conn,
        &format!("{SELECT} WHERE draft_id = ?1 ORDER BY created_at DESC, id DESC LIMIT 1"),
        &[&draft_id.to_string()],
        intake,
    )
}
