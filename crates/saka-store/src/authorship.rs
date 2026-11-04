use rusqlite::{Connection, Row, Transaction, params};
use saka_core::authorship::{Identity, ProjectAuthorship, ProjectUserToken};
use saka_core::ids::ProjectId;
use saka_core::time::Timestamp;

use crate::codec::{i64_of, opt_vocab, u64_col, uuid, vocab};
use crate::error::Result;
use crate::read::one;
use crate::user_link::SealedColumns;

const COLUMNS: &str = "project_id, mode, mode_updated_at, token_nonce, token_ciphertext, \
     token_integration_id, token_user_id, token_login, token_name, token_email, token_expires_at, \
     token_set_at, token_invalid_cause, updated_at";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAuthorshipRow {
    pub authorship: ProjectAuthorship,
    pub token: Option<SealedColumns>,
}

fn token(row: &Row<'_>) -> Result<Option<ProjectUserToken>> {
    let Some(integration_id) = row.get::<_, Option<String>>("token_integration_id")? else {
        return Ok(None);
    };
    let expires_at: Option<i64> = row.get("token_expires_at")?;
    Ok(Some(ProjectUserToken {
        integration_id,
        identity: Identity {
            provider_user_id: u64_col(row, "token_user_id")?,
            login: row.get("token_login")?,
            display_name: row.get("token_name")?,
            email: row.get("token_email")?,
        },
        expires_at: expires_at.map(Timestamp),
        set_at: Timestamp(row.get("token_set_at")?),
        invalid_cause: opt_vocab(row, "token_invalid_cause")?,
    }))
}

fn authorship_row(row: &Row<'_>) -> Result<ProjectAuthorshipRow> {
    let nonce: Option<Vec<u8>> = row.get("token_nonce")?;
    let ciphertext: Option<Vec<u8>> = row.get("token_ciphertext")?;
    Ok(ProjectAuthorshipRow {
        authorship: ProjectAuthorship {
            project_id: ProjectId(uuid(row, "project_id")?),
            mode: vocab(row, "mode")?,
            mode_updated_at: Timestamp(row.get("mode_updated_at")?),
            token: token(row)?,
            updated_at: Timestamp(row.get("updated_at")?),
        },
        token: match (nonce, ciphertext) {
            (Some(nonce), Some(ciphertext)) => Some(SealedColumns { nonce, ciphertext }),
            _ => None,
        },
    })
}

pub fn project_authorship_row(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<Option<ProjectAuthorshipRow>> {
    one(
        conn,
        &format!("SELECT {COLUMNS} FROM project_authorship WHERE project_id = ?1"),
        &[&project_id.0.to_string()],
        authorship_row,
    )
}

pub fn project_authorship(
    conn: &Connection,
    project_id: ProjectId,
) -> Result<Option<ProjectAuthorship>> {
    Ok(project_authorship_row(conn, project_id)?.map(|r| r.authorship))
}

pub fn project_authorships(conn: &Connection) -> Result<Vec<ProjectAuthorship>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM project_authorship ORDER BY project_id"
    ))?;
    let mut q = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(authorship_row(row)?.authorship);
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

pub(crate) fn write_authorship(tx: &Transaction<'_>, r: &ProjectAuthorshipRow) -> Result<()> {
    let a = &r.authorship;
    let t = a.token.as_ref();
    let user_id = t.map(|t| i64_of(t.identity.provider_user_id)).transpose()?;
    tx.execute(
        "INSERT INTO project_authorship (project_id, mode, mode_updated_at, token_nonce,
           token_ciphertext, token_integration_id, token_user_id, token_login, token_name,
           token_email, token_expires_at, token_set_at, token_invalid_cause, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (project_id) DO UPDATE SET
           mode = excluded.mode, mode_updated_at = excluded.mode_updated_at,
           token_nonce = excluded.token_nonce, token_ciphertext = excluded.token_ciphertext,
           token_integration_id = excluded.token_integration_id,
           token_user_id = excluded.token_user_id, token_login = excluded.token_login,
           token_name = excluded.token_name, token_email = excluded.token_email,
           token_expires_at = excluded.token_expires_at, token_set_at = excluded.token_set_at,
           token_invalid_cause = excluded.token_invalid_cause, updated_at = excluded.updated_at",
        params![
            a.project_id.0.to_string(),
            a.mode.as_str(),
            a.mode_updated_at.0,
            r.token.as_ref().map(|c| c.nonce.as_slice()),
            r.token.as_ref().map(|c| c.ciphertext.as_slice()),
            t.map(|t| t.integration_id.as_str()),
            user_id,
            t.map(|t| t.identity.login.as_str()),
            t.and_then(|t| t.identity.display_name.as_deref()),
            t.map(|t| t.identity.email.as_str()),
            t.and_then(|t| t.expires_at.map(|at| at.0)),
            t.map(|t| t.set_at.0),
            t.and_then(|t| t.invalid_cause.map(|c| c.as_str())),
            a.updated_at.0,
        ],
    )?;
    Ok(())
}
