use std::fs::File;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use saka_core::time::Timestamp;
use sha2::{Digest, Sha256};

use crate::error::{Result, StoreError};

pub(crate) const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("../migrations/0001_init.sql"))];

pub(crate) fn checksum(sql: &str) -> String {
    let digest = Sha256::digest(sql.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub applied: Vec<i64>,
    pub backup: Option<PathBuf>,
}

pub(crate) fn migrate(
    conn: &mut Connection,
    backups_dir: &Path,
    now: Timestamp,
) -> Result<MigrationReport> {
    migrate_with(conn, MIGRATIONS, backups_dir, now)
}

fn migrate_with(
    conn: &mut Connection,
    migrations: &[(i64, &str)],
    backups_dir: &Path,
    now: Timestamp,
) -> Result<MigrationReport> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migration (
           version    INTEGER PRIMARY KEY,
           checksum   TEXT    NOT NULL,
           applied_at INTEGER NOT NULL
         ) STRICT;",
    )?;
    let applied: Vec<(i64, String)> = {
        let mut stmt =
            conn.prepare("SELECT version, checksum FROM schema_migration ORDER BY version")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let binary = migrations.iter().map(|m| m.0).max().unwrap_or(0);
    if let Some(&(database, _)) = applied.iter().find(|(v, _)| *v > binary) {
        return Err(StoreError::MigrationVersionAhead { database, binary });
    }
    for (version, stored) in &applied {
        let known = migrations
            .iter()
            .find(|m| m.0 == *version)
            .map(|m| checksum(m.1));
        if known.as_deref() != Some(stored.as_str()) {
            return Err(StoreError::MigrationChecksumMismatch { version: *version });
        }
    }
    let pending: Vec<&(i64, &str)> = migrations
        .iter()
        .filter(|m| !applied.iter().any(|a| a.0 == m.0))
        .collect();
    let mut report = MigrationReport {
        applied: Vec::new(),
        backup: None,
    };
    let Some(first) = pending.first() else {
        return Ok(report);
    };
    if !applied.is_empty() {
        report.backup = Some(backup(conn, backups_dir, now, first.0)?);
    }
    for (version, sql) in pending {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migration (version, checksum, applied_at) VALUES (?1, ?2, ?3)",
            params![version, checksum(sql), now.0],
        )?;
        tx.commit()?;
        report.applied.push(*version);
    }
    Ok(report)
}

fn backup(conn: &Connection, dir: &Path, now: Timestamp, version: i64) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let target = dir.join(format!("saka-{}-pre-{version:04}.db", now.0));
    let tmp = dir.join(format!(".saka-{}-pre-{version:04}.db.tmp", now.0));
    if tmp.exists() {
        std::fs::remove_file(&tmp)?;
    }
    let tmp_str = tmp.to_str().ok_or_else(|| {
        StoreError::Corrupt(format!("backup path is not UTF-8: {}", tmp.display()))
    })?;
    conn.execute("VACUUM INTO ?1", params![tmp_str])?;
    File::open(&tmp)?.sync_all()?;
    std::fs::rename(&tmp, &target)?;
    Ok(target)
}

pub fn current_version(conn: &Connection) -> Result<Option<i64>> {
    let exists: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'schema_migration'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Ok(None);
    }
    Ok(
        conn.query_row("SELECT MAX(version) FROM schema_migration", [], |r| {
            r.get(0)
        })?,
    )
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, OpenFlags};

    use super::*;

    const M1: (i64, &str) = (
        1,
        "CREATE TABLE a (x INTEGER) STRICT; INSERT INTO a VALUES (7);",
    );
    const M2: (i64, &str) = (2, "CREATE TABLE b (y INTEGER) STRICT;");

    #[test]
    fn pending_migrations_on_an_existing_database_are_backed_up_first() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("saka.db");
        let backups = dir.path().join("backups");
        let mut conn = Connection::open(&db).unwrap();

        let first = migrate_with(&mut conn, &[M1], &backups, Timestamp(10)).unwrap();
        assert_eq!(
            first,
            MigrationReport {
                applied: vec![1],
                backup: None
            }
        );

        let second = migrate_with(&mut conn, &[M1, M2], &backups, Timestamp(20)).unwrap();
        assert_eq!(second.applied, vec![2]);
        let backup = second.backup.unwrap();
        assert_eq!(backup.file_name().unwrap(), "saka-20-pre-0002.db");

        let ro = Connection::open_with_flags(&backup, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let x: i64 = ro.query_row("SELECT x FROM a", [], |r| r.get(0)).unwrap();
        assert_eq!(x, 7);
        let has_b: i64 = ro
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'b'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_b, 0, "the backup is taken before the pending migration");
        assert_eq!(current_version(&conn).unwrap(), Some(2));

        let again = migrate_with(&mut conn, &[M1, M2], &backups, Timestamp(30)).unwrap();
        assert_eq!(
            again,
            MigrationReport {
                applied: vec![],
                backup: None
            }
        );
    }

    #[test]
    fn changed_or_unknown_migrations_stop_startup() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let mut conn = Connection::open(dir.path().join("saka.db")).unwrap();
        migrate_with(&mut conn, &[M1, M2], &backups, Timestamp(1)).unwrap();

        let edited = (1, "CREATE TABLE a (x INTEGER, z INTEGER) STRICT;");
        assert!(matches!(
            migrate_with(&mut conn, &[edited, M2], &backups, Timestamp(2)),
            Err(StoreError::MigrationChecksumMismatch { version: 1 })
        ));
        assert!(matches!(
            migrate_with(&mut conn, &[M1], &backups, Timestamp(2)),
            Err(StoreError::MigrationVersionAhead {
                database: 2,
                binary: 1
            })
        ));
    }
}
