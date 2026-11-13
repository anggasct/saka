#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use rusqlite::{Connection, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const BEFORE: [&str; 34] = [
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
];

const ROWS: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO work_item (id, project_id, origin, task_key, work, repo_id, branch, state, version, created_at, updated_at)
      VALUES ('w1', 'p1', 'external_change', 'app#1', 'x', 'r1', 'feature/old', 'awaiting_start', 1, 1, 1);
    INSERT INTO stage_run (id, work_item_id, project_id, stage, attempt, state, trigger, entry_phase, created_at, updated_at)
      VALUES ('run1', 'w1', 'p1', 'dev', 1, 'running', 'auto', 'preparing_environment', 2, 2);
    INSERT INTO environment (id, project_id, work_item_id, repo_id, root_path, worktree_path, branch, base_sha, state,
        created_at, updated_at)
      VALUES ('e1', 'p1', 'w1', 'r1', '/root', '/root/wt', 'feature/old', 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
        'ready', 2, 2);
    INSERT INTO session (id, project_id, work_item_id, stage_run_id, environment_id, cli, account, state, runner_unit,
        socket_path, log_path, created_at, updated_at)
      VALUES ('x1', 'p1', 'w1', 'run1', 'e1', 'claude', 'default', 'running', 'unit', '/s', '/l', 2, 2);
    INSERT INTO blocker (id, work_item_id, stage_run_id, project_id, code, params, state, opened_at)
      VALUES ('b1', 'w1', 'run1', 'p1', 'session_lost', '{}', 'open', 3);
";

fn checksum(sql: &str) -> String {
    let hex: String = Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn blocker(conn: &Connection, id: &str, work_item: &str, code: &str, state: &str) -> bool {
    let resolution = (state == "resolved").then_some("retried");
    conn.execute(
        "INSERT INTO blocker (id, work_item_id, project_id, code, params, state, resolution, opened_at, resolved_at)
         VALUES (?1, ?2, 'p1', ?3, '{}', ?4, ?5, 9, ?6)",
        params![id, work_item, code, state, resolution, resolution.map(|_| 10)],
    )
    .is_ok()
}

fn decision(
    conn: &Connection,
    id: &str,
    session: &str,
    state: &str,
    question: &str,
    owner: &str,
) -> bool {
    conn.execute(
        "INSERT INTO decision_request (id, project_id, session_id, owner, state, question, options, asked_at, expires_at)
         VALUES (?1, 'p1', ?2, ?3, ?4, ?5, '[]', 1, 2)",
        params![id, session, owner, state, question],
    )
    .is_ok()
}

#[test]
fn the_decision_migration_keeps_blockers_accepts_the_new_code_and_allows_one_open_question_per_session()
 {
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
            35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50
        ]
    );
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let kept: String = conn
        .query_row(
            "SELECT code || ' ' || state FROM blocker WHERE id = 'b1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, "session_lost open");
    assert!(blocker(&conn, "b2", "w1", "decision_expired", "resolved"));
    assert!(!blocker(&conn, "b3", "w1", "not_a_code", "resolved"));
    assert!(
        !blocker(&conn, "b4", "w1", "decision_expired", "open"),
        "one open blocker per task"
    );

    let owner = r#"{"kind":"stage_run","work_item_id":"w1","stage_run_id":"run1"}"#;
    assert!(decision(&conn, "d1", "x1", "asked", "Where?", owner));
    assert!(
        !decision(&conn, "d2", "x1", "asked", "Again?", owner),
        "one asked per session"
    );
    assert!(decision(&conn, "d3", "x1", "answered", "Earlier?", owner));
    assert!(!decision(&conn, "d4", "x1", "pending", "Bad state?", owner));
    assert!(
        !decision(&conn, "d5", "x1", "delivered", "", owner),
        "question is not empty"
    );
    assert!(!decision(
        &conn,
        "d6",
        "x1",
        "delivered",
        &"x".repeat(501),
        owner
    ));
    assert!(!decision(
        &conn,
        "d7",
        "x1",
        "delivered",
        "Fine?",
        "not json"
    ));
    let found: String = conn
        .query_row(
            "SELECT group_concat(id) FROM decision_request WHERE json_extract(owner, '$.work_item_id') = 'w1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(found, "d1,d3");
}
