#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests fail loudly on a broken setup"
)]

use rusqlite::{Connection, params};
use saka_core::ids::ProjectId;
use saka_core::time::Timestamp;
use saka_store::{Store, list};
use uuid::Uuid;

#[test]
fn the_latest_task_requests_of_a_project_are_capped_and_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("saka.db");
    let store = Store::open(&path, &dir.path().join("backups"), Timestamp(1)).unwrap();
    let project = Uuid::from_u128(1);
    let other = Uuid::from_u128(2);
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        conn.execute(
            "INSERT INTO session (id, project_id, work_item_id, stage_run_id, environment_id, cli, account, state,
               runner_unit, socket_path, log_path, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'claude', 'default', 'running', 'unit', '/s', '/l', 1, 1)",
            params![
                Uuid::from_u128(9).to_string(),
                project.to_string(),
                Uuid::from_u128(7).to_string(),
                Uuid::from_u128(10).to_string(),
                Uuid::from_u128(11).to_string(),
            ],
        )
        .unwrap();
        for i in 0..205_i64 {
            let (owner, project): (Option<String>, Uuid) = if i == 3 {
                (None, project)
            } else if i == 4 {
                (Some(Uuid::from_u128(7).to_string()), other)
            } else {
                (Some(Uuid::from_u128(7).to_string()), project)
            };
            conn.execute(
                "INSERT INTO permission_request (id, project_id, work_item_id, spec_draft_id, session_id,
                   cli_request_id, tool, input, summary, state, requested_at)
                 VALUES (?1, ?2, ?3, ?4, '00000000-0000-0000-0000-000000000009', ?5, 'Bash', '{}', 'ls', 'pending', ?6)",
                params![
                    Uuid::from_u128(1000 + u128::try_from(i).unwrap()).to_string(),
                    project.to_string(),
                    owner,
                    owner.is_none().then(|| Uuid::from_u128(8).to_string()),
                    format!("c{i}"),
                    i,
                ],
            )
            .unwrap();
        }
    }
    let rows = store
        .read(|c| list::recent_task_permissions(c, ProjectId(project), 200))
        .unwrap();
    assert_eq!(rows.len(), 200);
    assert_eq!(rows[0].requested_at.0, 204);
    assert_eq!(rows[199].requested_at.0, 5);
    assert!(
        rows.windows(2)
            .all(|w| w[0].requested_at > w[1].requested_at)
    );
    assert!(
        rows.iter()
            .all(|r| r.requested_at.0 != 3 && r.requested_at.0 != 4)
    );
}
