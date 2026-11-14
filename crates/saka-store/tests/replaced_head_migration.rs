#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::fs;
use std::path::Path;

use rusqlite::{Connection, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const ROWS: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO work_item (id, project_id, origin, task_key, work, repo_id, branch, state, version, created_at, updated_at)
      VALUES ('w1', 'p1', 'external_change', 'app#1', 'x', 'r1', 'feature/old', 'queued', 1, 1, 1);
    INSERT INTO change_request (id, project_id, work_item_id, role, repo_id, number, url, head_branch, base_branch,
        head_sha, state, created_at, updated_at)
      VALUES ('c1', 'p1', 'w1', 'code', 'r1', 7, 'https://github.com/acme/app/pull/7', 'feature/old', 'main',
        'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'open', 2, 2);
";

fn checksum(sql: &str) -> String {
    let hex: String = Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn earlier_migrations() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n[..4] < *"0051")
        })
        .collect();
    files.sort();
    files
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect()
}

#[test]
fn the_migration_adds_an_empty_replaced_head_to_existing_change_requests() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saka.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migration (version INTEGER PRIMARY KEY, checksum TEXT NOT NULL,
               applied_at INTEGER NOT NULL) STRICT;",
        )
        .unwrap();
        for (i, sql) in earlier_migrations().iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO schema_migration (version, checksum, applied_at) VALUES (?1, ?2, 1)",
                params![i64::try_from(i + 1).unwrap(), checksum(sql)],
            )
            .unwrap();
        }
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute_batch(ROWS).unwrap();
    }
    let store = Store::open(&path, &dir.path().join("backups"), Timestamp(10)).unwrap();
    assert_eq!(store.migration().applied, vec![51]);
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let replaced: Option<String> = conn
        .query_row(
            "SELECT replaced_head_sha FROM change_request WHERE id = 'c1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(replaced, None);
    conn.execute(
        "UPDATE change_request SET replaced_head_sha = 'cccccccccccccccccccccccccccccccccccccccc' WHERE id = 'c1'",
        [],
    )
    .unwrap();
}
