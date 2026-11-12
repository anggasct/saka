#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use rusqlite::{Connection, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const BEFORE: [&str; 20] = [
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
];

const ROWS: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO work_item (id, project_id, origin, task_key, work, repo_id, branch, state, version, created_at, updated_at)
      VALUES ('w1', 'p1', 'external_change', 'app#0', 'x', 'r1', 'feature/old', 'queued', 1, 1, 1);
";

fn checksum(sql: &str) -> String {
    let hex: String = Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn item(
    conn: &Connection,
    id: &str,
    origin: &str,
    key: &str,
    branch: &str,
) -> rusqlite::Result<usize> {
    let (spec, rev, step, index) = if origin == "spec_step" {
        (Some("s1"), Some("v1"), Some("S1"), Some(1))
    } else {
        (None, None, None, None)
    };
    conn.execute(
        "INSERT INTO work_item (id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key,
           work, repo_id, branch, state, version, created_at, updated_at)
         VALUES (?1, 'p1', ?2, ?3, ?4, ?5, ?6, ?7, 'w', 'r1', ?8, 'queued', 1, 1, 1)",
        params![id, origin, spec, rev, step, index, key, branch],
    )
}

#[test]
fn outside_changes_may_share_a_branch_and_a_spec_step_branch_stays_unique() {
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
            21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39
        ]
    );
    drop(store);

    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    let kept: String = conn
        .query_row("SELECT branch FROM work_item WHERE id = 'w1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kept, "feature/old", "existing rows survive the rebuild");
    item(&conn, "w2", "spec_step", "x/s1", "saka/x/s1").unwrap();
    assert!(
        item(&conn, "w7", "spec_step", "x/s2", "saka/x/s1").is_err(),
        "two spec steps never share a branch in a repo"
    );
    item(&conn, "w3", "external_change", "app#1", "feature/a").unwrap();
    item(&conn, "w4", "external_change", "app#2", "feature/a").unwrap();
    item(&conn, "w5", "external_change", "app#3", "saka/x/s1").unwrap();
    assert!(
        item(&conn, "w6", "external_change", "app#1", "feature/b").is_err(),
        "a task key is unique in a project"
    );
}

#[test]
fn the_review_phases_are_accepted_and_an_unknown_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saka.db");
    let store = Store::open(&path, &dir.path().join("backups"), Timestamp(10)).unwrap();
    drop(store);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
           VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
         INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
           VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);",
    )
    .unwrap();
    let place = |id: &str, phase: &str| {
        conn.execute(
            "INSERT INTO work_item (id, project_id, origin, task_key, work, repo_id, branch, state, stage, stage_phase,
               version, created_at, updated_at)
             VALUES (?1, 'p1', 'external_change', ?1, 'w', 'r1', ?1, 'in_stage', 'review', ?2, 1, 1, 1)",
            params![id, phase],
        )
    };
    place("a", "awaiting_trigger").unwrap();
    place("b", "awaiting_review").unwrap();
    assert!(place("c", "awaiting_nothing").is_err());

    conn.execute(
        "INSERT INTO external_change (work_item_id, project_id, title, author, from_fork, draft, cause, created_at)
         VALUES ('a', 'p1', 'Add retries', 'alice', 0, 0, 'comment', 1)",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO external_change (work_item_id, project_id, title, author, from_fork, draft, step_id, cause, created_at)
             VALUES ('b', 'p1', 't', 'alice', 0, 0, 'S1', 'auto', 1)",
            [],
        )
        .is_err(),
        "a step needs its spec"
    );
    conn.execute(
        "INSERT INTO change_signal_cursor (repo_id, number, updated_at, through) VALUES ('r1', 7, 1, 1)",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO change_signal_cursor (repo_id, number, updated_at, through) VALUES ('r1', 7, 2, 2)",
            [],
        )
        .is_err(),
        "one cursor per pull request"
    );
}
