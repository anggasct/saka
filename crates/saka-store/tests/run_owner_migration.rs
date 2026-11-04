#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use saka_core::time::Timestamp;
use saka_store::Store;
use sha2::{Digest, Sha256};

const BEFORE_RUN_OWNER: [&str; 10] = [
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
];

const TASK_DATA: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('r1', 'github', 'github', 'acme/app', 'https://github.com/acme/app', 'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('p1', 'app', 'r1', 'docs', 'main', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO spec (id, project_id, path, slug, title, state, priority, latest_revision_id, approved_revision_id,
        approved_at, created_at, updated_at)
      VALUES ('s1', 'p1', 'docs/specs/a.md', 'a', 'A', 'approved', 0, 'v1', 'v1', 2, 1, 2);
    INSERT INTO spec_revision (id, spec_id, project_id, source_commit, content_hash, context_hash, content, lint_ok,
        lint_issues, model, parser_version, created_at)
      VALUES ('v1', 's1', 'p1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'h1', 'c1', X'00', 1, '[]', '{}', 3, 1);
    INSERT INTO work_item (id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work,
        repo_id, branch, state, stage, stage_phase, priority, version, created_at, updated_at)
      VALUES ('w1', 'p1', 'spec_step', 's1', 'v1', 'S1', 1, 'a/s1', 'one', 'r1', 'saka/a/s1', 'in_stage', 'dev',
                'running_session', 0, 3, 2, 2),
             ('w2', 'p1', 'spec_step', 's1', 'v1', 'S2', 2, 'a/s2', 'two', 'r1', 'saka/a/s2', 'cancelled', NULL,
                NULL, 0, 4, 2, 3);
    INSERT INTO stage_run (id, work_item_id, project_id, stage, attempt, state, trigger, entry_phase, created_at,
        updated_at)
      VALUES ('run1', 'w1', 'p1', 'dev', 1, 'running', 'auto', 'preparing_environment', 2, 2),
             ('run2', 'w2', 'p1', 'dev', 1, 'cancelled', 'auto', 'preparing_environment', 2, 3);
    INSERT INTO environment (id, project_id, work_item_id, repo_id, root_path, worktree_path, branch, base_sha, state,
        created_at, updated_at, recipe_json, toolchains_json, port_start, port_count, recipe_sha256)
      VALUES ('run1', 'p1', 'w1', 'r1', '/envs/run1', '/envs/run1/worktree', 'saka/a/s1',
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'ready', 2, 2, '{\"setup\":[]}', '[]', 41000, 10, NULL),
             ('run2', 'p1', 'w2', 'r1', '/envs/run2', '/envs/run2/worktree', 'saka/a/s2',
                'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'torn_down', 2, 3, NULL, '[]', NULL, NULL, NULL);
    INSERT INTO session (id, project_id, work_item_id, stage_run_id, environment_id, cli, cli_session_id, account,
        state, runner_unit, socket_path, log_path, last_offset, turns, running_ms, cost_usd_micros, end_reason,
        created_at, updated_at)
      VALUES ('x1', 'p1', 'w1', 'run1', 'run1', 'claude', 'cli-1', 'default', 'lost', 'u1', '/s1', '/l1', 9, 1, 500,
                120, '{\"code\":\"session_lost\",\"params\":{}}', 2, 3),
             ('x3', 'p1', 'w2', 'run2', 'run2', 'claude', NULL, 'default', 'ended', 'u3', '/s3', '/l3', 0, 0, 0,
                NULL, NULL, 2, 3);
    INSERT INTO session (id, project_id, work_item_id, stage_run_id, environment_id, cli, cli_session_id, account,
        state, runner_unit, socket_path, log_path, last_offset, turns, running_ms, resumed_from_session_id,
        resume_cause, created_at, updated_at)
      VALUES ('x2', 'p1', 'w1', 'run1', 'run1', 'claude', 'cli-1', 'default', 'running', 'u2', '/s2', '/l2', 4, 1,
                0, 'x1', 'lost', 4, 4);
    INSERT INTO permission_request (id, project_id, work_item_id, session_id, cli_request_id, tool, input, summary,
        state, answer_source, requested_at, answered_at)
      VALUES ('q1', 'p1', 'w1', 'x1', 'c1', 'Bash', '{\"command\":\"ls\"}', 'ls', 'allowed', 'human:operator:local',
                3, 3),
             ('q2', 'p1', 'w1', 'x2', 'c2', 'Edit', '{}', 'a.go', 'allowed', 'human:operator:local', 4, 4);
    INSERT INTO permission_rule (id, project_id, effect, tool, command, permission_request_id, created_by, created_at)
      VALUES ('pr1', 'p1', 'allow', 'Bash', 'ls', 'q1', 'human:operator:local', 3);
    INSERT INTO session_activity (seq, session_id, \"offset\", work_item_id, project_id, at, kind, summary, data)
      VALUES (7, 'x1', 1, 'w1', 'p1', 3, 'action', 'Bash: ls', '{\"tool\":\"Bash\"}'),
             (9, 'x2', 2, 'w1', 'p1', 4, 'message', 'done', '{}');
    INSERT INTO usage_record (id, project_id, work_item_id, stage_run_id, session_id, account, turn, status,
        input_tokens, output_tokens, cost_usd_micros, running_ms, ended_at)
      VALUES ('u1', 'p1', 'w1', 'run1', 'x1', 'claude:default', 1, 'success', 10, 5, 120, 500, 3);
";

fn checksum(sql: &str) -> String {
    let hex: String = Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn database_at_0010(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migration (version INTEGER PRIMARY KEY, checksum TEXT NOT NULL,
           applied_at INTEGER NOT NULL) STRICT;",
    )
    .unwrap();
    for (i, sql) in BEFORE_RUN_OWNER.iter().enumerate() {
        conn.execute_batch(sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migration (version, checksum, applied_at) VALUES (?1, ?2, 1)",
            params![i64::try_from(i + 1).unwrap(), checksum(sql)],
        )
        .unwrap();
    }
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    conn.execute_batch(TASK_DATA).unwrap();
}

fn strings(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

const SNAPSHOTS: [&str; 6] = [
    "SELECT id || '|' || project_id || '|' || work_item_id || '|' || repo_id || '|' || root_path || '|' || branch
       || '|' || base_sha || '|' || state || '|' || ifnull(recipe_json, '-') || '|' || toolchains_json || '|'
       || ifnull(port_start, '-') || '|' || ifnull(port_count, '-') || '|' || created_at || '|' || updated_at
       FROM environment ORDER BY id",
    "SELECT id || '|' || work_item_id || '|' || stage_run_id || '|' || environment_id || '|' || state || '|'
       || ifnull(cli_session_id, '-') || '|' || last_offset || '|' || turns || '|' || running_ms || '|'
       || ifnull(cost_usd_micros, '-') || '|' || ifnull(end_reason, '-') || '|'
       || ifnull(resumed_from_session_id, '-') || '|' || ifnull(resume_cause, '-') || '|' || updated_at
       FROM session ORDER BY id",
    "SELECT id || '|' || work_item_id || '|' || session_id || '|' || tool || '|' || input || '|' || state || '|'
       || answer_source FROM permission_request ORDER BY id",
    "SELECT seq || '|' || session_id || '|' || \"offset\" || '|' || work_item_id || '|' || kind || '|' || summary
       || '|' || data FROM session_activity ORDER BY seq",
    "SELECT id || '|' || work_item_id || '|' || stage_run_id || '|' || session_id || '|' || turn || '|'
       || input_tokens || '|' || cost_usd_micros || '|' || running_ms FROM usage_record ORDER BY id",
    "SELECT id || '|' || permission_request_id FROM permission_rule ORDER BY id",
];

fn snapshot(conn: &Connection) -> Vec<Vec<String>> {
    SNAPSHOTS.iter().map(|sql| strings(conn, sql)).collect()
}

const DRAFT: &str = "
    INSERT INTO spec_draft (id, project_id, path, slug, idea, locale, context_paths, state, attempt,
        current_attempt_id, retries, queued_at, version, created_by, created_at, updated_at)
      VALUES ('d1', 'p1', 'docs/specs/new.md', 'new', 'An idea', 'en', '[]', 'drafting', 1, 'a1', 0, 5, 3,
                'operator:local', 5, 6);
    INSERT INTO spec_draft_attempt (id, spec_draft_id, project_id, attempt, trigger, state, budget, started_at)
      VALUES ('a1', 'd1', 'p1', 1, 'auto', 'running', '{}', 6);
";

#[test]
fn the_run_owner_migration_keeps_every_task_row_and_guards_the_owner_arc() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saka.db");
    database_at_0010(&path);
    let before = snapshot(&Connection::open(&path).unwrap());
    assert!(before.iter().all(|rows| !rows.is_empty()));

    let store = Store::open(&path, &dir.path().join("backups"), Timestamp(10)).unwrap();
    assert_eq!(
        store.migration().applied,
        vec![11, 12, 13, 14, 15, 16, 17, 18]
    );
    assert!(store.migration().backup.is_some());
    drop(store);

    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    assert_eq!(snapshot(&conn), before);
    let broken: Option<String> = conn
        .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
        .optional()
        .unwrap();
    assert_eq!(broken, None);
    for table in [
        "environment",
        "session",
        "permission_request",
        "session_activity",
        "usage_record",
    ] {
        let drafts: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE spec_draft_id IS NOT NULL"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(drafts, 0, "{table}");
    }
    assert_eq!(
        strings(
            &conn,
            "SELECT name FROM sqlite_master WHERE type = 'index' AND sql IS NOT NULL
               AND tbl_name IN ('environment', 'session', 'permission_request', 'session_activity',
                 'usage_record', 'spec_draft', 'spec_draft_attempt', 'spec_draft_lint_round')
             ORDER BY name"
        ),
        [
            "environment_one_live",
            "environment_one_live_draft",
            "environment_port_live",
            "permission_request_draft_idx",
            "permission_request_pending_idx",
            "session_draft_attempt_idx",
            "session_one_live",
            "session_one_live_draft",
            "session_stage_run_idx",
            "session_state_idx",
            "session_work_item_idx",
            "spec_draft_attempt_one_running",
            "spec_draft_lint_round_draft_idx",
            "spec_draft_one_live_path",
            "spec_draft_project_idx",
            "spec_draft_state_idx",
            "usage_record_account_idx",
            "usage_record_draft_attempt_idx",
            "usage_record_stage_run_idx",
            "usage_record_work_item_idx",
        ]
    );

    let refused = [
        "INSERT INTO environment (id, project_id, repo_id, root_path, worktree_path, state, created_at, updated_at)
           VALUES ('e-none', 'p1', 'r1', '/e', '/e/w', 'preparing', 7, 7)",
        "INSERT INTO environment (id, project_id, work_item_id, spec_draft_id, spec_draft_attempt_id, repo_id,
             root_path, worktree_path, branch, state, created_at, updated_at)
           VALUES ('e-both', 'p1', 'w2', 'd1', 'a1', 'r1', '/e', '/e/w', 'b', 'preparing', 7, 7)",
        "INSERT INTO environment (id, project_id, work_item_id, repo_id, root_path, worktree_path, state, created_at,
             updated_at)
           VALUES ('e-nobranch', 'p1', 'w2', 'r1', '/e', '/e/w', 'preparing', 7, 7)",
        "INSERT INTO environment (id, project_id, spec_draft_id, spec_draft_attempt_id, repo_id, root_path,
             worktree_path, state, created_at, updated_at)
           VALUES ('e-ghost', 'p1', 'd-missing', 'a-missing', 'r1', '/e', '/e/w', 'preparing', 7, 7)",
        "INSERT INTO environment (id, project_id, spec_draft_id, spec_draft_attempt_id, repo_id, root_path,
             worktree_path, state, created_at, updated_at, port_start, port_count)
           VALUES ('e-ports', 'p1', 'd1', 'a1', 'r1', '/e', '/e/w', 'preparing', 7, 7, 42000, 10)",
        "INSERT INTO environment (id, project_id, work_item_id, repo_id, root_path, worktree_path, branch, state,
             created_at, updated_at)
           VALUES ('e-second', 'p1', 'w1', 'r1', '/e', '/e/w', 'b', 'preparing', 7, 7)",
        "INSERT INTO session (id, project_id, environment_id, cli, account, state, runner_unit, socket_path,
             log_path, created_at, updated_at)
           VALUES ('s-none', 'p1', 'run1', 'claude', 'default', 'ended', 'u', '/s', '/l', 7, 7)",
        "INSERT INTO session (id, project_id, work_item_id, stage_run_id, spec_draft_id, spec_draft_attempt_id,
             environment_id, cli, account, state, runner_unit, socket_path, log_path, created_at, updated_at)
           VALUES ('s-both', 'p1', 'w2', 'run2', 'd1', 'a1', 'run2', 'claude', 'default', 'ended', 'u', '/s', '/l',
             7, 7)",
        "INSERT INTO session (id, project_id, work_item_id, environment_id, cli, account, state, runner_unit,
             socket_path, log_path, created_at, updated_at)
           VALUES ('s-half', 'p1', 'w2', 'run2', 'claude', 'default', 'ended', 'u', '/s', '/l', 7, 7)",
        "INSERT INTO session (id, project_id, spec_draft_id, spec_draft_attempt_id, environment_id, cli, account,
             state, runner_unit, socket_path, log_path, created_at, updated_at)
           VALUES ('s-ghost', 'p1', 'd-missing', 'a1', 'run2', 'claude', 'default', 'ended', 'u', '/s', '/l', 7, 7)",
        "INSERT INTO session (id, project_id, work_item_id, stage_run_id, environment_id, cli, account, state,
             runner_unit, socket_path, log_path, created_at, updated_at)
           VALUES ('s-second', 'p1', 'w1', 'run1', 'run1', 'claude', 'default', 'starting', 'u', '/s', '/l', 7, 7)",
        "INSERT INTO permission_request (id, project_id, session_id, cli_request_id, tool, input, summary, state,
             requested_at)
           VALUES ('q-none', 'p1', 'x1', 'c9', 'Read', '{}', 'a', 'pending', 7)",
        "INSERT INTO permission_request (id, project_id, work_item_id, spec_draft_id, session_id, cli_request_id,
             tool, input, summary, state, requested_at)
           VALUES ('q-both', 'p1', 'w1', 'd1', 'x1', 'c9', 'Read', '{}', 'a', 'pending', 7)",
        "INSERT INTO permission_request (id, project_id, spec_draft_id, session_id, cli_request_id, tool, input,
             summary, state, requested_at)
           VALUES ('q-ghost', 'p1', 'd-missing', 'x1', 'c9', 'Read', '{}', 'a', 'pending', 7)",
        "INSERT INTO session_activity (session_id, \"offset\", project_id, at, kind, summary, data)
           VALUES ('x1', 20, 'p1', 7, 'action', 'a', '{}')",
        "INSERT INTO session_activity (session_id, \"offset\", work_item_id, spec_draft_id, project_id, at, kind,
             summary, data)
           VALUES ('x1', 21, 'w1', 'd1', 'p1', 7, 'action', 'a', '{}')",
        "INSERT INTO session_activity (session_id, \"offset\", spec_draft_id, project_id, at, kind, summary, data)
           VALUES ('x1', 22, 'd-missing', 'p1', 7, 'action', 'a', '{}')",
        "INSERT INTO usage_record (id, project_id, session_id, account, turn, status, running_ms, ended_at)
           VALUES ('u-none', 'p1', 'x2', 'claude:default', 5, 'success', 1, 7)",
        "INSERT INTO usage_record (id, project_id, work_item_id, stage_run_id, spec_draft_id, spec_draft_attempt_id,
             session_id, account, turn, status, running_ms, ended_at)
           VALUES ('u-both', 'p1', 'w1', 'run1', 'd1', 'a1', 'x2', 'claude:default', 6, 'success', 1, 7)",
        "INSERT INTO usage_record (id, project_id, spec_draft_id, spec_draft_attempt_id, session_id, account, turn,
             status, running_ms, ended_at)
           VALUES ('u-ghost', 'p1', 'd1', 'a-missing', 'x2', 'claude:default', 7, 'success', 1, 7)",
    ];
    conn.execute_batch(DRAFT).unwrap();
    for sql in refused {
        assert!(conn.execute(sql, []).is_err(), "{sql}");
    }

    conn.execute_batch(
        "INSERT INTO environment (id, project_id, spec_draft_id, spec_draft_attempt_id, repo_id, root_path,
             worktree_path, base_sha, state, created_at, updated_at)
           VALUES ('a1', 'p1', 'd1', 'a1', 'r1', '/envs/a1', '/envs/a1/worktree',
             'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'ready', 7, 7);
         INSERT INTO session (id, project_id, spec_draft_id, spec_draft_attempt_id, environment_id, cli, account,
             state, runner_unit, socket_path, log_path, created_at, updated_at)
           VALUES ('a1', 'p1', 'd1', 'a1', 'a1', 'claude', 'default', 'running', 'u', '/s', '/l', 7, 7);
         INSERT INTO permission_request (id, project_id, spec_draft_id, session_id, cli_request_id, tool, input,
             summary, state, answer_source, requested_at, answered_at)
           VALUES ('q-draft', 'p1', 'd1', 'a1', 'c1', 'Read', '{}', 'docs/prd.md', 'allowed',
             'policy:architect:read', 7, 7);
         INSERT INTO session_activity (session_id, \"offset\", spec_draft_id, project_id, at, kind, summary, data)
           VALUES ('a1', 1, 'd1', 'p1', 7, 'action', 'Read: docs/prd.md', '{}');
         INSERT INTO usage_record (id, project_id, spec_draft_id, spec_draft_attempt_id, session_id, account, turn,
             status, running_ms, ended_at)
           VALUES ('u-draft', 'p1', 'd1', 'a1', 'a1', 'claude:default', 1, 'success', 1, 7);",
    )
    .unwrap();
    let second_live = [
        "INSERT INTO environment (id, project_id, spec_draft_id, spec_draft_attempt_id, repo_id, root_path,
             worktree_path, state, created_at, updated_at)
           VALUES ('a2', 'p1', 'd1', 'a1', 'r1', '/e', '/e/w', 'preparing', 8, 8)",
        "INSERT INTO session (id, project_id, spec_draft_id, spec_draft_attempt_id, environment_id, cli, account,
             state, runner_unit, socket_path, log_path, created_at, updated_at)
           VALUES ('a2', 'p1', 'd1', 'a1', 'a1', 'claude', 'default', 'starting', 'u', '/s', '/l', 8, 8)",
        "INSERT INTO spec_draft_attempt (id, spec_draft_id, project_id, attempt, trigger, reason, state, budget,
             started_at)
           VALUES ('a2', 'd1', 'p1', 2, 'manual', 'again', 'running', '{}', 8)",
        "INSERT INTO spec_draft (id, project_id, path, slug, idea, locale, context_paths, state, attempt, retries,
             queued_at, version, created_by, created_at, updated_at)
           VALUES ('d2', 'p1', 'docs/specs/new.md', 'new', 'Again', 'en', '[]', 'queued', 0, 0, 8, 1,
             'operator:local', 8, 8)",
    ];
    for sql in second_live {
        assert!(conn.execute(sql, []).is_err(), "{sql}");
    }
    conn.execute(
        "UPDATE session SET state = 'ended', ended_at = 9 WHERE id = 'a1'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session (id, project_id, spec_draft_id, spec_draft_attempt_id, environment_id, cli, account,
             state, runner_unit, socket_path, log_path, resumed_from_session_id, resume_cause, created_at,
             updated_at)
           VALUES ('a3', 'p1', 'd1', 'a1', 'a1', 'claude', 'default', 'starting', 'u', '/s', '/l', 'a1', 'lost', 9, 9)",
        [],
    )
    .unwrap();
    let broken: Option<String> = conn
        .query_row("PRAGMA foreign_key_check", [], |r| r.get(0))
        .optional()
        .unwrap();
    assert_eq!(broken, None);
}

#[test]
fn the_draft_tables_check_their_own_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saka.db");
    database_at_0010(&path);
    drop(Store::open(&path, &dir.path().join("backups"), Timestamp(10)).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    conn.execute_batch(DRAFT).unwrap();
    conn.execute_batch(
        "INSERT INTO environment (id, project_id, spec_draft_id, spec_draft_attempt_id, repo_id, root_path,
             worktree_path, base_sha, state, created_at, updated_at)
           VALUES ('a1', 'p1', 'd1', 'a1', 'r1', '/e', '/e/w', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'ready', 7, 7);
         INSERT INTO session (id, project_id, spec_draft_id, spec_draft_attempt_id, environment_id, cli, account,
             state, runner_unit, socket_path, log_path, created_at, updated_at)
           VALUES ('a1', 'p1', 'd1', 'a1', 'a1', 'claude', 'default', 'running', 'u', '/s', '/l', 7, 7);
         INSERT INTO spec_draft_lint_round (attempt_id, round, spec_draft_id, project_id, session_id, turn, outcome,
             bytes, content, content_hash, parser_version, issues, decisions_open, created_at)
           VALUES ('a1', 1, 'd1', 'p1', 'a1', 1, 'missing', NULL, NULL, NULL, 4, '[]', 0, 8);",
    )
    .unwrap();
    let hash = format!("sha256:{}", "b".repeat(64));
    let refused = [
        "UPDATE spec_draft SET state = 'failed' WHERE id = 'd1'".to_string(),
        "UPDATE spec_draft SET state = 'succeeded', finished_at = 9 WHERE id = 'd1'".to_string(),
        "UPDATE spec_draft SET state = 'queued' WHERE id = 'd1'".to_string(),
        "UPDATE spec_draft SET retries = 4, attempt = 5 WHERE id = 'd1'".to_string(),
        "UPDATE spec_draft SET current_attempt_id = NULL WHERE id = 'd1'".to_string(),
        "UPDATE spec_draft SET locale = 'fr' WHERE id = 'd1'".to_string(),
        "UPDATE spec_draft SET idea = '' WHERE id = 'd1'".to_string(),
        "UPDATE spec_draft SET context_paths = '{}' WHERE id = 'd1'".to_string(),
        "UPDATE spec_draft_attempt SET state = 'failed', ended_at = 9 WHERE id = 'a1'".to_string(),
        "UPDATE spec_draft_attempt SET trigger = 'manual' WHERE id = 'a1'".to_string(),
        "UPDATE spec_draft_attempt SET note = 'n' WHERE id = 'a1'".to_string(),
        format!("UPDATE spec_draft_attempt SET packet = 'p', packet_sha256 = '{}' WHERE id = 'a1'", "c".repeat(64)),
        "UPDATE spec_draft_attempt SET packet = 'p' WHERE id = 'a1'".to_string(),
        "UPDATE spec_draft_attempt SET attempt = 5 WHERE id = 'a1'".to_string(),
        format!(
            "INSERT INTO spec_draft_lint_round (attempt_id, round, spec_draft_id, project_id, session_id, turn,
                 outcome, bytes, content, content_hash, parser_version, issues, created_at)
               VALUES ('a1', 2, 'd1', 'p1', 'a1', 1, 'failed', 1, X'00', '{hash}', 4, '[]', 9)"
        ),
        String::from(
            "INSERT INTO spec_draft_lint_round (attempt_id, round, spec_draft_id, project_id, session_id, turn,
                 outcome, bytes, content, content_hash, parser_version, issues, created_at)
               VALUES ('a1', 2, 'd1', 'p1', 'a1', 2, 'passed', 1, NULL, NULL, 4, '[]', 9)"
        ),
        String::from(
            "INSERT INTO spec_draft_lint_round (attempt_id, round, spec_draft_id, project_id, session_id, turn,
                 outcome, bytes, content, content_hash, parser_version, issues, created_at)
               VALUES ('a1', 2, 'd1', 'p1', 'a1', 2, 'missing', 1, NULL, NULL, 4, '[]', 9)"
        ),
        format!(
            "INSERT INTO spec_draft_lint_round (attempt_id, round, spec_draft_id, project_id, session_id, turn,
                 outcome, bytes, content, content_hash, parser_version, issues, created_at)
               VALUES ('a1', 11, 'd1', 'p1', 'a1', 3, 'failed', 1, X'00', '{hash}', 4, '[]', 9)"
        ),
        "INSERT INTO spec_draft_lint_round (attempt_id, round, spec_draft_id, project_id, session_id, turn,
             outcome, bytes, content, content_hash, parser_version, issues, created_at)
           VALUES ('a1', 3, 'd1', 'p1', 'a1', 4, 'failed', 1, X'00', NULL, 4, '[]', 9)"
            .to_string(),
        "INSERT INTO spec_draft_lint_round (attempt_id, round, spec_draft_id, project_id, session_id, turn,
             outcome, bytes, content, content_hash, parser_version, issues, created_at)
           VALUES ('a1', 4, 'd1', 'p1', 'a1', 5, 'failed', 1, X'00', 'short', 4, '[]', 9)"
            .to_string(),
    ];
    for sql in &refused {
        assert!(conn.execute(sql, []).is_err(), "{sql}");
    }
    conn.execute(
        &format!(
            "INSERT INTO spec_draft_lint_round (attempt_id, round, spec_draft_id, project_id, session_id, turn,
                 outcome, bytes, content, content_hash, parser_version, issues, decisions_open, created_at)
               VALUES ('a1', 2, 'd1', 'p1', 'a1', 2, 'passed', 1, X'00', '{hash}', 4, '[]', 1, 9)"
        ),
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE spec_draft SET state = 'failed', failure = '{\"code\":\"lint_rounds_exhausted\",\"params\":{}}',
           finished_at = 9 WHERE id = 'd1'",
        [],
    )
    .unwrap();
    assert_eq!(
        strings(&conn, "SELECT state FROM spec_draft"),
        vec!["failed".to_string()]
    );
}
