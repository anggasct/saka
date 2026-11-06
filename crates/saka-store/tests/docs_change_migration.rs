#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use rusqlite::{Connection, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const BEFORE_ROLES: [&str; 17] = [
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_spec_model.sql"),
    include_str!("../migrations/0003_spec_approval.sql"),
    include_str!("../migrations/0004_environment_teardown_failed.sql"),
    include_str!("../migrations/0005_agent_runtime.sql"),
    include_str!("../migrations/0006_task_environment.sql"),
    include_str!("../migrations/0007_toolchain_blockers.sql"),
    include_str!("../migrations/0008_secrets.sql"),
    include_str!("../migrations/0009_spec_context.sql"),
    include_str!("../migrations/0010_webhook_delivery.sql"),
    include_str!("../migrations/0011_run_owner_and_spec_drafts.sql"),
    include_str!("../migrations/0012_multi_repo_bindings.sql"),
    include_str!("../migrations/0013_path_scope.sql"),
    include_str!("../migrations/0014_user_link.sql"),
    include_str!("../migrations/0015_project_authorship.sql"),
    include_str!("../migrations/0016_user_token_blocker.sql"),
    include_str!("../migrations/0017_environment_docs.sql"),
];

const ROWS: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO work_item (id, project_id, origin, task_key, work, repo_id, branch, state, version, created_at, updated_at)
      VALUES ('w1', 'p1', 'external_change', 'app/x', 'x', 'r1', 'saka/x', 'queued', 1, 1, 1);
    INSERT INTO change_request (id, project_id, work_item_id, repo_id, number, url, head_branch, base_branch, head_sha,
      state, created_at, updated_at)
      VALUES ('c1', 'p1', 'w1', 'r1', 7, 'https://github.com/acme/app/pull/7', 'saka/x', 'main',
              'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'open', 1, 1);
";

fn checksum(sql: &str) -> String {
    let hex: String = Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn cr(conn: &Connection, id: &str, role: &str, number: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO change_request (id, project_id, work_item_id, role, repo_id, number, url, head_branch,
           base_branch, head_sha, state, created_at, updated_at)
         VALUES (?1, 'p1', 'w1', ?2, 'r1', ?3, 'u', 'saka/x-docs', 'main',
                 'cccccccccccccccccccccccccccccccccccccccc', 'open', 1, 1)",
        params![id, role, number],
    )
}

#[test]
fn change_requests_get_a_role_unique_per_task_and_the_new_phase_and_blocker_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saka.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migration (version INTEGER PRIMARY KEY, checksum TEXT NOT NULL,
               applied_at INTEGER NOT NULL) STRICT;",
        )
        .unwrap();
        for (i, sql) in BEFORE_ROLES.iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO schema_migration (version, checksum, applied_at) VALUES (?1, ?2, 1)",
                params![i64::try_from(i + 1).unwrap(), checksum(sql)],
            )
            .unwrap();
        }
        conn.execute_batch(ROWS).unwrap();
    }
    let store = Store::open(&path, &dir.path().join("backups"), Timestamp(10)).unwrap();
    assert_eq!(
        store.migration().applied,
        vec![18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29]
    );
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let role: String = conn
        .query_row("SELECT role FROM change_request WHERE id = 'c1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(role, "code");
    assert!(
        cr(&conn, "c2", "code", 8).is_err(),
        "one code change per task"
    );
    cr(&conn, "c3", "docs", 9).unwrap();
    assert!(
        cr(&conn, "c4", "docs", 10).is_err(),
        "one docs change per task"
    );
    assert!(
        conn.execute(
            "UPDATE change_request SET skipped_at = 5 WHERE id = 'c3'",
            []
        )
        .is_err(),
        "only a closed docs change is skipped"
    );
    conn.execute(
        "UPDATE change_request SET state = 'closed', skipped_at = 5 WHERE id = 'c3'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE work_item SET state = 'in_stage', stage = 'sync', stage_phase = 'merging_docs' WHERE id = 'w1'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO blocker (id, work_item_id, project_id, code, params, state, opened_at)
         VALUES ('b1', 'w1', 'p1', 'docs_merge_failed', '{}', 'open', 1)",
        [],
    )
    .unwrap();
    let broken: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(broken, 0);
}
