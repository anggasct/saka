#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use rusqlite::{Connection, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const BEFORE: [&str; 22] = [
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
];

const ROWS: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO work_item (id, project_id, origin, task_key, work, repo_id, branch, state, stage, stage_phase, version,
      created_at, updated_at)
      VALUES ('w1', 'p1', 'external_change', 'app#1', 'x', 'r1', 'feature/old', 'in_stage', 'review',
              'awaiting_review', 1, 1, 1);
    INSERT INTO environment (id, project_id, work_item_id, repo_id, root_path, worktree_path, branch, base_sha, state,
      created_at, updated_at)
      VALUES ('e0', 'p1', 'w1', 'r1', '/r', '/r/w', 'feature/old', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
              'torn_down', 1, 1);
";

fn checksum(sql: &str) -> String {
    let hex: String = Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

const SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";

fn review(conn: &Connection, id: &str, run: &str, posted: Option<&str>) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO review (id, work_item_id, stage_run_id, session_id, head_sha, outcome, basis, summary,
           posted_event, posted_at, created_at)
         VALUES (?1, 'w1', ?2, 's1', ?3, 'approved', 'none', 'ok', ?4, ?5, 1)",
        params![id, run, SHA, posted, posted.map(|_| 2)],
    )
}

#[test]
fn the_review_migration_keeps_rows_and_opens_the_review_phases_and_a_branchless_environment() {
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
    assert_eq!(
        store.migration().applied,
        vec![
            23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44,
            45, 46, 47, 48, 49
        ]
    );
    drop(store);

    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    let (branch, phase): (String, String) = conn
        .query_row(
            "SELECT branch, stage_phase FROM work_item WHERE id = 'w1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (branch.as_str(), phase.as_str()),
        ("feature/old", "awaiting_review")
    );
    let old_env: String = conn
        .query_row("SELECT branch FROM environment WHERE id = 'e0'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(old_env, "feature/old", "existing environments survive");

    for (id, phase) in [("w2", "posting_review"), ("w3", "reviewed")] {
        conn.execute(
            "INSERT INTO work_item (id, project_id, origin, task_key, work, repo_id, branch, state, stage, stage_phase,
               version, created_at, updated_at)
             VALUES (?1, 'p1', 'external_change', ?1, 'w', 'r1', ?1, 'in_stage', 'review', ?2, 1, 1, 1)",
            params![id, phase],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO environment (id, project_id, work_item_id, repo_id, root_path, worktree_path, branch, base_sha,
           state, created_at, updated_at)
         VALUES ('e1', 'p1', 'w1', 'r1', '/r', '/r/w', NULL, ?1, 'ready', 1, 1)",
        params![SHA],
    )
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO environment (id, project_id, work_item_id, repo_id, root_path, worktree_path, branch, base_sha,
               state, created_at, updated_at)
             VALUES ('e2', 'p1', 'w1', 'r1', '/r', '/r/w', NULL, ?1, 'ready', 1, 1)",
            params![SHA],
        )
        .is_err(),
        "a task still has one live environment"
    );
}

#[test]
fn a_review_row_is_one_per_run_and_posted_columns_come_together() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saka.db");
    drop(Store::open(&path, &dir.path().join("backups"), Timestamp(10)).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    review(&conn, "v1", "run1", None).unwrap();
    assert!(
        review(&conn, "v2", "run1", None).is_err(),
        "one verdict per stage run"
    );
    review(&conn, "v3", "run2", Some("approve")).unwrap();
    assert!(
        conn.execute(
            "INSERT INTO review (id, work_item_id, stage_run_id, session_id, head_sha, outcome, basis, summary,
               posted_event, created_at)
             VALUES ('v4', 'w1', 'run3', 's1', ?1, 'approved', 'none', 'ok', 'approve', 1)",
            params![SHA],
        )
        .is_err(),
        "a posted event has a time"
    );
    assert!(
        conn.execute(
            "INSERT INTO review (id, work_item_id, stage_run_id, session_id, head_sha, outcome, basis, summary, created_at)
             VALUES ('v5', 'w1', 'run4', 's1', ?1, 'maybe', 'none', 'ok', 1)",
            params![SHA],
        )
        .is_err(),
        "an outcome is one of the three"
    );
}
