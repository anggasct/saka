#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use rusqlite::{Connection, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const BEFORE: [&str; 31] = [
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
    INSERT INTO permission_request (id, project_id, work_item_id, session_id, cli_request_id, tool, input, summary,
        state, answer_source, requested_at)
      VALUES ('q1', 'p1', 'w1', 'x1', 'c1', 'Bash', '{}', 'npm install', 'allowed', 'human:operator:local', 1),
        ('q2', 'p1', 'w1', 'x1', 'c2', 'Edit', '{}', 'a.rs', 'allowed', 'human:operator:local', 1);
    INSERT INTO permission_rule (id, project_id, effect, tool, command, permission_request_id, created_by, created_at)
      VALUES ('old1', 'p1', 'allow', 'Bash', 'npm install', 'q1', 'human:operator:local', 2);
    INSERT INTO permission_rule (id, project_id, effect, tool, command, permission_request_id, created_by, created_at,
        revoked_at, revoke_reason)
      VALUES ('old2', 'p1', 'allow', 'Edit', NULL, 'q2', 'human:operator:local', 3, 4, 'too broad');
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
    effect: &str,
    kind: &str,
    tool: &str,
    command: Option<&str>,
) -> bool {
    conn.execute(
        "INSERT INTO permission_rule (id, project_id, effect, tool, match_kind, command, created_by, created_at)
         VALUES (?1, 'p1', ?2, ?3, ?4, ?5, 'human:operator:local', 9)",
        params![id, effect, tool, kind, command],
    )
    .is_ok()
}

#[test]
fn the_permission_rule_migration_keeps_old_rules_as_exact_allows_and_opens_the_new_shapes() {
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
    assert_eq!(store.migration().applied, vec![32, 33, 34, 35, 36, 37, 38]);
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let rows: Vec<String> = conn
        .prepare(
            "SELECT id || ' ' || effect || ' ' || match_kind || ' ' || tool || ' ' || ifnull(command, '-') || ' '
               || permission_request_id || ' ' || ifnull(revoked_at, '-')
               FROM permission_rule ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        vec![
            "old1 allow exact Bash npm install q1 -",
            "old2 allow exact Edit - q2 4",
        ]
    );

    assert!(insert(&conn, "n1", "deny", "glob", "Bash", Some("rm *")));
    assert!(insert(
        &conn,
        "n2",
        "ask",
        "exact",
        "Bash",
        Some("make deploy")
    ));
    assert!(
        !insert(&conn, "n3", "allow", "exact", "Bash", Some("npm install")),
        "an active identical rule is refused"
    );
    assert!(insert(
        &conn,
        "n4",
        "allow",
        "glob",
        "Bash",
        Some("npm install")
    ));
    assert!(
        !insert(&conn, "n5", "allow", "glob", "Bash", None),
        "Bash needs a command"
    );
    assert!(!insert(&conn, "n6", "maybe", "glob", "Edit", None));
    assert!(!insert(&conn, "n7", "allow", "fuzzy", "Edit", None));
    assert!(
        !conn
            .execute(
                "UPDATE permission_rule SET revoked_at = 11 WHERE id = 'n1'",
                [],
            )
            .is_ok()
    );
    conn.execute(
        "UPDATE permission_rule SET revoked_at = 11, revoke_reason = 'noisy' WHERE id = 'old1'",
        [],
    )
    .unwrap();
    assert!(insert(
        &conn,
        "n8",
        "allow",
        "exact",
        "Bash",
        Some("npm install")
    ));
}
