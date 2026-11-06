use rusqlite::types::Value;
use rusqlite::{Connection, Row, Transaction, params, params_from_iter};
use saka_core::identity::{Credential, Device, MAX_FAILED_ATTEMPTS, Pairing, TOUCH_INTERVAL_MS};
use saka_core::ids::{CredentialId, DeviceId, LoginSessionId, PairingId};
use saka_core::time::Timestamp;
use saka_core::vocab::PairingPurpose;

use crate::codec::{uuid, vocab};
use crate::error::Result;
use crate::read::{one, opt_ts, ts};

const PAIRING_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;
const SESSION_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDevice {
    pub device: Device,
    pub token_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCredential {
    pub credential: Credential,
    pub passkey: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialLogin {
    pub id: CredentialId,
    pub at: Timestamp,
    pub passkey: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub id: LoginSessionId,
    pub credential_id: CredentialId,
    pub token_sha256: String,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionHit {
    pub id: LoginSessionId,
    pub credential_id: CredentialId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPairing {
    pub pairing: Pairing,
    pub code_sha256: String,
}

const DEVICE_COLUMNS: &str = "id, name, kind, created_at, last_used_at, revoked_at";

fn device_row(row: &Row<'_>) -> Result<Device> {
    Ok(Device {
        id: DeviceId(uuid(row, "id")?),
        name: row.get("name")?,
        kind: vocab(row, "kind")?,
        created_at: ts(row, "created_at")?,
        last_used_at: opt_ts(row, "last_used_at")?,
        revoked_at: opt_ts(row, "revoked_at")?,
    })
}

pub(crate) fn insert_device(tx: &Transaction<'_>, d: &NewDevice) -> Result<()> {
    tx.execute(
        "INSERT INTO device (id, name, kind, token_sha256, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            d.device.id.to_string(),
            d.device.name,
            d.device.kind.as_str(),
            d.token_sha256,
            d.device.created_at.0,
        ],
    )?;
    Ok(())
}

pub(crate) fn revoke_device(tx: &Transaction<'_>, id: DeviceId, at: Timestamp) -> Result<()> {
    tx.execute(
        "UPDATE device SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL AND kind = 'client'",
        params![id.to_string(), at.0],
    )?;
    Ok(())
}

pub(crate) fn insert_pairing(tx: &Transaction<'_>, p: &NewPairing, now: Timestamp) -> Result<()> {
    tx.execute(
        "DELETE FROM pairing_code WHERE expires_at < ?1",
        [now.0 - PAIRING_RETENTION_MS],
    )?;
    tx.execute(
        "INSERT INTO pairing_code (id, purpose, code_sha256, created_by, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            p.pairing.id.to_string(),
            p.pairing.purpose.as_str(),
            p.code_sha256,
            p.pairing.created_by,
            p.pairing.created_at.0,
            p.pairing.expires_at.0,
        ],
    )?;
    Ok(())
}

pub fn device_by_token(conn: &Connection, token_sha256: &str) -> Result<Option<Device>> {
    one(
        conn,
        &format!(
            "SELECT {DEVICE_COLUMNS} FROM device WHERE token_sha256 = ?1 AND revoked_at IS NULL"
        ),
        &[&token_sha256],
        device_row,
    )
}

pub fn device(conn: &Connection, id: DeviceId) -> Result<Option<Device>> {
    one(
        conn,
        &format!("SELECT {DEVICE_COLUMNS} FROM device WHERE id = ?1"),
        &[&id.to_string()],
        device_row,
    )
}

pub fn local_cli_device(conn: &Connection) -> Result<Option<Device>> {
    one(
        conn,
        &format!("SELECT {DEVICE_COLUMNS} FROM device WHERE kind = 'local_cli'"),
        &[],
        device_row,
    )
}

pub fn active_devices(conn: &Connection) -> Result<Vec<Device>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {DEVICE_COLUMNS} FROM device WHERE revoked_at IS NULL ORDER BY created_at, id"
    ))?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(device_row(row)?);
    }
    Ok(out)
}

pub fn touch_device(tx: &Transaction<'_>, id: DeviceId, now: Timestamp) -> Result<()> {
    tx.execute(
        "UPDATE device SET last_used_at = ?2
         WHERE id = ?1 AND (last_used_at IS NULL OR last_used_at <= ?2 - ?3)",
        params![id.to_string(), now.0, TOUCH_INTERVAL_MS],
    )?;
    Ok(())
}

pub fn reissue_local_cli(
    tx: &Transaction<'_>,
    token_sha256: &str,
    now: Timestamp,
) -> Result<Option<DeviceId>> {
    let mut stmt = tx.prepare(
        "UPDATE device SET token_sha256 = ?1, created_at = ?2, last_used_at = NULL, revoked_at = NULL
         WHERE kind = 'local_cli' RETURNING id",
    )?;
    let mut rows = stmt.query(params![token_sha256, now.0])?;
    match rows.next()? {
        Some(row) => Ok(Some(DeviceId(uuid(row, "id")?))),
        None => Ok(None),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedPairing {
    pub id: PairingId,
    pub purpose: PairingPurpose,
    pub created_by: String,
}

pub fn claim_pairing(
    tx: &Transaction<'_>,
    code_sha256: &str,
    purposes: &[PairingPurpose],
    now: Timestamp,
) -> Result<Option<ClaimedPairing>> {
    check_pairing(tx, code_sha256, purposes, now, true)
}

pub fn peek_pairing(
    tx: &Transaction<'_>,
    code_sha256: &str,
    purposes: &[PairingPurpose],
    now: Timestamp,
) -> Result<Option<ClaimedPairing>> {
    check_pairing(tx, code_sha256, purposes, now, false)
}

fn check_pairing(
    tx: &Transaction<'_>,
    code_sha256: &str,
    purposes: &[PairingPurpose],
    now: Timestamp,
    consume: bool,
) -> Result<Option<ClaimedPairing>> {
    let purpose_marks = |first: usize| {
        (first..first + purposes.len())
            .map(|n| format!("?{n}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let purpose_values = || {
        purposes
            .iter()
            .map(|p| Value::Text(p.as_str().to_string()))
            .collect::<Vec<_>>()
    };
    let claim = if consume {
        format!(
            "UPDATE pairing_code SET used_at = ?1
             WHERE code_sha256 = ?2 AND used_at IS NULL AND expires_at > ?1 AND failed < ?3
               AND purpose IN ({})
             RETURNING id, purpose, created_by",
            purpose_marks(4)
        )
    } else {
        format!(
            "SELECT id, purpose, created_by FROM pairing_code
             WHERE code_sha256 = ?2 AND used_at IS NULL AND expires_at > ?1
               AND failed < ?3 AND purpose IN ({})",
            purpose_marks(4)
        )
    };
    let mut bound = vec![
        Value::Integer(now.0),
        Value::Text(code_sha256.to_string()),
        Value::Integer(MAX_FAILED_ATTEMPTS),
    ];
    bound.extend(purpose_values());
    let claimed = {
        let mut stmt = tx.prepare(&claim)?;
        let mut rows = stmt.query(params_from_iter(bound))?;
        match rows.next()? {
            Some(row) => Some(ClaimedPairing {
                id: PairingId(uuid(row, "id")?),
                purpose: vocab(row, "purpose")?,
                created_by: row.get("created_by")?,
            }),
            None => None,
        }
    };
    if claimed.is_none() {
        let burn = format!(
            "UPDATE pairing_code SET failed = failed + 1
             WHERE used_at IS NULL AND expires_at > ?1 AND purpose IN ({})",
            purpose_marks(2)
        );
        let mut bound = vec![Value::Integer(now.0)];
        bound.extend(purpose_values());
        tx.execute(&burn, params_from_iter(bound))?;
    }
    Ok(claimed)
}

pub fn active_credentials(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT count(*) FROM credential WHERE revoked_at IS NULL",
        [],
        |row| row.get(0),
    )?)
}

pub fn active_credential_list(conn: &Connection) -> Result<Vec<Credential>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, created_at, last_used_at, revoked_at FROM credential
         WHERE revoked_at IS NULL ORDER BY created_at, id",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(credential_row(row)?);
    }
    Ok(out)
}

pub(crate) fn insert_credential(tx: &Transaction<'_>, c: &NewCredential) -> Result<()> {
    tx.execute(
        "INSERT INTO credential (id, name, passkey, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![
            c.credential.id.to_string(),
            c.credential.name,
            c.passkey,
            c.credential.created_at.0,
        ],
    )?;
    Ok(())
}

pub(crate) fn record_login(tx: &Transaction<'_>, l: &CredentialLogin) -> Result<()> {
    tx.execute(
        "UPDATE credential SET last_used_at = ?2, passkey = coalesce(?3, passkey)
         WHERE id = ?1 AND revoked_at IS NULL",
        params![l.id.to_string(), l.at.0, l.passkey],
    )?;
    Ok(())
}

pub(crate) fn revoke_credential(
    tx: &Transaction<'_>,
    id: CredentialId,
    at: Timestamp,
) -> Result<()> {
    tx.execute(
        "UPDATE credential SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        params![id.to_string(), at.0],
    )?;
    tx.execute(
        "UPDATE login_session SET revoked_at = ?2 WHERE credential_id = ?1 AND revoked_at IS NULL",
        params![id.to_string(), at.0],
    )?;
    Ok(())
}

pub(crate) fn insert_session(tx: &Transaction<'_>, s: &NewSession, now: Timestamp) -> Result<()> {
    tx.execute(
        "DELETE FROM login_session WHERE expires_at < ?1",
        [now.0 - SESSION_RETENTION_MS],
    )?;
    tx.execute(
        "INSERT INTO login_session (id, credential_id, token_sha256, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            s.id.to_string(),
            s.credential_id.to_string(),
            s.token_sha256,
            s.created_at.0,
            s.expires_at.0,
        ],
    )?;
    Ok(())
}

pub(crate) fn revoke_session(
    tx: &Transaction<'_>,
    id: LoginSessionId,
    at: Timestamp,
) -> Result<()> {
    tx.execute(
        "UPDATE login_session SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
        params![id.to_string(), at.0],
    )?;
    Ok(())
}

pub fn session_by_token(
    conn: &Connection,
    token_sha256: &str,
    now: Timestamp,
) -> Result<Option<SessionHit>> {
    one(
        conn,
        "SELECT s.id AS id, s.credential_id AS credential_id
         FROM login_session s JOIN credential c ON c.id = s.credential_id
         WHERE s.token_sha256 = ?1 AND s.revoked_at IS NULL AND s.expires_at > ?2
           AND c.revoked_at IS NULL",
        &[&token_sha256, &now.0],
        |row| {
            Ok(SessionHit {
                id: LoginSessionId(uuid(row, "id")?),
                credential_id: CredentialId(uuid(row, "credential_id")?),
            })
        },
    )
}

pub fn credential(conn: &Connection, id: CredentialId) -> Result<Option<Credential>> {
    one(
        conn,
        "SELECT id, name, created_at, last_used_at, revoked_at FROM credential WHERE id = ?1",
        &[&id.to_string()],
        credential_row,
    )
}

fn credential_row(row: &Row<'_>) -> Result<Credential> {
    Ok(Credential {
        id: CredentialId(uuid(row, "id")?),
        name: row.get("name")?,
        created_at: ts(row, "created_at")?,
        last_used_at: opt_ts(row, "last_used_at")?,
        revoked_at: opt_ts(row, "revoked_at")?,
    })
}

pub fn active_passkeys(conn: &Connection) -> Result<Vec<(CredentialId, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, passkey FROM credential WHERE revoked_at IS NULL ORDER BY created_at, id",
    )?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push((CredentialId(uuid(row, "id")?), row.get("passkey")?));
    }
    Ok(out)
}
