use rusqlite::{Connection, Row};
use saka_core::ids::{IssueIntakeId, ProjectId, RepoId, SpecDraftId, SpecId};
use saka_core::issue_intake::IssueIntake;
use saka_core::model::{Repo, Spec};
use saka_core::vocab::SpecDraftState;
use uuid::Uuid;

use crate::codec::{opt_json, opt_uuid, u32_col, u64_col, uuid, vocab};
use crate::error::Result;
use crate::list::{Key, Page};
use crate::read::{load_repo, load_spec, one, ts};

fn intake(row: &Row<'_>) -> Result<IssueIntake> {
    Ok(IssueIntake {
        id: IssueIntakeId(uuid(row, "id")?),
        project_id: ProjectId(uuid(row, "project_id")?),
        repo_id: RepoId(uuid(row, "repo_id")?),
        number: u64_col(row, "number")?,
        url: row.get("url")?,
        title: row.get("title")?,
        author: row.get("author")?,
        state: vocab(row, "state")?,
        lane: vocab(row, "lane")?,
        kind: vocab(row, "kind")?,
        cause: vocab(row, "cause")?,
        actor: row.get("actor")?,
        spec_id: opt_uuid(row, "spec_id")?.map(SpecId),
        draft_id: opt_uuid(row, "draft_id")?.map(SpecDraftId),
        failure: opt_json(row, "failure")?,
        retries: u32_col(row, "retries")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

const SELECT: &str = concat!("SELECT ", issue_intake_columns!(), " FROM issue_intake");

pub fn load_issue_intake(conn: &Connection, id: IssueIntakeId) -> Result<Option<IssueIntake>> {
    one(
        conn,
        &format!("{SELECT} WHERE id = ?1"),
        &[&id.to_string()],
        intake,
    )
}

pub fn issue_intakes(
    conn: &Connection,
    project_id: ProjectId,
    states: &[String],
    page: &Page,
) -> Result<Vec<(IssueIntake, Key)>> {
    let mut sql = format!("{SELECT} WHERE project_id = ?1");
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project_id.to_string())];
    if !states.is_empty() {
        binds.push(Box::new(
            serde_json::to_string(states).unwrap_or_else(|_| "[]".into()),
        ));
        sql.push_str(&format!(
            " AND state IN (SELECT value FROM json_each(?{}))",
            binds.len()
        ));
    }
    if let Some((key, id)) = page.after {
        binds.push(Box::new(key));
        binds.push(Box::new(id.to_string()));
        let (k, i) = (binds.len() - 1, binds.len());
        sql.push_str(&format!(
            " AND (created_at < ?{k} OR (created_at = ?{k} AND id < ?{i}))"
        ));
    }
    binds.push(Box::new(i64::from(page.limit)));
    sql.push_str(&format!(
        " ORDER BY created_at DESC, id DESC LIMIT ?{}",
        binds.len()
    ));
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let mut q = stmt.query(refs.as_slice())?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        let found = intake(row)?;
        let key: (i64, Uuid) = (found.created_at.0, found.id.0);
        out.push((found, key));
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub struct IntakeLinks {
    pub repo: Option<Repo>,
    pub spec: Option<Spec>,
    pub draft: Option<SpecDraftState>,
}

pub fn intake_links(conn: &Connection, intake: &IssueIntake) -> Result<IntakeLinks> {
    let spec = match intake.spec_id {
        Some(id) => load_spec(conn, id)?,
        None => None,
    };
    let draft = match intake.draft_id {
        Some(id) => one(
            conn,
            "SELECT state FROM spec_draft WHERE id = ?1",
            &[&id.to_string()],
            |row| vocab(row, "state"),
        )?,
        None => None,
    };
    Ok(IntakeLinks {
        repo: load_repo(conn, intake.repo_id)?,
        spec,
        draft,
    })
}
