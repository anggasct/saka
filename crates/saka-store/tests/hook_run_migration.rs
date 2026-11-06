#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use rusqlite::{Connection, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const BEFORE: [&str; 19] = [
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
    include_str!("../migrations/0018_docs_change_requests.sql"),
    include_str!("../migrations/0019_project_workflow.sql"),
];

const ROWS: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO work_item (id, project_id, origin, task_key, work, repo_id, branch, state, version, created_at, updated_at)
      VALUES ('w1', 'p1', 'external_change', 'app/x', 'x', 'r1', 'saka/x', 'queued', 1, 1, 1);
";

fn checksum(sql: &str) -> String {
    let hex: String = Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn insert(
    conn: &Connection,
    id: &str,
    state: &str,
    source: &str,
    rerun_of: Option<&str>,
) -> rusqlite::Result<usize> {
    let finished = (state != "pending").then_some(5);
    let error = (state == "failed").then_some("{\"code\":\"x\",\"params\":{}}");
    conn.execute(
        "INSERT INTO hook_run (id, project_id, hook, action, mode, trigger, state, work_item_id, context,
           source_event_id, rerun_of, queued_at, finished_at, error)
         VALUES (?1, 'p1', 'announce', 'webhook', 'after', 'merged', ?2, 'w1', '{}', ?3, ?4, 1, ?5, ?6)",
        params![id, state, source, rerun_of, finished, error],
    )
}

#[test]
fn hook_runs_are_added_without_touching_existing_rows_and_the_table_guards_its_invariants() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saka.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migration (version INTEGER PRIMARY KEY, checksum TEXT NOT NULL,
               applied_at INTEGER NOT NULL) STRICT;",
        )
        .unwrap();
        for (i, sql) in BEFORE.iter().enumerate() {
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
        vec![
            20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37
        ]
    );
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let items: i64 = conn
        .query_row("SELECT count(*) FROM work_item", [], |r| r.get(0))
        .unwrap();
    assert_eq!(items, 1);

    insert(&conn, "h1", "pending", "e1", None).unwrap();
    assert!(
        insert(&conn, "h2", "pending", "e1", None).is_err(),
        "one first run per hook and event"
    );
    insert(&conn, "h3", "failed", "e1", Some("h1")).unwrap();
    insert(&conn, "h4", "succeeded", "e1", Some("h1")).unwrap();
    assert!(
        insert(&conn, "h5", "paused", "e2", None).is_err(),
        "unknown state"
    );
    assert!(
        conn.execute("UPDATE hook_run SET finished_at = 9 WHERE id = 'h1'", [])
            .is_err(),
        "a pending run has no finish time"
    );
    assert!(
        conn.execute(
            "UPDATE hook_run SET state = 'failed', finished_at = 9 WHERE id = 'h1'",
            []
        )
        .is_err(),
        "a failed run carries its error"
    );
    assert!(
        conn.execute(
            "UPDATE hook_run SET action = 'teleport' WHERE id = 'h1'",
            []
        )
        .is_err()
    );
}
