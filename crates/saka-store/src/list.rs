use rusqlite::{Connection, Row, ToSql};
use saka_core::ids::{
    PermissionRequestId, ProjectId, RepoId, SessionId, SpecDraftId, SpecId, WorkItemId,
};
use saka_core::model::{PermissionRequest, Project, Repo, Session, Spec, StageRun};
use uuid::Uuid;

use crate::codec::uuid;
use crate::error::Result;
use crate::read::{
    PERMISSION_SELECT, load_project, permission, project_row, repo_row, session, spec_row,
    stage_run, ts,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub after: Option<(i64, Uuid)>,
    pub limit: u32,
}

pub type Key = (i64, Uuid);

struct Query {
    clauses: Vec<String>,
    binds: Vec<Box<dyn ToSql>>,
}

impl Query {
    fn new() -> Query {
        Query {
            clauses: Vec::new(),
            binds: Vec::new(),
        }
    }

    fn bind(&mut self, value: impl ToSql + 'static) -> String {
        self.binds.push(Box::new(value));
        format!("?{}", self.binds.len())
    }

    fn eq(&mut self, column: &str, value: impl ToSql + 'static) {
        let p = self.bind(value);
        self.clauses.push(format!("{column} = {p}"));
    }

    fn any_of(&mut self, column: &str, values: &[String]) {
        if values.is_empty() {
            return;
        }
        let json = serde_json::to_string(values).unwrap_or_else(|_| "[]".into());
        let p = self.bind(json);
        self.clauses
            .push(format!("{column} IN (SELECT value FROM json_each({p}))"));
    }

    fn after(&mut self, sort: &str, page: &Page, descending: bool) {
        if let Some((key, id)) = page.after {
            let k = self.bind(key);
            let i = self.bind(id.to_string());
            let op = if descending { "<" } else { ">" };
            self.clauses.push(format!(
                "({sort} {op} {k} OR ({sort} = {k} AND id {op} {i}))"
            ));
        }
    }

    fn run<T>(
        mut self,
        conn: &Connection,
        select: &str,
        order: &str,
        page: &Page,
        map: impl Fn(&Row<'_>) -> Result<T>,
    ) -> Result<Vec<T>> {
        let where_sql = if self.clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", self.clauses.join(" AND "))
        };
        let limit = self.bind(i64::from(page.limit));
        let sql = format!("{select}{where_sql} ORDER BY {order} LIMIT {limit}");
        let mut stmt = conn.prepare(&sql)?;
        let binds: Vec<&dyn ToSql> = self.binds.iter().map(|b| b.as_ref()).collect();
        let mut q = stmt.query(binds.as_slice())?;
        let mut out = Vec::new();
        while let Some(row) = q.next()? {
            out.push(map(row)?);
        }
        Ok(out)
    }
}

pub fn projects(conn: &Connection, page: &Page) -> Result<Vec<Project>> {
    let mut q = Query::new();
    q.after("created_at", page, true);
    let ids = q.run(
        conn,
        "SELECT * FROM project",
        "created_at DESC, id DESC",
        page,
        project_row,
    )?;
    let mut out = Vec::with_capacity(ids.len());
    for p in ids {
        if let Some(project) = load_project(conn, p.id)? {
            out.push(project);
        }
    }
    Ok(out)
}

pub fn repos(conn: &Connection, ids: &[RepoId]) -> Result<Vec<Repo>> {
    let list: Vec<String> = ids.iter().map(|i| i.0.to_string()).collect();
    let mut q = Query::new();
    q.any_of("id", &list);
    if list.is_empty() {
        return Ok(Vec::new());
    }
    let page = Page {
        after: None,
        limit: u32::try_from(list.len()).unwrap_or(u32::MAX),
    };
    q.run(
        conn,
        "SELECT * FROM repo",
        "created_at, id",
        &page,
        repo_row,
    )
}

pub fn project_at(
    conn: &Connection,
    repo: RepoId,
    project_path: &str,
) -> Result<Option<ProjectId>> {
    let mut q = Query::new();
    q.eq("workspace_repo_id", repo.0.to_string());
    q.eq("project_path", project_path.to_string());
    let page = Page {
        after: None,
        limit: 1,
    };
    Ok(q.run(conn, "SELECT id FROM project", "id", &page, |r| {
        uuid(r, "id")
    })?
    .into_iter()
    .next()
    .map(ProjectId))
}

pub fn specs(
    conn: &Connection,
    project: ProjectId,
    states: &[String],
    path: Option<&str>,
    page: &Page,
) -> Result<Vec<Spec>> {
    let mut q = Query::new();
    q.eq("project_id", project.0.to_string());
    if let Some(path) = path {
        q.eq("path", path.to_string());
    }
    q.any_of("state", states);
    q.after("updated_at", page, true);
    q.run(
        conn,
        "SELECT * FROM spec",
        "updated_at DESC, id DESC",
        page,
        spec_row,
    )
}

pub fn spec_by_slug(conn: &Connection, project: ProjectId, slug: &str) -> Result<Option<Spec>> {
    let mut q = Query::new();
    q.eq("project_id", project.0.to_string());
    q.eq("slug", slug.to_string());
    let page = Page {
        after: None,
        limit: 1,
    };
    Ok(q.run(conn, "SELECT * FROM spec", "id", &page, spec_row)?
        .into_iter()
        .next())
}

pub fn branches_in_use(
    conn: &Connection,
    repo: RepoId,
    branches: &[String],
    except_project: ProjectId,
) -> Result<Vec<String>> {
    let mut q = Query::new();
    q.eq("repo_id", repo.0.to_string());
    q.any_of("branch", branches);
    let p = q.bind(except_project.0.to_string());
    q.clauses.push(format!("project_id <> {p}"));
    let page = Page {
        after: None,
        limit: 1000,
    };
    q.run(conn, "SELECT branch FROM work_item", "branch", &page, |r| {
        Ok(r.get("branch")?)
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkItemFilter {
    pub project: Option<ProjectId>,
    pub spec: Option<SpecId>,
    pub states: Vec<String>,
}

pub fn work_items(
    conn: &Connection,
    filter: &WorkItemFilter,
    page: &Page,
) -> Result<Vec<(WorkItemId, Key)>> {
    let mut q = Query::new();
    if let Some(p) = filter.project {
        q.eq("project_id", p.0.to_string());
    }
    if let Some(s) = filter.spec {
        q.eq("spec_id", s.0.to_string());
    }
    q.any_of("state", &filter.states);
    q.after("created_at", page, true);
    q.run(
        conn,
        "SELECT id, created_at FROM work_item",
        "created_at DESC, id DESC",
        page,
        |r| {
            let id = uuid(r, "id")?;
            Ok((WorkItemId(id), (ts(r, "created_at")?.0, id)))
        },
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionFilter {
    pub states: Vec<String>,
    pub project: Option<ProjectId>,
    pub work_item: Option<WorkItemId>,
    pub spec_draft: Option<SpecDraftId>,
}

pub type PermissionRow = (PermissionRequest, ProjectId);

fn permission_row(row: &Row<'_>) -> Result<PermissionRow> {
    Ok((permission(row)?, ProjectId(uuid(row, "project_id")?)))
}

pub fn permissions(
    conn: &Connection,
    filter: &PermissionFilter,
    page: &Page,
) -> Result<Vec<PermissionRow>> {
    let mut q = Query::new();
    q.any_of("state", &filter.states);
    if let Some(p) = filter.project {
        q.eq("project_id", p.0.to_string());
    }
    if let Some(w) = filter.work_item {
        q.eq("work_item_id", w.0.to_string());
    }
    if let Some(d) = filter.spec_draft {
        q.eq("spec_draft_id", d.0.to_string());
    }
    q.after("requested_at", page, false);
    q.run(
        conn,
        PERMISSION_SELECT,
        "requested_at, id",
        page,
        permission_row,
    )
}

pub fn load_permission(
    conn: &Connection,
    id: PermissionRequestId,
) -> Result<Option<PermissionRow>> {
    let mut q = Query::new();
    q.eq("id", id.0.to_string());
    let page = Page {
        after: None,
        limit: 1,
    };
    Ok(q.run(conn, PERMISSION_SELECT, "id", &page, permission_row)?
        .into_iter()
        .next())
}

pub fn load_session(conn: &Connection, id: SessionId) -> Result<Option<(Session, ProjectId)>> {
    let mut q = Query::new();
    q.eq("s.id", id.0.to_string());
    let page = Page {
        after: None,
        limit: 1,
    };
    Ok(
        q.run(conn, "SELECT s.* FROM session s", "s.id", &page, |r| {
            Ok((session(r)?, ProjectId(uuid(r, "project_id")?)))
        })?
        .into_iter()
        .next(),
    )
}

pub fn stage_runs(conn: &Connection, id: WorkItemId) -> Result<Vec<StageRun>> {
    let mut q = Query::new();
    q.eq("work_item_id", id.0.to_string());
    let page = Page {
        after: None,
        limit: 10_000,
    };
    q.run(
        conn,
        "SELECT * FROM stage_run",
        "created_at, id",
        &page,
        stage_run,
    )
}

fn counts(conn: &Connection, sql: &str) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(sql)?;
    let mut q = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push((row.get(0)?, row.get(1)?));
    }
    Ok(out)
}

pub fn work_item_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    counts(
        conn,
        "SELECT state, COUNT(*) FROM work_item GROUP BY state ORDER BY state",
    )
}

pub fn live_session_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    counts(
        conn,
        "SELECT state, COUNT(*) FROM session WHERE state IN ('starting', 'running', 'waiting', 'idle', 'interrupted') GROUP BY state ORDER BY state",
    )
}

pub fn resumed_session_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    counts(
        conn,
        "SELECT resume_cause, COUNT(*) FROM session WHERE resume_cause IS NOT NULL GROUP BY resume_cause ORDER BY resume_cause",
    )
}

pub fn budget_exhausted_counts(conn: &Connection) -> Result<Vec<(String, i64)>> {
    counts(
        conn,
        "SELECT json_extract(params, '$.dimension'), COUNT(*) FROM blocker WHERE code = 'budget_exhausted'
         GROUP BY 1 ORDER BY 1",
    )
}

pub fn usage_totals(conn: &Connection) -> Result<Vec<(String, i64, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT account, ifnull(SUM(ifnull(input_tokens, 0) + ifnull(output_tokens, 0)), 0),
           ifnull(SUM(ifnull(cost_usd_micros, 0)), 0)
         FROM usage_record GROUP BY account ORDER BY account",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

pub fn pending_permission_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM permission_request WHERE state = 'pending'",
        [],
        |r| r.get(0),
    )?)
}

pub fn teardown_failed_environment_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM environment WHERE state = 'teardown_failed'",
        [],
        |r| r.get(0),
    )?)
}

pub fn stage_durations(conn: &Connection) -> Result<Vec<(String, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT stage, state, ended_at - started_at FROM stage_run
         WHERE ended_at IS NOT NULL AND started_at IS NOT NULL ORDER BY stage, state",
    )?;
    let mut q = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push((row.get(0)?, row.get(1)?, row.get(2)?));
    }
    Ok(out)
}

pub fn last_seq(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM event", [], |r| r.get(0))?)
}
