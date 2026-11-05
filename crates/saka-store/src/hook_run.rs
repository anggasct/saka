use rusqlite::{Connection, Row, Transaction, params};
use saka_core::hook::HookRun;
use saka_core::ids::{HookRunId, ProjectId, RepoId, SpecId, WorkItemId};
use saka_core::model::Coded;
use saka_core::vocab::HookRunState;

use crate::codec::{json, opt_json, opt_uuid, to_json, uuid, value_json, vocab};
use crate::error::Result;
use crate::list::{Key, Page};
use crate::read::{opt_ts, ts};

const SELECT: &str = "SELECT * FROM hook_run";

fn hook_run_row(row: &Row<'_>) -> Result<HookRun> {
    Ok(HookRun {
        id: HookRunId(uuid(row, "id")?),
        project_id: ProjectId(uuid(row, "project_id")?),
        hook: row.get("hook")?,
        action: vocab(row, "action")?,
        mode: vocab(row, "mode")?,
        trigger: vocab(row, "trigger")?,
        state: vocab(row, "state")?,
        work_item_id: opt_uuid(row, "work_item_id")?.map(WorkItemId),
        spec_id: opt_uuid(row, "spec_id")?.map(SpecId),
        repo_id: opt_uuid(row, "repo_id")?.map(RepoId),
        sha: row.get("sha")?,
        context: json(row, "context")?,
        source_event_id: uuid(row, "source_event_id")?,
        rerun_of: opt_uuid(row, "rerun_of")?.map(HookRunId),
        attempts: crate::codec::u32_col(row, "attempts")?,
        result: opt_json(row, "result")?,
        error: opt_json::<Coded>(row, "error")?,
        queued_at: ts(row, "queued_at")?,
        finished_at: opt_ts(row, "finished_at")?,
        duration_ms: row.get("duration_ms")?,
    })
}

pub(crate) fn upsert(tx: &Transaction<'_>, r: &HookRun) -> Result<()> {
    tx.execute(
        "INSERT INTO hook_run (id, project_id, hook, action, mode, trigger, state, work_item_id, spec_id, repo_id,
           sha, context, source_event_id, rerun_of, attempts, result, error, queued_at, finished_at, duration_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
         ON CONFLICT (id) DO UPDATE SET state = excluded.state, attempts = excluded.attempts,
           result = excluded.result, error = excluded.error, finished_at = excluded.finished_at,
           duration_ms = excluded.duration_ms",
        params![
            r.id.to_string(),
            r.project_id.to_string(),
            r.hook,
            r.action.as_str(),
            r.mode.as_str(),
            r.trigger.as_str(),
            r.state.as_str(),
            r.work_item_id.map(|x| x.to_string()),
            r.spec_id.map(|x| x.to_string()),
            r.repo_id.map(|x| x.to_string()),
            r.sha,
            to_json(&r.context)?,
            r.source_event_id.to_string(),
            r.rerun_of.map(|x| x.to_string()),
            r.attempts,
            r.result.as_ref().map(value_json).transpose()?,
            r.error.as_ref().map(to_json).transpose()?,
            r.queued_at.0,
            r.finished_at.map(|x| x.0),
            r.duration_ms,
        ],
    )?;
    Ok(())
}

pub fn load(conn: &Connection, id: HookRunId) -> Result<Option<HookRun>> {
    let mut stmt = conn.prepare(&format!("{SELECT} WHERE id = ?1"))?;
    let mut rows = stmt.query([id.to_string()])?;
    match rows.next()? {
        Some(row) => Ok(Some(hook_run_row(row)?)),
        None => Ok(None),
    }
}

#[derive(Debug, Clone, Default)]
pub struct HookRunFilter {
    pub project: Option<ProjectId>,
    pub hook: Option<String>,
    pub state: Option<HookRunState>,
    pub work_item: Option<WorkItemId>,
}

pub fn list(conn: &Connection, filter: &HookRunFilter, page: &Page) -> Result<Vec<(HookRun, Key)>> {
    let mut clauses: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let mut add = |clause: &str,
                   value: Box<dyn rusqlite::ToSql>,
                   binds: &mut Vec<Box<dyn rusqlite::ToSql>>| {
        binds.push(value);
        clauses.push(clause.replace('?', &format!("?{}", binds.len())));
    };
    if let Some(p) = filter.project {
        add("project_id = ?", Box::new(p.to_string()), &mut binds);
    }
    if let Some(h) = &filter.hook {
        add("hook = ?", Box::new(h.clone()), &mut binds);
    }
    if let Some(s) = filter.state {
        add("state = ?", Box::new(s.as_str().to_string()), &mut binds);
    }
    if let Some(w) = filter.work_item {
        add("work_item_id = ?", Box::new(w.to_string()), &mut binds);
    }
    if let Some((key, id)) = page.after {
        binds.push(Box::new(key));
        let k = binds.len();
        binds.push(Box::new(id.to_string()));
        let i = binds.len();
        clauses.push(format!(
            "(queued_at < ?{k} OR (queued_at = ?{k} AND id < ?{i}))"
        ));
    }
    binds.push(Box::new(i64::from(page.limit)));
    let limit = binds.len();
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    let sql = format!("{SELECT}{where_sql} ORDER BY queued_at DESC, id DESC LIMIT ?{limit}");
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let mut rows = stmt.query(refs.as_slice())?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let run = hook_run_row(row)?;
        let key = (run.queued_at.0, run.id.0);
        out.push((run, key));
    }
    Ok(out)
}
