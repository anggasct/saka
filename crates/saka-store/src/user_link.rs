use rusqlite::{Connection, Row, Transaction, params};
use saka_core::authorship::{UserLink, UserLinkAuthorization};
use saka_core::ids::{UserLinkAuthorizationId, UserLinkId};
use saka_core::time::Timestamp;

use crate::codec::{i64_of, opt_json, opt_uuid, opt_vocab, to_json, u32_col, u64_col, uuid, vocab};
use crate::error::Result;
use crate::read::one;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedColumns {
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserLinkRow {
    pub link: UserLink,
    pub access: SealedColumns,
    pub refresh: SealedColumns,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AuthorizationRow {
    pub authorization: UserLinkAuthorization,
    pub device: Option<SealedColumns>,
}

fn ts(row: &Row<'_>, col: &str) -> Result<Timestamp> {
    Ok(Timestamp(row.get(col)?))
}

fn link_row(row: &Row<'_>) -> Result<UserLinkRow> {
    let refreshed_at: Option<i64> = row.get("refreshed_at")?;
    Ok(UserLinkRow {
        link: UserLink {
            id: UserLinkId(uuid(row, "id")?),
            owner: row.get("owner")?,
            integration_id: row.get("integration_id")?,
            provider_user_id: u64_col(row, "provider_user_id")?,
            login: row.get("login")?,
            display_name: row.get("display_name")?,
            email: row.get("email")?,
            state: vocab(row, "state")?,
            relink_cause: opt_vocab(row, "relink_cause")?,
            access_expires_at: ts(row, "access_expires_at")?,
            refresh_expires_at: ts(row, "refresh_expires_at")?,
            linked_at: ts(row, "linked_at")?,
            refreshed_at: refreshed_at.map(Timestamp),
            created_at: ts(row, "created_at")?,
            updated_at: ts(row, "updated_at")?,
        },
        access: SealedColumns {
            nonce: row.get("access_nonce")?,
            ciphertext: row.get("access_ciphertext")?,
        },
        refresh: SealedColumns {
            nonce: row.get("refresh_nonce")?,
            ciphertext: row.get("refresh_ciphertext")?,
        },
    })
}

fn authorization_row(row: &Row<'_>) -> Result<AuthorizationRow> {
    let nonce: Option<Vec<u8>> = row.get("device_nonce")?;
    let ciphertext: Option<Vec<u8>> = row.get("device_ciphertext")?;
    Ok(AuthorizationRow {
        authorization: UserLinkAuthorization {
            id: UserLinkAuthorizationId(uuid(row, "id")?),
            owner: row.get("owner")?,
            integration_id: row.get("integration_id")?,
            state: vocab(row, "state")?,
            user_code: row.get("user_code")?,
            verification_uri: row.get("verification_uri")?,
            interval_secs: u32_col(row, "interval_secs")?,
            expires_at: ts(row, "expires_at")?,
            failure: opt_json(row, "failure")?,
            link_id: opt_uuid(row, "link_id")?.map(UserLinkId),
            created_at: ts(row, "created_at")?,
            updated_at: ts(row, "updated_at")?,
        },
        device: match (nonce, ciphertext) {
            (Some(nonce), Some(ciphertext)) => Some(SealedColumns { nonce, ciphertext }),
            _ => None,
        },
    })
}

pub fn user_link(
    conn: &Connection,
    owner: &str,
    integration_id: &str,
) -> Result<Option<UserLinkRow>> {
    one(
        conn,
        "SELECT * FROM user_link WHERE owner = ?1 AND integration_id = ?2",
        &[&owner, &integration_id],
        link_row,
    )
}

pub fn user_link_by_id(conn: &Connection, id: UserLinkId) -> Result<Option<UserLinkRow>> {
    one(
        conn,
        "SELECT * FROM user_link WHERE id = ?1",
        &[&id.0.to_string()],
        link_row,
    )
}

pub fn active_user_links(conn: &Connection) -> Result<Vec<UserLink>> {
    let mut stmt = conn
        .prepare("SELECT * FROM user_link WHERE state = 'active' ORDER BY integration_id, owner")?;
    let mut q = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(link_row(row)?.link);
    }
    Ok(out)
}

pub fn authorization(
    conn: &Connection,
    id: UserLinkAuthorizationId,
) -> Result<Option<AuthorizationRow>> {
    one(
        conn,
        "SELECT * FROM user_link_authorization WHERE id = ?1",
        &[&id.0.to_string()],
        authorization_row,
    )
}

pub fn pending_authorization(
    conn: &Connection,
    owner: &str,
    integration_id: &str,
) -> Result<Option<AuthorizationRow>> {
    one(
        conn,
        "SELECT * FROM user_link_authorization
         WHERE owner = ?1 AND integration_id = ?2 AND state = 'pending'",
        &[&owner, &integration_id],
        authorization_row,
    )
}

pub fn pending_authorizations(conn: &Connection) -> Result<Vec<UserLinkAuthorization>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM user_link_authorization WHERE state = 'pending' ORDER BY created_at, id",
    )?;
    let mut q = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(authorization_row(row)?.authorization);
    }
    Ok(out)
}

pub(crate) fn write_link(tx: &Transaction<'_>, r: &UserLinkRow) -> Result<()> {
    let l = &r.link;
    tx.execute(
        "INSERT INTO user_link (id, owner, integration_id, provider_user_id, login, display_name, email,
           state, relink_cause, access_nonce, access_ciphertext, access_expires_at, refresh_nonce,
           refresh_ciphertext, refresh_expires_at, linked_at, refreshed_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
         ON CONFLICT (id) DO UPDATE SET
           provider_user_id = excluded.provider_user_id, login = excluded.login,
           display_name = excluded.display_name, email = excluded.email, state = excluded.state,
           relink_cause = excluded.relink_cause, access_nonce = excluded.access_nonce,
           access_ciphertext = excluded.access_ciphertext, access_expires_at = excluded.access_expires_at,
           refresh_nonce = excluded.refresh_nonce, refresh_ciphertext = excluded.refresh_ciphertext,
           refresh_expires_at = excluded.refresh_expires_at, linked_at = excluded.linked_at,
           refreshed_at = excluded.refreshed_at, updated_at = excluded.updated_at",
        params![
            l.id.0.to_string(),
            l.owner,
            l.integration_id,
            i64_of(l.provider_user_id)?,
            l.login,
            l.display_name,
            l.email,
            l.state.as_str(),
            l.relink_cause.map(|c| c.as_str()),
            r.access.nonce,
            r.access.ciphertext,
            l.access_expires_at.0,
            r.refresh.nonce,
            r.refresh.ciphertext,
            l.refresh_expires_at.0,
            l.linked_at.0,
            l.refreshed_at.map(|t| t.0),
            l.created_at.0,
            l.updated_at.0,
        ],
    )?;
    Ok(())
}

pub(crate) fn remove_link(tx: &Transaction<'_>, id: UserLinkId) -> Result<()> {
    tx.execute("DELETE FROM user_link WHERE id = ?1", [id.0.to_string()])?;
    Ok(())
}

pub(crate) fn write_authorization(tx: &Transaction<'_>, r: &AuthorizationRow) -> Result<()> {
    let a = &r.authorization;
    let failure = a.failure.as_ref().map(to_json).transpose()?;
    tx.execute(
        "INSERT INTO user_link_authorization (id, owner, integration_id, state, device_nonce,
           device_ciphertext, user_code, verification_uri, interval_secs, expires_at, failure, link_id,
           created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (id) DO UPDATE SET
           state = excluded.state, device_nonce = excluded.device_nonce,
           device_ciphertext = excluded.device_ciphertext, interval_secs = excluded.interval_secs,
           failure = excluded.failure, link_id = excluded.link_id, updated_at = excluded.updated_at",
        params![
            a.id.0.to_string(),
            a.owner,
            a.integration_id,
            a.state.as_str(),
            r.device.as_ref().map(|d| d.nonce.clone()),
            r.device.as_ref().map(|d| d.ciphertext.clone()),
            a.user_code,
            a.verification_uri,
            a.interval_secs,
            a.expires_at.0,
            failure,
            a.link_id.map(|id| id.0.to_string()),
            a.created_at.0,
            a.updated_at.0,
        ],
    )?;
    Ok(())
}
