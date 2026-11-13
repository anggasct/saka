#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use std::path::Path;

use rusqlite::{Connection, params};
use saka_core::ids::ProjectId;
use saka_core::time::Timestamp;
use saka_store::{Store, read};
use sha2::{Digest, Sha256};

const BEFORE_BINDINGS: [&str; 11] = [
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
];

const PROJECT_DATA: &str = "
    INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
      VALUES ('0193f0c4-0000-7000-8000-000000000001', 'github', 'github', 'acme/app', 'https://github.com/acme/app',
              'main', 1, 1);
    INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, created_at, updated_at)
      VALUES ('0193f0c4-0000-7000-8000-000000000002', 'app', '0193f0c4-0000-7000-8000-000000000001', 'docs',
              'trunk', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1, 1);
    INSERT INTO project_repo (project_id, repo_id, name, is_default, path_scope, created_at, updated_at)
      VALUES ('0193f0c4-0000-7000-8000-000000000002', '0193f0c4-0000-7000-8000-000000000001', 'app', 1,
              '[\"services/**\"]', 1, 2);
";

fn checksum(sql: &str) -> String {
    let hex: String = Sha256::digest(sql.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

fn database_at_0011(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migration (version INTEGER PRIMARY KEY, checksum TEXT NOT NULL,
           applied_at INTEGER NOT NULL) STRICT;",
    )
    .unwrap();
    for (i, sql) in BEFORE_BINDINGS.iter().enumerate() {
        conn.execute_batch(sql).unwrap();
        conn.execute(
            "INSERT INTO schema_migration (version, checksum, applied_at) VALUES (?1, ?2, 1)",
            params![i64::try_from(i + 1).unwrap(), checksum(sql)],
        )
        .unwrap();
    }
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    conn.execute_batch(PROJECT_DATA).unwrap();
}

#[test]
fn existing_bindings_take_the_project_base_branch_and_an_unknown_config_hash() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saka.db");
    database_at_0011(&path);

    let store = Store::open(&path, &dir.path().join("backups"), Timestamp(10)).unwrap();
    assert_eq!(
        store.migration().applied,
        vec![
            12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33,
            34, 35, 36, 37, 38, 39, 40, 41, 42, 43
        ]
    );
    let id = ProjectId("0193f0c4-0000-7000-8000-000000000002".parse().unwrap());
    let project = store.read(|c| read::load_project(c, id)).unwrap().unwrap();
    assert_eq!(project.config_sha256, format!("sha256:{}", "0".repeat(64)));
    assert_eq!(project.config_error, None);
    assert_eq!(project.repos.len(), 1);
    let binding = &project.repos[0];
    assert_eq!(
        (
            binding.name.as_str(),
            binding.base_branch.as_str(),
            binding.is_default
        ),
        ("app", "trunk", true)
    );
    assert_eq!(binding.path_scope, ["services/**"]);
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let created: (i64, i64) = conn
        .query_row("SELECT created_at, updated_at FROM project_repo", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(created, (1, 2));
    let indexes: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index'
               AND name IN ('project_repo_one_default', 'project_repo_repo_idx')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 2);
    conn.execute_batch(
        "INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
           VALUES ('0193f0c4-0000-7000-8000-000000000003', 'github', 'github', 'acme/web',
                   'https://github.com/acme/web', 'main', 3, 3);",
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO project_repo (project_id, repo_id, name, is_default, base_branch, path_scope, created_at,
           updated_at)
         VALUES ('0193f0c4-0000-7000-8000-000000000002', '0193f0c4-0000-7000-8000-000000000003', 'web', 0,
                 'develop', '[\"**\"]', 3, 3);",
    )
    .unwrap();
    let second_default = conn.execute(
        "UPDATE project_repo SET is_default = 1 WHERE name = 'web'",
        [],
    );
    assert!(second_default.is_err(), "one default per project");
}
