use std::fs::File;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use saka_core::time::Timestamp;
use sha2::{Digest, Sha256};

use crate::error::{Result, StoreError};

pub(crate) const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../migrations/0001_init.sql")),
    (2, include_str!("../migrations/0002_spec_model.sql")),
    (3, include_str!("../migrations/0003_spec_approval.sql")),
    (
        4,
        include_str!("../migrations/0004_environment_teardown_failed.sql"),
    ),
    (5, include_str!("../migrations/0005_agent_runtime.sql")),
    (6, include_str!("../migrations/0006_task_environment.sql")),
    (7, include_str!("../migrations/0007_toolchain_blockers.sql")),
    (8, include_str!("../migrations/0008_secrets.sql")),
    (9, include_str!("../migrations/0009_spec_context.sql")),
    (10, include_str!("../migrations/0010_webhook_delivery.sql")),
    (
        11,
        include_str!("../migrations/0011_run_owner_and_spec_drafts.sql"),
    ),
    (
        12,
        include_str!("../migrations/0012_multi_repo_bindings.sql"),
    ),
    (13, include_str!("../migrations/0013_path_scope.sql")),
    (14, include_str!("../migrations/0014_user_link.sql")),
    (
        15,
        include_str!("../migrations/0015_project_authorship.sql"),
    ),
    (
        16,
        include_str!("../migrations/0016_user_token_blocker.sql"),
    ),
    (17, include_str!("../migrations/0017_environment_docs.sql")),
    (
        18,
        include_str!("../migrations/0018_docs_change_requests.sql"),
    ),
    (19, include_str!("../migrations/0019_project_workflow.sql")),
    (20, include_str!("../migrations/0020_hook_run.sql")),
    (21, include_str!("../migrations/0021_external_changes.sql")),
    (
        22,
        include_str!("../migrations/0022_taken_over_resolution.sql"),
    ),
    (23, include_str!("../migrations/0023_review.sql")),
    (
        24,
        include_str!("../migrations/0024_branch_moved_blocker.sql"),
    ),
    (25, include_str!("../migrations/0025_merge_queue.sql")),
];

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
    // Table rebuilds need foreign keys off, and the pragma is ignored inside a transaction.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let applied = apply(conn, &pending, now, &mut report);
    conn.pragma_update(None, "foreign_keys", "ON")?;
    applied?;
    Ok(report)
}

fn apply(
    conn: &mut Connection,
    pending: &[&(i64, &str)],
    now: Timestamp,
    report: &mut MigrationReport,
) -> Result<()> {
    for (version, sql) in pending {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        let broken: Option<String> = tx
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()?;
        if let Some(table) = broken {
            return Err(StoreError::MigrationForeignKeys {
                version: *version,
                table,
            });
        }
        tx.execute(
            "INSERT INTO schema_migration (version, checksum, applied_at) VALUES (?1, ?2, ?3)",
            params![version, checksum(sql), now.0],
        )?;
        tx.commit()?;
        report.applied.push(*version);
    }
    Ok(())
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
    fn the_status_reads_the_applied_version_without_migrating_or_creating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("saka.db");
        let latest = crate::latest_migration();
        let state = |applied| saka_core::diagnostics::MigrationState { applied, latest };
        assert_eq!(crate::migration_status(&path).unwrap(), state(0));
        assert!(!path.exists());
        let mut conn = Connection::open(&path).unwrap();
        migrate_with(
            &mut conn,
            &MIGRATIONS[..2],
            &dir.path().join("b"),
            Timestamp(1),
        )
        .unwrap();
        drop(conn);
        assert_eq!(crate::migration_status(&path).unwrap(), state(2));
        assert_eq!(crate::migration_status(&path).unwrap(), state(2));
        let conn = Connection::open(&path).unwrap();
        assert_eq!(current_version(&conn).unwrap(), Some(2));
    }

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
    fn a_migration_that_breaks_a_foreign_key_is_rolled_back() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let mut conn = Connection::open(dir.path().join("saka.db")).unwrap();
        let parent = (
            1,
            "CREATE TABLE p (id INTEGER PRIMARY KEY) STRICT; CREATE TABLE c (p INTEGER REFERENCES p(id)) STRICT; INSERT INTO p VALUES (1); INSERT INTO c VALUES (1);",
        );
        migrate_with(&mut conn, &[parent], &backups, Timestamp(1)).unwrap();
        let orphan = (2, "DELETE FROM p;");
        assert!(matches!(
            migrate_with(&mut conn, &[parent, orphan], &backups, Timestamp(2)),
            Err(StoreError::MigrationForeignKeys { version: 2, .. })
        ));
        let parents: i64 = conn
            .query_row("SELECT COUNT(*) FROM p", [], |r| r.get(0))
            .unwrap();
        assert_eq!(parents, 1);
        assert_eq!(current_version(&conn).unwrap(), Some(1));
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    const V1_DATA: &str = "
        INSERT INTO repo VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
        INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
          VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
        INSERT INTO spec (id, project_id, path, slug, title, state, priority, latest_revision_id, approved_revision_id,
            approved_at, created_at, updated_at)
          VALUES ('s1', 'p1', 'docs/specs/t.md', 't', 'T', 'approved', 0, 'v1', 'v1', 2, 1, 2);
        INSERT INTO spec_revision VALUES ('v1', 's1', 'p1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'sha256:h',
          X'00', 1, '[]', '{}', 1, 1);
        INSERT INTO work_item (id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work,
            repo_id, branch, state, version, created_at, updated_at)
          VALUES ('w1', 'p1', 'spec_step', 's1', 'v1', 'S1', 1, 't/s1', 'a', 'r1', 'saka/t/s1', 'done', 3, 2, 2),
                 ('w2', 'p1', 'spec_step', 's1', 'v1', 'S2', 2, 't/s2', 'b', 'r1', 'saka/t/s2', 'queued', 1, 2, 2),
                 ('w3', 'p1', 'spec_step', 's1', 'v1', 'S3', 3, 't/s3', 'c', 'r1', 'saka/t/s3', 'queued', 1, 2, 2);
    ";

    #[test]
    fn the_spec_model_migration_keeps_rows_and_backfills_the_step_chain() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let mut conn = Connection::open(dir.path().join("saka.db")).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..1], &backups, Timestamp(1)).unwrap();
        conn.execute_batch(V1_DATA).unwrap();

        let report = migrate_with(&mut conn, &MIGRATIONS[..2], &backups, Timestamp(2)).unwrap();
        assert_eq!(report.applied, vec![2]);
        assert!(report.backup.is_some());

        let rows: Vec<(String, String, i64)> = {
            let mut stmt = conn
                .prepare("SELECT id, spec_id, parser_version FROM spec_revision ORDER BY id")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        assert_eq!(rows, vec![("v1".to_string(), "s1".to_string(), 1)]);
        let items: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM work_item WHERE spec_revision_id = 'v1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(items, 3);

        let chain: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare("SELECT work_item_id, depends_on_work_item_id FROM work_item_dependency ORDER BY 1")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            chain,
            vec![("w2".into(), "w1".into()), ("w3".into(), "w2".into())]
        );

        conn.execute(
            "INSERT INTO spec_revision VALUES ('v2', 's1', 'p1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'sha256:h',
               X'00', 1, '[]', '{}', 2, 3)",
            [],
        )
        .unwrap();
        let duplicate = conn.execute(
            "INSERT INTO spec_revision VALUES ('v3', 's1', 'p1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'sha256:h',
               X'00', 1, '[]', '{}', 2, 4)",
            [],
        );
        assert!(duplicate.is_err());

        let broken: Option<String> = conn
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert_eq!(broken, None);
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
        let orphan = conn.execute(
            "UPDATE work_item SET spec_revision_id = 'missing' WHERE id = 'w1'",
            [],
        );
        assert!(orphan.is_err(), "work_item still references spec_revision");
    }

    const V2_DATA: &str = "
        INSERT INTO repo VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
        INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
          VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
        INSERT INTO spec (id, project_id, path, slug, title, state, priority, latest_revision_id, approved_revision_id,
            approved_at, shipped_at, created_at, updated_at)
          VALUES ('s-draft', 'p1', 'docs/d.md', 'd', 'D', 'draft', 0, 'v-draft', NULL, NULL, NULL, 1, 1),
                 ('s-ready', 'p1', 'docs/r.md', 'r', 'R', 'ready', 0, 'v-ready', NULL, NULL, NULL, 1, 1),
                 ('s-approved', 'p1', 'docs/a.md', 'a', 'A', 'approved', 5, 'v-approved', 'v-approved', 2, NULL, 1, 2),
                 ('s-shipped', 'p1', 'docs/s.md', 's', 'S', 'shipped', 0, 'v-shipped', 'v-shipped', 2, 3, 1, 3),
                 ('s-withdrawn', 'p1', 'docs/w.md', 'w', 'W', 'withdrawn', 0, 'v-withdrawn', NULL, NULL, NULL, 1, 2);
        INSERT INTO spec_revision VALUES
          ('v-draft', 's-draft', 'p1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'h1', X'00', 0, '[]', '{}', 2, 1),
          ('v-ready', 's-ready', 'p1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'h2', X'00', 1, '[]', '{}', 2, 1),
          ('v-approved', 's-approved', 'p1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'h3', X'00', 1, '[]', '{}', 2, 1),
          ('v-shipped', 's-shipped', 'p1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'h4', X'00', 1, '[]', '{}', 2, 1),
          ('v-withdrawn', 's-withdrawn', 'p1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'h5', X'00', 1, '[]', '{}', 2, 1);
        INSERT INTO work_item (id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work,
            repo_id, branch, state, stage, stage_phase, priority, version, created_at, updated_at)
          VALUES ('w1', 'p1', 'spec_step', 's-approved', 'v-approved', 'S1', 1, 'a/s1', 'a', 'r1', 'saka/a/s1', 'queued', NULL, NULL, 5, 1, 2, 2),
                 ('w2', 'p1', 'spec_step', 's-approved', 'v-approved', 'S2', 2, 'a/s2', 'b', 'r1', 'saka/a/s2', 'awaiting_start', NULL, NULL, 5, 1, 2, 2),
                 ('w3', 'p1', 'spec_step', 's-approved', 'v-approved', 'S3', 3, 'a/s3', 'c', 'r1', 'saka/a/s3', 'admitted', NULL, NULL, 5, 2, 2, 2),
                 ('w4', 'p1', 'spec_step', 's-approved', 'v-approved', 'S4', 4, 'a/s4', 'd', 'r1', 'saka/a/s4', 'in_stage', 'dev', 'running_session', 5, 3, 2, 2),
                 ('w5', 'p1', 'spec_step', 's-approved', 'v-approved', 'S5', 5, 'a/s5', 'e', 'r1', 'saka/a/s5', 'blocked', NULL, NULL, 5, 4, 2, 2),
                 ('w6', 'p1', 'spec_step', 's-approved', 'v-approved', 'S6', 6, 'a/s6', 'f', 'r1', 'saka/a/s6', 'taken_over', NULL, NULL, 5, 4, 2, 2),
                 ('w7', 'p1', 'spec_step', 's-approved', 'v-approved', 'S7', 7, 'a/s7', 'g', 'r1', 'saka/a/s7', 'cancelled', NULL, NULL, 5, 4, 2, 2),
                 ('w8', 'p1', 'spec_step', 's-shipped', 'v-shipped', 'S1', 1, 's/s1', 'h', 'r1', 'saka/s/s1', 'done', NULL, NULL, 0, 9, 2, 3);
        INSERT INTO work_item_dependency VALUES ('w2', 'w1', 'p1', 2);
        INSERT INTO stage_run (id, work_item_id, project_id, stage, attempt, state, trigger, entry_phase, created_at, updated_at)
          VALUES ('run1', 'w4', 'p1', 'dev', 1, 'running', 'auto', 'preparing_environment', 2, 2);
        INSERT INTO environment VALUES ('e1', 'p1', 'w4', 'r1', '/root', '/root/wt', 'saka/a/s4',
          'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'ready', 2, 2);
        INSERT INTO session (id, project_id, work_item_id, stage_run_id, environment_id, cli, account, state, runner_unit,
            socket_path, log_path, created_at, updated_at)
          VALUES ('x1', 'p1', 'w4', 'run1', 'e1', 'claude', 'default', 'running', 'unit', '/s', '/l', 2, 2);
        INSERT INTO permission_request (id, project_id, work_item_id, session_id, cli_request_id, tool, input, summary,
            state, requested_at)
          VALUES ('q1', 'p1', 'w4', 'x1', 'c1', 'Bash', '{}', 'ls', 'pending', 2);
        INSERT INTO change_request VALUES ('cr1', 'p1', 'w8', 'r1', 1, 'https://x/1', 'saka/s/s1', 'main',
          'cccccccccccccccccccccccccccccccccccccccc', 'merged', 'dddddddddddddddddddddddddddddddddddddddd', 3, 3, 2, 3);
    ";

    fn ids(conn: &Connection, sql: &str) -> Vec<String> {
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn the_spec_approval_migration_keeps_rows_and_widens_the_states() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let mut conn = Connection::open(dir.path().join("saka.db")).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..2], &backups, Timestamp(1)).unwrap();
        conn.execute_batch(V2_DATA).unwrap();
        let specs_before = ids(
            &conn,
            "SELECT id || ':' || state || ':' || ifnull(approved_revision_id, '-') FROM spec ORDER BY id",
        );
        let items_before = ids(
            &conn,
            "SELECT id || ':' || state || ':' || task_key FROM work_item ORDER BY id",
        );

        let report = migrate_with(&mut conn, &MIGRATIONS[..3], &backups, Timestamp(2)).unwrap();
        assert_eq!(report.applied, vec![3]);
        assert_eq!(
            ids(
                &conn,
                "SELECT id || ':' || state || ':' || ifnull(approved_revision_id, '-') FROM spec ORDER BY id"
            ),
            specs_before
        );
        assert_eq!(
            ids(
                &conn,
                "SELECT id || ':' || state || ':' || task_key FROM work_item ORDER BY id"
            ),
            items_before
        );
        for (table, expected) in [
            ("stage_run", 1),
            ("environment", 1),
            ("session", 1),
            ("permission_request", 1),
            ("change_request", 1),
            ("work_item_dependency", 1),
        ] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, expected, "{table}");
        }
        assert_eq!(
            ids(
                &conn,
                "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name IN ('spec', 'work_item') AND sql IS NOT NULL ORDER BY name"
            ),
            vec![
                "spec_project_state_idx",
                "work_item_admission_idx",
                "work_item_project_state_idx",
                "work_item_spec_idx"
            ]
        );
        let broken: Option<String> = conn
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert_eq!(broken, None);

        conn.execute(
            "UPDATE work_item SET state = 'superseded' WHERE id = 'w1'",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE spec SET state = 'withdrawn', approved_revision_id = 'v-approved' WHERE id = 's-approved'",
            [],
        )
        .unwrap();
        let refused = [
            "UPDATE work_item SET state = 'paused' WHERE id = 'w2'",
            "UPDATE spec SET state = 'mystery' WHERE id = 's-draft'",
            "UPDATE spec SET state = 'approved' WHERE id = 's-ready'",
            "UPDATE spec SET approved_revision_id = 'v-ready' WHERE id = 's-ready'",
            "UPDATE spec SET approved_revision_id = 'v-draft' WHERE id = 's-draft'",
            "UPDATE spec SET approved_revision_id = NULL WHERE id = 's-shipped'",
            "INSERT INTO stage_run (id, work_item_id, project_id, stage, attempt, state, trigger, entry_phase, created_at, updated_at)
               VALUES ('run2', 'missing', 'p1', 'dev', 1, 'pending', 'auto', 'preparing_environment', 2, 2)",
            "INSERT INTO work_item_dependency VALUES ('w1', 'missing', 'p1', 2)",
            "UPDATE environment SET work_item_id = 'missing'",
            "UPDATE change_request SET work_item_id = 'missing'",
        ];
        for sql in refused {
            assert!(conn.execute(sql, []).is_err(), "{sql}");
        }
    }

    #[test]
    fn the_teardown_failed_migration_keeps_environments_and_accepts_the_new_state() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let mut conn = Connection::open(dir.path().join("saka.db")).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..2], &backups, Timestamp(1)).unwrap();
        conn.execute_batch(V2_DATA).unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..3], &backups, Timestamp(2)).unwrap();
        conn.execute_batch(
            "INSERT INTO environment VALUES
               ('e2', 'p1', 'w5', 'r1', '/root2', '/root2/wt', 'saka/a/s5', NULL, 'preparing', 3, 3),
               ('e3', 'p1', 'w7', 'r1', '/root3', '/root3/wt', 'saka/a/s7', NULL, 'failed', 3, 3),
               ('e4', 'p1', 'w8', 'r1', '/root4', '/root4/wt', 'saka/s/s1',
                'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee', 'torn_down', 3, 4);",
        )
        .unwrap();
        let columns = "SELECT id || ':' || work_item_id || ':' || state || ':' || root_path || ':' \
                       || ifnull(base_sha, '-') || ':' || created_at || ':' || updated_at \
                       FROM environment ORDER BY id";
        let before = ids(&conn, columns);
        assert_eq!(before.len(), 4);

        let report = migrate_with(&mut conn, &MIGRATIONS[..4], &backups, Timestamp(3)).unwrap();
        assert_eq!(report.applied, vec![4]);
        assert_eq!(ids(&conn, columns), before);
        assert_eq!(
            ids(
                &conn,
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND tbl_name = 'environment' AND sql IS NOT NULL"
            ),
            vec![
                "CREATE UNIQUE INDEX environment_one_live ON environment (work_item_id) WHERE state IN ('preparing','ready')"
            ]
        );
        let broken: Option<String> = conn
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert_eq!(broken, None);

        conn.execute(
            "UPDATE environment SET state = 'teardown_failed' WHERE id = 'e4'",
            [],
        )
        .unwrap();
        let refused = [
            "UPDATE environment SET state = 'mystery' WHERE id = 'e3'",
            "UPDATE environment SET state = 'ready' WHERE id = 'e3'",
            "UPDATE environment SET work_item_id = 'missing' WHERE id = 'e3'",
            "UPDATE session SET environment_id = 'missing' WHERE id = 'x1'",
            "INSERT INTO environment VALUES ('e5', 'p1', 'w4', 'r1', '/r5', '/r5/wt', 'saka/a/s4', NULL, 'preparing', 5, 5)",
        ];
        for sql in refused {
            assert!(conn.execute(sql, []).is_err(), "{sql}");
        }
    }

    #[test]
    fn the_agent_runtime_migration_keeps_sessions_and_adds_the_runtime_tables() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let mut conn = Connection::open(dir.path().join("saka.db")).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..2], &backups, Timestamp(1)).unwrap();
        conn.execute_batch(V2_DATA).unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..4], &backups, Timestamp(2)).unwrap();
        conn.execute_batch(
            "INSERT INTO session (id, project_id, work_item_id, stage_run_id, environment_id, cli, cli_session_id,
                 account, state, runner_unit, socket_path, log_path, protocol_version, last_offset, turns, running_ms,
                 cost_usd_micros, end_reason, created_at, updated_at)
               VALUES ('x2', 'p1', 'w4', 'run1', 'e1', 'claude', 'cli-2', 'default', 'lost', 'unit2', '/s2', '/l2', 1,
                 41, 2, 9000, 120, '{\"code\":\"session_lost\",\"params\":{}}', 3, 4);
             UPDATE session SET state = 'idle', updated_at = 7 WHERE id = 'x1';",
        )
        .unwrap();
        let sessions = "SELECT id || ':' || state || ':' || ifnull(cli_session_id, '-') || ':' || last_offset || ':' \
                        || turns || ':' || running_ms || ':' || ifnull(cost_usd_micros, '-') || ':' \
                        || ifnull(end_reason, '-') || ':' || created_at || ':' || updated_at FROM session ORDER BY id";
        let runs = "SELECT id || ':' || state || ':' || attempt FROM stage_run ORDER BY id";
        let permissions =
            "SELECT id || ':' || session_id || ':' || state FROM permission_request ORDER BY id";
        let before = (
            ids(&conn, sessions),
            ids(&conn, runs),
            ids(&conn, permissions),
        );

        let report = migrate_with(&mut conn, &MIGRATIONS[..5], &backups, Timestamp(3)).unwrap();
        assert_eq!(report.applied, vec![5]);
        assert!(report.backup.is_some());
        assert_eq!(
            (
                ids(&conn, sessions),
                ids(&conn, runs),
                ids(&conn, permissions)
            ),
            before
        );
        assert_eq!(
            ids(
                &conn,
                "SELECT id || ':' || ifnull(idle_since, '-') || ':' || ifnull(resumed_from_session_id, '-') || ':' \
                 || ifnull(resume_cause, '-') || ':' || ifnull(interrupt_requested, '-') FROM session ORDER BY id"
            ),
            vec!["x1:7:-:-:-", "x2:-:-:-:-"]
        );
        assert_eq!(
            ids(&conn, "SELECT budget || budget_warnings FROM stage_run"),
            vec!["{}[]"]
        );
        let protocol: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('session') WHERE name = 'protocol_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(protocol, 0);
        assert_eq!(
            ids(
                &conn,
                "SELECT name FROM sqlite_master WHERE type = 'index' AND sql IS NOT NULL
                   AND tbl_name IN ('session', 'session_activity', 'permission_rule', 'usage_record', 'account_quota')
                 ORDER BY name"
            ),
            vec![
                "permission_rule_active",
                "session_one_live",
                "session_stage_run_idx",
                "session_state_idx",
                "session_work_item_idx",
                "usage_record_account_idx",
                "usage_record_stage_run_idx",
                "usage_record_work_item_idx",
            ]
        );
        let broken: Option<String> = conn
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert_eq!(broken, None);

        conn.execute_batch(
            "UPDATE session SET state = 'resumed', idle_since = NULL WHERE id = 'x2';
             INSERT INTO session (id, project_id, work_item_id, stage_run_id, environment_id, cli, account, state,
                 runner_unit, socket_path, log_path, resumed_from_session_id, resume_cause, created_at, updated_at)
               VALUES ('x3', 'p1', 'w5', 'run1', 'e1', 'claude', 'default', 'ended', 'unit3', '/s3', '/l3', 'x2',
                 'lost', 5, 5);
             INSERT INTO session_activity (session_id, \"offset\", work_item_id, project_id, at, kind, summary, data)
               VALUES ('x3', 1, 'w5', 'p1', 5, 'action', 'Bash: ls', '{\"tool\":\"Bash\"}');
             INSERT INTO permission_rule (id, project_id, effect, tool, command, permission_request_id, created_by,
                 created_at)
               VALUES ('pr1', 'p1', 'allow', 'Bash', 'npm install', 'q1', 'human:operator:local', 5);
             INSERT INTO usage_record VALUES ('u1', 'p1', 'w4', 'run1', 'x1', 'claude:default', 1, 'success', 10, 5,
               120, 9000, 5);
             INSERT INTO account_quota VALUES ('claude:default', 'seven_day', 0.42, 'allowed', 99, 5);",
        )
        .unwrap();
        let refused = [
            "UPDATE session SET state = 'idle' WHERE id = 'x3'",
            "UPDATE session SET idle_since = 9 WHERE id = 'x3'",
            "UPDATE session SET resume_cause = NULL WHERE id = 'x3'",
            "UPDATE session SET resume_cause = 'whim' WHERE id = 'x3'",
            "UPDATE session SET interrupt_requested = 'nobody' WHERE id = 'x3'",
            "UPDATE session SET resumed_from_session_id = 'missing' WHERE id = 'x3'",
            "INSERT INTO session (id, project_id, work_item_id, stage_run_id, environment_id, cli, account, state,
               runner_unit, socket_path, log_path, created_at, updated_at)
             VALUES ('x4', 'p1', 'w4', 'run1', 'e1', 'claude', 'default', 'starting', 'u', '/s', '/l', 6, 6)",
            "UPDATE stage_run SET budget = 'nope' WHERE id = 'run1'",
            "INSERT INTO session_activity (session_id, \"offset\", work_item_id, project_id, at, kind, summary, data)
               VALUES ('x3', 1, 'w5', 'p1', 6, 'action', 'again', '{}')",
            "INSERT INTO session_activity (session_id, \"offset\", work_item_id, project_id, at, kind, summary, data)
               VALUES ('x3', 2, 'w5', 'p1', 6, 'gossip', 'x', '{}')",
            "INSERT INTO permission_rule (id, project_id, effect, tool, command, permission_request_id, created_by,
               created_at) VALUES ('pr2', 'p1', 'allow', 'Bash', 'npm install', 'q1', 'human:operator:local', 6)",
            "INSERT INTO permission_rule (id, project_id, effect, tool, permission_request_id, created_by, created_at)
               VALUES ('pr3', 'p1', 'allow', 'Bash', 'q1', 'human:operator:local', 6)",
            "INSERT INTO permission_rule (id, project_id, effect, tool, permission_request_id, created_by, created_at)
               VALUES ('pr4', 'p1', 'deny', 'Edit', 'q1', 'human:operator:local', 6)",
            "UPDATE permission_rule SET revoked_at = 7 WHERE id = 'pr1'",
            "INSERT INTO usage_record VALUES ('u2', 'p1', 'w4', 'run1', 'x1', 'claude:default', 1, 'success', NULL,
               NULL, NULL, 1, 6)",
            "INSERT INTO usage_record VALUES ('u3', 'p1', 'w4', 'run1', 'x1', 'claude:default', 2, 'success', -1,
               NULL, NULL, 1, 6)",
            "INSERT INTO account_quota VALUES ('claude:default', 'seven_day', 0.5, 'allowed', NULL, 6)",
        ];
        for sql in refused {
            assert!(conn.execute(sql, []).is_err(), "{sql}");
        }
        conn.execute(
            "UPDATE permission_rule SET revoked_at = 7, revoke_reason = 'too broad' WHERE id = 'pr1'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO permission_rule (id, project_id, effect, tool, command, permission_request_id, created_by,
               created_at) VALUES ('pr5', 'p1', 'allow', 'Bash', 'npm install', 'q1', 'human:operator:local', 8)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn the_task_environment_migration_keeps_rows_and_guards_port_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let mut conn = Connection::open(dir.path().join("saka.db")).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..2], &backups, Timestamp(1)).unwrap();
        conn.execute_batch(V2_DATA).unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..5], &backups, Timestamp(2)).unwrap();
        conn.execute_batch(
            "INSERT INTO blocker VALUES ('b1', 'w5', NULL, 'p1', 'env_setup_failed', '{}', 'open', NULL, 3, NULL),
               ('b2', 'w5', NULL, 'p1', 'ci_red', '{\"checks\":[]}', 'resolved', 'retried', 2, 3);",
        )
        .unwrap();
        let blockers = "SELECT id || ':' || code || ':' || params || ':' || state || ':' \
                        || ifnull(resolution, '-') FROM blocker ORDER BY id";
        let environments = "SELECT id || ':' || state || ':' || ifnull(base_sha, '-') FROM environment ORDER BY id";
        let before = (ids(&conn, blockers), ids(&conn, environments));

        let report = migrate_with(&mut conn, &MIGRATIONS[..6], &backups, Timestamp(3)).unwrap();
        assert_eq!(report.applied, vec![6]);
        assert_eq!((ids(&conn, blockers), ids(&conn, environments)), before);
        assert_eq!(
            ids(
                &conn,
                "SELECT ifnull(port_start, '-') || ':' || toolchains_json FROM environment"
            ),
            vec!["-:[]"]
        );
        let broken: Option<String> = conn
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert_eq!(broken, None);

        conn.execute_batch(
            "UPDATE blocker SET state = 'resolved', resolution = 'retried', resolved_at = 4 WHERE id = 'b1';
             INSERT INTO blocker VALUES ('b3', 'w5', NULL, 'p1', 'env_recipe_invalid', '{}', 'open', NULL, 4, NULL);
             UPDATE environment SET port_start = 41000, port_count = 10 WHERE id = 'e1';",
        )
        .unwrap();
        let refused = [
            "INSERT INTO blocker VALUES ('b4', 'w5', NULL, 'p1', 'env_recipe_invalid', '{}', 'open', NULL, 5, NULL)",
            "INSERT INTO blocker VALUES ('b5', 'w4', NULL, 'p1', 'mystery', '{}', 'open', NULL, 5, NULL)",
            "UPDATE environment SET port_start = 80 WHERE id = 'e1'",
            "UPDATE environment SET port_count = 0 WHERE id = 'e1'",
            "UPDATE environment SET recipe_sha256 = 'abc' WHERE id = 'e1'",
            "INSERT INTO environment (id, project_id, work_item_id, repo_id, root_path, worktree_path, branch,
               state, created_at, updated_at, port_start, port_count)
             VALUES ('e9', 'p1', 'w3', 'r1', '/r9', '/r9/wt', 'saka/a/s3', 'preparing', 5, 5, 41000, 10)",
        ];
        for sql in refused {
            assert!(conn.execute(sql, []).is_err(), "{sql}");
        }
    }

    #[test]
    fn the_toolchain_migration_keeps_blockers_and_accepts_the_toolchain_codes() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let mut conn = Connection::open(dir.path().join("saka.db")).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..2], &backups, Timestamp(1)).unwrap();
        conn.execute_batch(V2_DATA).unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..6], &backups, Timestamp(2)).unwrap();
        conn.execute_batch(
            "INSERT INTO blocker VALUES ('b1', 'w5', NULL, 'p1', 'env_recipe_invalid', '{}', 'open', NULL, 3, NULL),
               ('b2', 'w5', NULL, 'p1', 'ci_red', '{}', 'resolved', 'retried', 2, 3);",
        )
        .unwrap();
        let blockers = "SELECT id || ':' || code || ':' || state || ':' || ifnull(resolution, '-') FROM blocker ORDER BY id";
        let before = ids(&conn, blockers);

        let report = migrate_with(&mut conn, &MIGRATIONS[..7], &backups, Timestamp(3)).unwrap();
        assert_eq!(report.applied, vec![7]);
        assert_eq!(ids(&conn, blockers), before);
        let broken: Option<String> = conn
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert_eq!(broken, None);
        conn.execute_batch(
            "UPDATE blocker SET state = 'resolved', resolution = 'retried', resolved_at = 4 WHERE id = 'b1';
             INSERT INTO blocker VALUES ('b3', 'w5', NULL, 'p1', 'toolchain_unpinned', '{}', 'resolved', 'retried', 4, 5),
               ('b4', 'w5', NULL, 'p1', 'toolchain_unavailable', '{}', 'open', NULL, 5, NULL);",
        )
        .unwrap();
        for sql in [
            "INSERT INTO blocker VALUES ('b5', 'w5', NULL, 'p1', 'toolchain_unavailable', '{}', 'open', NULL, 6, NULL)",
            "INSERT INTO blocker VALUES ('b6', 'w4', NULL, 'p1', 'toolchain_missing', '{}', 'open', NULL, 6, NULL)",
        ] {
            assert!(conn.execute(sql, []).is_err(), "{sql}");
        }
    }

    #[test]
    fn the_secret_migration_keeps_blockers_and_adds_the_secret_table() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let mut conn = Connection::open(dir.path().join("saka.db")).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..2], &backups, Timestamp(1)).unwrap();
        conn.execute_batch(V2_DATA).unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..7], &backups, Timestamp(2)).unwrap();
        conn.execute_batch(
            "INSERT INTO blocker VALUES ('b1', 'w5', NULL, 'p1', 'toolchain_unpinned', '{}', 'open', NULL, 3, NULL);",
        )
        .unwrap();
        let blockers = "SELECT id || ':' || code || ':' || state FROM blocker ORDER BY id";
        let before = ids(&conn, blockers);

        let report = migrate_with(&mut conn, &MIGRATIONS[..8], &backups, Timestamp(3)).unwrap();
        assert_eq!(report.applied, vec![8]);
        assert_eq!(ids(&conn, blockers), before);
        conn.execute_batch(
            "UPDATE blocker SET state = 'resolved', resolution = 'retried', resolved_at = 4 WHERE id = 'b1';
             INSERT INTO blocker VALUES ('b2', 'w5', NULL, 'p1', 'secret_missing', '{}', 'resolved', 'retried', 4, 5),
               ('b3', 'w5', NULL, 'p1', 'secret_unreadable', '{}', 'open', NULL, 5, NULL);
             INSERT INTO secret VALUES ('p1', 'NPM_TOKEN', zeroblob(24), x'00', 1, 1);",
        )
        .unwrap();
        for sql in [
            "INSERT INTO secret VALUES ('p1', 'SHORT_NONCE', zeroblob(12), x'00', 1, 1)",
            "INSERT INTO secret VALUES ('p9', 'NO_PROJECT', zeroblob(24), x'00', 1, 1)",
            "INSERT INTO secret VALUES ('p1', 'NPM_TOKEN', zeroblob(24), x'00', 2, 2)",
        ] {
            assert!(conn.execute(sql, []).is_err(), "{sql}");
        }
    }

    #[test]
    fn the_spec_context_migration_keeps_revisions_with_an_empty_context() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let mut conn = Connection::open(dir.path().join("saka.db")).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..1], &backups, Timestamp(1)).unwrap();
        conn.execute_batch(V1_DATA).unwrap();
        migrate_with(&mut conn, &MIGRATIONS[..8], &backups, Timestamp(2)).unwrap();

        let report = migrate_with(&mut conn, &MIGRATIONS[..9], &backups, Timestamp(3)).unwrap();
        assert_eq!(report.applied, vec![9]);
        assert_eq!(
            ids(
                &conn,
                "SELECT id || ':' || context_hash || ':' || parser_version FROM spec_revision"
            ),
            vec!["v1:sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855:1"]
        );
        assert_eq!(
            ids(
                &conn,
                "SELECT id FROM work_item WHERE spec_revision_id = 'v1' ORDER BY id"
            ),
            vec!["w1", "w2", "w3"]
        );
        assert_eq!(
            ids(
                &conn,
                "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'spec_revision' AND sql IS NOT NULL"
            ),
            vec!["spec_revision_spec_idx"]
        );
        let broken: Option<String> = conn
            .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
            .optional()
            .unwrap();
        assert_eq!(broken, None);

        let hash = format!("sha256:{}", "a".repeat(64));
        conn.execute(
            "INSERT INTO context_blob VALUES ('p1', ?1, x'2341', 3)",
            [&hash],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO spec_revision_context VALUES ('v1', 1, 'p1', 'docs/a.md', 'link', 4, ?1, 2)",
            [&hash],
        )
        .unwrap();
        let refused = [
            "INSERT INTO spec_revision SELECT 'v2', spec_id, project_id, source_commit, content_hash, context_hash,
               content, lint_ok, lint_issues, model, parser_version, created_at FROM spec_revision WHERE id = 'v1'"
                .to_string(),
            "INSERT INTO context_blob VALUES ('p1', 'sha256:short', x'00', 3)".to_string(),
            format!("INSERT INTO spec_revision_context VALUES ('v1', 2, 'p1', 'docs/a.md', 'link', 4, '{hash}', 2)"),
            format!("INSERT INTO spec_revision_context VALUES ('v1', 3, 'p1', 'docs/b.md', 'other', 4, '{hash}', 2)"),
            format!("INSERT INTO spec_revision_context VALUES ('v1', 33, 'p1', 'docs/c.md', 'link', 4, '{hash}', 2)"),
            format!("INSERT INTO spec_revision_context VALUES ('missing', 4, 'p1', 'docs/d.md', 'link', 4, '{hash}', 2)"),
        ];
        for sql in refused {
            assert!(conn.execute(&sql, []).is_err(), "{sql}");
        }
        conn.execute(
            "INSERT INTO spec_revision_context VALUES ('v1', 5, 'p1', 'docs/e.md', 'link', 4, 'sha256:unknown', 2)",
            [],
        )
        .unwrap_err();
    }

    #[test]
    fn the_webhook_delivery_table_dedupes_per_integration() {
        let dir = tempfile::tempdir().unwrap();
        let backups = dir.path().join("backups");
        let mut conn = Connection::open(dir.path().join("saka.db")).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        let report = migrate_with(&mut conn, &MIGRATIONS[..10], &backups, Timestamp(1)).unwrap();
        assert_eq!(report.applied.last(), Some(&10));
        conn.execute_batch(
            "INSERT INTO webhook_delivery VALUES ('w1', 'github', 'd-1', 'ping', NULL, NULL, 'ignored', 0, 1);
             INSERT INTO webhook_delivery VALUES ('w2', 'other', 'd-1', 'ping', NULL, NULL, 'ignored', 0, 1);",
        )
        .unwrap();
        for sql in [
            "INSERT INTO webhook_delivery VALUES ('w3', 'github', 'd-1', 'ping', NULL, NULL, 'ignored', 0, 2)",
            "INSERT INTO webhook_delivery VALUES ('w4', 'github', 'd-2', 'ping', NULL, NULL, 'refreshed', 0, 2)",
            "INSERT INTO webhook_delivery VALUES ('w5', 'github', 'd-3', 'ping', NULL, NULL, 'ignored', 1, 2)",
            "INSERT INTO webhook_delivery VALUES ('w6', 'github', 'd-4', 'ping', NULL, 'missing', 'ignored', 0, 2)",
            "INSERT INTO webhook_delivery VALUES ('w7', 'github', '', 'ping', NULL, NULL, 'ignored', 0, 2)",
        ] {
            assert!(conn.execute(sql, []).is_err(), "{sql}");
        }
        assert_eq!(
            ids(
                &conn,
                "SELECT name FROM sqlite_master WHERE type = 'index' AND name IN ('webhook_delivery_received_idx', 'change_request_head_idx') ORDER BY name"
            ),
            vec!["change_request_head_idx", "webhook_delivery_received_idx"]
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
