#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use rusqlite::{Connection, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const BEFORE: [&str; 27] = [
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
    include_str!("../migrations/0020_hook_run.sql"),
    include_str!("../migrations/0021_external_changes.sql"),
    include_str!("../migrations/0022_taken_over_resolution.sql"),
    include_str!("../migrations/0023_review.sql"),
    include_str!("../migrations/0024_branch_moved_blocker.sql"),
    include_str!("../migrations/0025_merge_queue.sql"),
    include_str!("../migrations/0026_merge_queue_native.sql"),
    include_str!("../migrations/0027_fix_round.sql"),
];

const ROWS: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO project_repo (project_id, repo_id, name, is_default, base_branch, path_scope, created_at, updated_at)
      VALUES ('p1', 'r1', 'app', 1, 'main', '[\"**\"]', 1, 1);
";

fn checksum(sql: &str) -> String {
    let hex: String = Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

#[test]
fn the_autonomy_migration_moves_the_level_to_the_repo_binding_and_keeps_every_row() {
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
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
        conn.execute_batch(ROWS).unwrap();
    }
    let store = Store::open(&path, &dir.path().join("backups"), Timestamp(10)).unwrap();
    assert_eq!(store.migration().applied, vec![28, 29]);
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let projects: i64 = conn
        .query_row("SELECT count(*) FROM project WHERE id = 'p1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(projects, 1);
    assert!(
        conn.prepare("SELECT autonomy FROM project").is_err(),
        "the project no longer carries a level"
    );
    let level: Option<String> = conn
        .query_row(
            "SELECT autonomy FROM project_repo WHERE repo_id = 'r1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(level, None);

    conn.execute(
        "UPDATE project_repo SET autonomy = 'strict' WHERE repo_id = 'r1'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE project_repo SET autonomy = NULL WHERE repo_id = 'r1'",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE project_repo SET autonomy = 'loud' WHERE repo_id = 'r1'",
            []
        )
        .is_err()
    );
}
