#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use rusqlite::{Connection, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const BEFORE: [&str; 35] = [
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
];

const ROWS: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO spec_draft (id, project_id, path, slug, idea, locale, context_paths, state, attempt, retries,
        queued_at, version, created_by, created_at, updated_at)
      VALUES ('d1', 'p1', 'docs/specs/a.md', 'a', 'idea', 'en', '[]', 'queued', 0, 0, 1, 1, 'operator:local', 1, 1);
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
    path: &str,
    mode: &str,
    state: &str,
    since: Option<i64>,
) -> bool {
    let attempt = i64::from(state != "queued");
    let attempt_id = (attempt == 1).then_some("a1");
    conn.execute(
        "INSERT INTO spec_draft (id, project_id, path, slug, idea, locale, mode, context_paths, state, attempt,
           current_attempt_id, retries, queued_at, awaiting_since, version, created_by, created_at, updated_at)
         VALUES (?1, 'p1', ?2, 'a', 'idea', 'en', ?3, '[]', ?4, ?5, ?6, 0, 1, ?7, 1, 'operator:local', 1, 1)",
        params![id, path, mode, state, attempt, attempt_id, since],
    )
    .is_ok()
}

#[test]
fn the_session_migration_keeps_drafts_and_accepts_the_awaiting_state_only_for_sessions() {
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
    assert_eq!(store.migration().applied, vec![36, 37, 38]);
    drop(store);

    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    let kept: String = conn
        .query_row(
            "SELECT path || ' ' || mode || ' ' || state FROM spec_draft WHERE id = 'd1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, "docs/specs/a.md draft queued");
    let kind: String = conn
        .query_row(
            "SELECT target_kind FROM spec_draft WHERE id = 'd1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "new");
    assert!(
        conn.execute(
            "UPDATE spec_draft SET target_kind = 'rewrite' WHERE id = 'd1'",
            []
        )
        .is_err()
    );
    conn.execute(
        "UPDATE spec_draft SET target_kind = 'revise' WHERE id = 'd1'",
        [],
    )
    .unwrap();

    assert!(insert(
        &conn,
        "d2",
        "docs/specs/b.md",
        "session",
        "awaiting_operator",
        Some(5)
    ));
    assert!(
        !insert(
            &conn,
            "d3",
            "docs/specs/c.md",
            "draft",
            "awaiting_operator",
            Some(5)
        ),
        "only a session waits for the operator"
    );
    assert!(
        !insert(
            &conn,
            "d4",
            "docs/specs/d.md",
            "session",
            "awaiting_operator",
            None
        ),
        "a waiting draft says since when"
    );
    assert!(
        !insert(
            &conn,
            "d5",
            "docs/specs/e.md",
            "session",
            "drafting",
            Some(5)
        ),
        "only a waiting draft has the time"
    );
    assert!(!insert(
        &conn,
        "d6",
        "docs/specs/f.md",
        "chat",
        "queued",
        None
    ));
    assert!(
        !insert(&conn, "d7", "docs/specs/b.md", "session", "queued", None),
        "a waiting draft still holds its path"
    );

    conn.execute(
        "INSERT INTO spec_draft_attempt (id, spec_draft_id, project_id, attempt, trigger, state, budget, started_at)
         VALUES ('a1', 'd2', 'p1', 1, 'auto', 'running', '{}', 1)",
        [],
    )
    .unwrap();
    for round in [1, 11] {
        conn.execute(
            "INSERT INTO spec_draft_lint_round (attempt_id, round, spec_draft_id, project_id, session_id, turn, outcome,
               bytes, parser_version, issues, created_at)
             VALUES ('a1', ?1, 'd2', 'p1', 'x1', ?1, 'failed', 3, 4, '[]', 3)",
            params![round],
        )
        .unwrap();
    }
}
