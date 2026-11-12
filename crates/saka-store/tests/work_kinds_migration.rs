#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use rusqlite::{Connection, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const BEFORE: [&str; 38] = [
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
];

const ROWS: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO spec (id, project_id, path, slug, title, state, origin, priority, latest_revision_id,
        approved_revision_id, approved_at, created_at, updated_at)
      VALUES ('s1', 'p1', 'docs/specs/a.md', 'a', 'A', 'approved', 'architect', 3, 'v1', 'v1', 5, 1, 1);
    INSERT INTO spec (id, project_id, path, slug, title, state, origin, priority, latest_revision_id, created_at, updated_at)
      VALUES ('s2', 'p1', 'docs/specs/b.md', 'b', 'B', 'ready', 'intake', 0, 'v2', 1, 1);
    INSERT INTO spec_revision (id, spec_id, project_id, source_commit, content_hash, context_hash, content, lint_ok,
        lint_issues, model, parser_version, created_at)
      VALUES ('v1', 's1', 'p1', 'cccccccccccccccccccccccccccccccccccccccc', 'sha256:a', 'sha256:c', x'00', 1, '[]', '{}', 5, 1);
    INSERT INTO work_item (id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work,
        repo_id, branch, state, version, created_at, updated_at)
      VALUES ('w1', 'p1', 'spec_step', 's1', 'v1', 'S1', 1, 'a/s1', 'x', 'r1', 'saka/a/s1', 'queued', 1, 1, 1);
    INSERT INTO work_item (id, project_id, origin, task_key, work, repo_id, branch, state, version, created_at, updated_at)
      VALUES ('w2', 'p1', 'external_change', 'app#1', 'x', 'r1', 'feature/old', 'blocked', 1, 1, 1);
    INSERT INTO stage_run (id, work_item_id, project_id, stage, attempt, state, trigger, entry_phase, created_at, updated_at)
      VALUES ('run1', 'w2', 'p1', 'merge', 1, 'blocked', 'auto', 'awaiting_ci', 2, 2);
    INSERT INTO blocker (id, work_item_id, stage_run_id, project_id, code, params, state, opened_at)
      VALUES ('b1', 'w2', 'run1', 'p1', 'ci_red', '{\"source\":\"scm\"}', 'open', 3);
";

fn checksum(sql: &str) -> String {
    let hex: String = Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn spec_with(conn: &Connection, id: &str, origin: &str, kind: Option<&str>) -> bool {
    let (columns, values) = match kind {
        Some(_) => (", kind", ", ?4"),
        None => ("", ""),
    };
    let sql = format!(
        "INSERT INTO spec (id, project_id, path, slug, title, state, origin, latest_revision_id, created_at, updated_at{columns})
         VALUES (?1, 'p1', 'docs/specs/' || ?1 || '.md', ?1, 'T', 'draft', ?2, 'v', 1, 1{values})"
    );
    match kind {
        Some(kind) => conn.execute(&sql, params![id, origin, 0, kind]),
        None => conn.execute(&sql, params![id, origin]),
    }
    .is_ok()
}

fn work_item_kind(conn: &Connection, id: &str, kind: &str) -> bool {
    conn.execute(
        "INSERT INTO work_item (id, project_id, origin, task_key, work, repo_id, branch, state, version, kind,
           created_at, updated_at)
         VALUES (?1, 'p1', 'external_change', ?1, 'x', 'r1', ?1, 'queued', 1, ?2, 1, 1)",
        params![id, kind],
    )
    .is_ok()
}

fn blocker(conn: &Connection, id: &str, code: &str) -> bool {
    conn.execute(
        "INSERT INTO blocker (id, work_item_id, project_id, code, params, state, resolution, opened_at, resolved_at)
         VALUES (?1, 'w1', 'p1', ?2, '{}', 'resolved', 'retried', 9, 10)",
        params![id, code],
    )
    .is_ok()
}

fn check(
    conn: &Connection,
    id: &str,
    attempt: i64,
    outcome: Option<&str>,
    observed: Option<i64>,
) -> bool {
    conn.execute(
        "INSERT INTO regression_check (id, work_item_id, stage_run_id, project_id, attempt, commit_sha, base_sha,
           outcome, requested_at, observed_at)
         VALUES (?1, 'w2', 'run1', 'p1', ?2, ?3, ?3, ?4, 5, ?5)",
        params![id, attempt, "d".repeat(40), outcome, observed],
    )
    .is_ok()
}

#[test]
fn the_migration_carries_old_rows_as_features_and_adds_kinds_quick_specs_and_regression_checks() {
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
    assert_eq!(store.migration().applied, vec![39]);
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let specs: Vec<String> = conn
        .prepare("SELECT id || ' ' || kind || ' ' || origin || ' ' || state || ' ' || priority FROM spec ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        specs,
        [
            "s1 feature architect approved 3",
            "s2 feature intake ready 0"
        ]
    );
    let kinds: Vec<(String, Option<String>)> = conn
        .prepare("SELECT id, kind FROM work_item ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        kinds,
        [
            ("w1".to_string(), Some("feature".to_string())),
            ("w2".to_string(), None)
        ]
    );

    assert!(spec_with(&conn, "q1", "quick", None));
    assert!(!spec_with(&conn, "q2", "robot", None));
    assert!(spec_with(&conn, "q3", "manual", Some("security_patch")));
    assert!(!spec_with(&conn, "q4", "manual", Some("hotfix")));
    let defaulted: String = conn
        .query_row("SELECT kind FROM spec WHERE id = 'q1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(defaulted, "feature");
    assert!(work_item_kind(&conn, "w3", "bugfix"));
    assert!(!work_item_kind(&conn, "w4", "hotfix"));

    let kept: String = conn
        .query_row(
            "SELECT code || ' ' || state || ' ' || params FROM blocker WHERE id = 'b1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, "ci_red open {\"source\":\"scm\"}");
    assert!(blocker(&conn, "b2", "regression_not_reproduced"));
    assert!(blocker(&conn, "b3", "change_behind_base"));
    assert!(!blocker(&conn, "b4", "not_a_code"));
    let one_open = conn.execute(
        "INSERT INTO blocker (id, work_item_id, project_id, code, params, state, opened_at)
         VALUES ('b5', 'w2', 'p1', 'ci_red', '{}', 'open', 9)",
        [],
    );
    assert!(one_open.is_err(), "one open blocker per task");

    assert!(check(&conn, "g1", 1, None, None));
    assert!(check(&conn, "g2", 2, Some("reproduced"), Some(6)));
    assert!(
        !check(&conn, "g3", 2, Some("error"), Some(7)),
        "one row per attempt"
    );
    assert!(!check(&conn, "g4", 3, Some("flaky"), Some(7)));
    assert!(
        !check(&conn, "g5", 3, Some("error"), None),
        "an outcome is observed"
    );
    assert!(
        !check(&conn, "g6", 3, None, Some(7)),
        "a running check has no outcome"
    );
    assert!(!check(&conn, "g7", 0, None, None));
    let short = conn.execute(
        "INSERT INTO regression_check (id, work_item_id, stage_run_id, project_id, attempt, commit_sha, base_sha,
           requested_at) VALUES ('g8', 'w2', 'run1', 'p1', 9, 'abc', 'abc', 5)",
        [],
    );
    assert!(short.is_err(), "a sha has 40 characters");
    let defaults: String = conn
        .query_row(
            "SELECT checks || paths FROM regression_check WHERE id = 'g1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(defaults, "[][]");

    let broken: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(broken, 0);
}
