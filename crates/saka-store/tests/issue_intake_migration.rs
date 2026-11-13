#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use rusqlite::{Connection, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const BEFORE: [&str; 40] = [
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
    include_str!("../migrations/0038_change_behind_base.sql"),
    include_str!("../migrations/0039_work_kinds.sql"),
    include_str!("../migrations/0040_regression_delivery.sql"),
];

const ROWS: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO spec (id, project_id, path, slug, title, state, origin, priority, latest_revision_id, created_at, updated_at)
      VALUES ('s1', 'p1', 'docs/specs/a.md', 'a', 'A', 'ready', 'intake', 0, 'v1', 1, 1);
    INSERT INTO spec_draft (id, project_id, path, slug, idea, locale, mode, context_paths, state, queued_at, version,
        created_by, created_at, updated_at)
      VALUES ('d1', 'p1', 'docs/specs/b.md', 'b', 'idea', 'en', 'draft', '[]', 'queued', 1, 1, 'operator', 1, 1);
    INSERT INTO inbox_item (id, project_id, kind, subject, source_id, spec_id, state, params, action, opened_at)
      VALUES ('i1', 'p1', 'approval', 'spec', 's1:v1', 's1', 'open', '{}', '{\"kind\":\"approve_spec\",\"params\":{}}', 5);
";

fn checksum(sql: &str) -> String {
    let hex: String = Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn intake(
    conn: &Connection,
    id: &str,
    number: i64,
    state: &str,
    failure: Option<&str>,
    retries: i64,
) -> bool {
    conn.execute(
        "INSERT INTO issue_intake (id, project_id, repo_id, number, url, title, author, state, lane, kind, cause,
           actor, failure, retries, created_at, updated_at)
         VALUES (?1, 'p1', 'r1', ?2, 'https://github.com/acme/app/issues/1', 'Crash', 'alice', ?3, 'bugfix',
           'bugfix', 'label', 'maintainer', ?4, ?5, 1, 1)",
        params![id, number, state, failure, retries],
    )
    .is_ok()
}

fn inbox(conn: &Connection, id: &str, kind: &str, subject: &str) -> bool {
    conn.execute(
        "INSERT INTO inbox_item (id, project_id, kind, subject, source_id, state, params, action, opened_at)
         VALUES (?1, 'p1', ?2, ?3, ?1, 'open', '{}', '{}', 6)",
        params![id, kind, subject],
    )
    .is_ok()
}

#[test]
fn the_migration_adds_issue_intake_and_keeps_old_rows() {
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
        vec![41, 42, 43, 44, 45, 46, 47, 48, 49]
    );
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let kind: String = conn
        .query_row("SELECT kind FROM spec_draft WHERE id = 'd1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kind, "feature", "an old draft is a feature draft");
    let old: (String, String, String) = conn
        .query_row(
            "SELECT kind, subject, action FROM inbox_item WHERE id = 'i1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (old.0.as_str(), old.1.as_str()),
        ("approval", "spec"),
        "an open inbox item survives the rebuild"
    );
    assert!(old.2.contains("approve_spec"));

    assert!(inbox(&conn, "i2", "intake", "intake"));
    assert!(!inbox(&conn, "i3", "intake", "nonsense"));
    assert!(!inbox(&conn, "i4", "nonsense", "intake"));
    let indexes: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND tbl_name = 'inbox_item' AND name LIKE 'inbox_item_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 5, "the rebuilt inbox keeps its indexes");

    assert!(
        conn.execute("UPDATE spec_draft SET kind = 'epic' WHERE id = 'd1'", [])
            .is_err(),
        "a draft kind is one of the work kinds"
    );
    conn.execute("UPDATE spec_draft SET kind = 'chore' WHERE id = 'd1'", [])
        .unwrap();
}

#[test]
fn issue_intakes_are_unique_while_live_and_a_failure_is_present_exactly_when_failed() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        &dir.path().join("saka.db"),
        &dir.path().join("backups"),
        Timestamp(10),
    )
    .unwrap();
    drop(store);
    let conn = Connection::open(dir.path().join("saka.db")).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    conn.execute_batch(ROWS_AFTER).unwrap();

    assert!(intake(&conn, "a", 7, "accepted", None, 0));
    assert!(
        !intake(&conn, "b", 7, "awaiting_approval", None, 0),
        "one live intake per issue"
    );
    assert!(intake(&conn, "c", 8, "dismissed", None, 0));
    assert!(
        intake(&conn, "d", 8, "accepted", None, 0),
        "a dismissed intake does not block a new one"
    );
    assert!(
        !intake(&conn, "e", 9, "failed", None, 0),
        "failed needs its failure"
    );
    assert!(
        !intake(&conn, "f", 9, "accepted", Some("{}"), 0),
        "only failed has one"
    );
    assert!(intake(&conn, "g", 9, "failed", Some("{\"code\":\"x\"}"), 3));
    assert!(
        !intake(&conn, "h", 10, "accepted", None, 4),
        "at most three retries"
    );
    assert!(
        !intake(&conn, "i", 11, "paused", None, 0),
        "states are a closed set"
    );
    assert!(
        !intake(&conn, "j", 0, "accepted", None, 0),
        "issue numbers start at one"
    );
    assert!(
        conn.execute(
            "INSERT INTO issue_signal_cursor (repo_id, number, updated_at, through, seen_ids)
             VALUES ('r1', 7, 1, 1, '[]')",
            [],
        )
        .is_ok()
    );
    assert!(
        conn.execute(
            "INSERT INTO issue_scan_cursor (project_id, repo_id, through) VALUES ('p1', 'r1', 1)",
            [],
        )
        .is_ok()
    );
}

const ROWS_AFTER: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
";
