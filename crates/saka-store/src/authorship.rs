use rusqlite::{Connection, Row, Transaction, params};
use saka_core::authorship::ProjectAuthorship;
use saka_core::ids::ProjectId;
use saka_core::time::Timestamp;

use crate::codec::{uuid, vocab};
use crate::error::Result;
use crate::read::one;

const COLUMNS: &str = "project_id, mode, mode_updated_at, updated_at";

fn authorship_row(row: &Row<'_>) -> Result<ProjectAuthorship> {
    Ok(ProjectAuthorship {
        project_id: ProjectId(uuid(row, "project_id")?),
        mode: vocab(row, "mode")?,
        mode_updated_at: Timestamp(row.get("mode_updated_at")?),
        updated_at: Timestamp(row.get("updated_at")?),
    })
}

pub fn project_authorship(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<Option<ProjectAuthorship>> {
    one(
        conn,
        &format!("SELECT {COLUMNS} FROM project_authorship WHERE project_id = ?1"),
        &[&project_id.0.to_string()],
        authorship_row,
    )
}

pub fn project_authorships(conn: &Connection) -> Result<Vec<ProjectAuthorship>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM project_authorship ORDER BY project_id"
    ))?;
    let mut q = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(authorship_row(row)?);
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectName {
    pub project_id: ProjectId,
    pub name: String,
}

pub fn user_mode_projects_on(conn: &Connection, integration_id: &str) -> Result<Vec<ProjectName>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT p.id AS project_id, p.name AS name
         FROM project_authorship a
         JOIN project p ON p.id = a.project_id
         JOIN project_repo pr ON pr.project_id = p.id
         JOIN repo r ON r.id = pr.repo_id
         WHERE a.mode = 'user' AND r.integration_id = ?1
         ORDER BY p.name, p.id",
    )?;
    let mut q = stmt.query([integration_id])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(ProjectName {
            project_id: ProjectId(uuid(row, "project_id")?),
            name: row.get("name")?,
        });
    }
    Ok(out)
}

pub(crate) fn write_authorship(tx: &Transaction<'_>, a: &ProjectAuthorship) -> Result<()> {
    tx.execute(
        "INSERT INTO project_authorship (project_id, mode, mode_updated_at, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (project_id) DO UPDATE SET
           mode = excluded.mode, mode_updated_at = excluded.mode_updated_at,
           updated_at = excluded.updated_at",
        params![
            a.project_id.0.to_string(),
            a.mode.as_str(),
            a.mode_updated_at.0,
            a.updated_at.0,
        ],
    )?;
    Ok(())
}
