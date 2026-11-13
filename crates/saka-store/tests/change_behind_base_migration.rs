#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use rusqlite::{Connection, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const BEFORE: [&str; 37] = [
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
    include_str!("../migrations/0028_repo_autonomy.sql"),
    include_str!("../migrations/0029_start_approval.sql"),
    include_str!("../migrations/0030_hook_skip.sql"),
    include_str!("../migrations/0031_inbox_item.sql"),
    include_str!("../migrations/0032_permission_rule.sql"),
    include_str!("../migrations/0033_hook_inbox_event.sql"),
    include_str!("../migrations/0034_identity.sql"),
    include_str!("../migrations/0035_decision_request.sql"),
    include_str!("../migrations/0036_architect_session.sql"),
    include_str!("../migrations/0037_draft_target_kind.sql"),
];

const ROWS: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO work_item (id, project_id, origin, task_key, work, repo_id, branch, state, version, created_at, updated_at)
      VALUES ('w1', 'p1', 'external_change', 'app#1', 'x', 'r1', 'feature/old', 'blocked', 1, 1, 1);
    INSERT INTO stage_run (id, work_item_id, project_id, stage, attempt, state, trigger, entry_phase, created_at, updated_at)
      VALUES ('run1', 'w1', 'p1', 'merge', 1, 'blocked', 'auto', 'awaiting_ci', 2, 2);
    INSERT INTO change_request (id, project_id, work_item_id, role, repo_id, number, url, head_branch, base_branch,
        head_sha, state, created_at, updated_at)
      VALUES ('c1', 'p1', 'w1', 'code', 'r1', 7, 'https://github.com/acme/app/pull/7', 'feature/old', 'main',
        'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'open', 2, 2);
    INSERT INTO blocker (id, work_item_id, stage_run_id, project_id, code, params, state, opened_at)
      VALUES ('b1', 'w1', 'run1', 'p1', 'ci_red', '{\"source\":\"scm\"}', 'open', 3);
";

fn checksum(sql: &str) -> String {
    let hex: String = Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn blocker(conn: &Connection, id: &str, code: &str, state: &str) -> bool {
    let resolution = (state == "resolved").then_some("retried");
    conn.execute(
        "INSERT INTO blocker (id, work_item_id, project_id, code, params, state, resolution, opened_at, resolved_at)
         VALUES (?1, 'w1', 'p1', ?2, '{}', ?3, ?4, 9, ?5)",
        params![id, code, state, resolution, resolution.map(|_| 10)],
    )
    .is_ok()
}

#[test]
fn the_migration_keeps_blockers_accepts_the_new_code_and_adds_the_update_marker() {
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
        vec![38, 39, 40, 41, 42, 43, 44, 45, 46]
    );
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let kept: String = conn
        .query_row(
            "SELECT code || ' ' || state || ' ' || params FROM blocker WHERE id = 'b1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, "ci_red open {\"source\":\"scm\"}");
    assert!(blocker(&conn, "b2", "change_behind_base", "resolved"));
    assert!(!blocker(&conn, "b3", "not_a_code", "resolved"));
    assert!(
        !blocker(&conn, "b4", "ci_red", "open"),
        "one open blocker per task"
    );

    let marker: Option<String> = conn
        .query_row(
            "SELECT update_head_sha FROM change_request WHERE id = 'c1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(marker, None);
}
