use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, OptionalExtension, Row, params};
use saka_core::ids::{
    BlockerId, ChangeRequestId, EnvironmentId, GateResultId, PermissionRequestId, ProjectId,
    RepoId, SessionId, SpecId, SpecRevisionId, StageRunId, WorkItemId,
};
use saka_core::model::{
    Blocker, ChangeRequest, Environment, GateResult, PermissionRequest, Project, ProjectRepo, Repo,
    RevisionView, Session, Spec, SpecDependency, StageRun, StepPlan, Task, WorkItem,
    WorkItemDependency,
};
use saka_core::time::Timestamp;
use saka_core::vocab::{SessionState, Stage};
use serde_json::Value;
use uuid::Uuid;

use crate::codec::{json, opt_json, opt_uuid, opt_vocab, u32_col, u64_col, uuid, vocab};
use crate::error::{Result, StoreError};
use crate::write::{ActionRecord, SpecRevisionRow};

#[derive(Debug, Clone, Copy)]
pub enum TaskSet<'a> {
    Ids(&'a [WorkItemId]),
    Reconcile,
    Spec(SpecId),
}

const WORK_ITEM_COLUMNS: &str = "id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key,
    work, repo_id, branch, state, stage, stage_phase, priority, version, admitted_at, done_at, created_at, updated_at";

pub(crate) fn ts(row: &Row<'_>, col: &str) -> Result<Timestamp> {
    Ok(Timestamp(row.get(col)?))
}

pub(crate) fn opt_ts(row: &Row<'_>, col: &str) -> Result<Option<Timestamp>> {
    let v: Option<i64> = row.get(col)?;
    Ok(v.map(Timestamp))
}

fn work_item(row: &Row<'_>) -> Result<WorkItem> {
    Ok(WorkItem {
        id: WorkItemId(uuid(row, "id")?),
        project_id: ProjectId(uuid(row, "project_id")?),
        origin: vocab(row, "origin")?,
        spec_id: opt_uuid(row, "spec_id")?.map(SpecId),
        spec_revision_id: opt_uuid(row, "spec_revision_id")?.map(SpecRevisionId),
        step_id: row.get("step_id")?,
        step_index: row
            .get::<_, Option<i64>>("step_index")?
            .map(u32::try_from)
            .transpose()
            .map_err(|_| StoreError::Corrupt("step_index".into()))?,
        task_key: row.get("task_key")?,
        work: row.get("work")?,
        repo_id: RepoId(uuid(row, "repo_id")?),
        branch: row.get("branch")?,
        state: vocab(row, "state")?,
        stage: opt_vocab(row, "stage")?,
        stage_phase: opt_vocab(row, "stage_phase")?,
        priority: row.get("priority")?,
        version: u64_col(row, "version")?,
        admitted_at: opt_ts(row, "admitted_at")?,
        done_at: opt_ts(row, "done_at")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

fn rows<T>(
    conn: &Connection,
    sql_select: &str,
    selection: &str,
    order: &str,
    bind: &[&dyn rusqlite::ToSql],
    map: impl Fn(&Row<'_>) -> Result<T>,
) -> Result<Vec<T>> {
    let sql = format!("{sql_select} WHERE work_item_id IN ({selection}) ORDER BY {order}");
    let mut stmt = conn.prepare(&sql)?;
    let mut out = Vec::new();
    let mut q = stmt.query(bind)?;
    while let Some(row) = q.next()? {
        out.push(map(row)?);
    }
    Ok(out)
}

pub fn load_tasks(conn: &Connection, set: TaskSet<'_>) -> Result<Vec<Task>> {
    let ids_json;
    let spec_id;
    let (selection, bind): (&str, Vec<&dyn rusqlite::ToSql>) = match set {
        TaskSet::Ids(ids) => {
            ids_json =
                serde_json::to_string(&ids.iter().map(|i| i.0.to_string()).collect::<Vec<_>>())?;
            ("SELECT value FROM json_each(?1)", vec![&ids_json])
        }
        TaskSet::Reconcile => (
            "SELECT id FROM work_item WHERE state NOT IN ('done', 'cancelled')
               OR spec_id IN (SELECT id FROM spec WHERE state = 'approved')",
            vec![],
        ),
        TaskSet::Spec(id) => {
            spec_id = id.0.to_string();
            (
                "SELECT id FROM work_item WHERE spec_id = ?1",
                vec![&spec_id],
            )
        }
    };

    let items: Vec<WorkItem> = {
        let sql = format!(
            "SELECT {WORK_ITEM_COLUMNS} FROM work_item WHERE id IN ({selection}) ORDER BY created_at, id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut q = stmt.query(bind.as_slice())?;
        let mut out = Vec::new();
        while let Some(row) = q.next()? {
            out.push(work_item(row)?);
        }
        out
    };
    let mut tasks: BTreeMap<WorkItemId, Task> = items
        .into_iter()
        .map(|item| {
            (
                item.id,
                Task {
                    item,
                    stage_run: None,
                    gate: None,
                    blocker: None,
                    change_request: None,
                    environment: None,
                    session: None,
                    permissions: Vec::new(),
                    attempts: BTreeMap::new(),
                    retired_runs: Vec::new(),
                },
            )
        })
        .collect();
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    let b = bind.as_slice();

    for run in rows(
        conn,
        "SELECT * FROM stage_run",
        selection,
        "created_at DESC, id DESC",
        b,
        stage_run,
    )? {
        if let Some(t) = tasks.get_mut(&run.work_item_id) {
            let n = t.attempts.entry(run.stage).or_insert(0);
            *n = (*n).max(run.attempt);
            if t.stage_run.is_none() {
                t.stage_run = Some(run);
            }
        }
    }
    for g in rows(
        conn,
        "SELECT * FROM gate_result",
        selection,
        "observed_at DESC, id DESC",
        b,
        gate,
    )? {
        if let Some(t) = tasks.get_mut(&g.work_item_id)
            && t.gate.is_none()
            && t.stage_run.as_ref().is_some_and(|r| r.id == g.stage_run_id)
        {
            t.gate = Some(g);
        }
    }
    for x in rows(
        conn,
        "SELECT * FROM blocker",
        selection,
        "opened_at DESC, id DESC",
        b,
        blocker,
    )? {
        if let Some(t) = tasks.get_mut(&x.work_item_id)
            && t.blocker.is_none()
        {
            t.blocker = Some(x);
        }
    }
    for c in rows(
        conn,
        "SELECT * FROM change_request",
        selection,
        "created_at DESC",
        b,
        change_request,
    )? {
        if let Some(t) = tasks.get_mut(&c.work_item_id) {
            t.change_request.get_or_insert(c);
        }
    }
    for e in rows(
        conn,
        "SELECT * FROM environment",
        selection,
        "created_at DESC, id DESC",
        b,
        environment,
    )? {
        if let Some(t) = tasks.get_mut(&e.work_item_id) {
            t.environment.get_or_insert(e);
        }
    }
    for s in rows(
        conn,
        "SELECT * FROM session",
        selection,
        "created_at DESC, id DESC",
        b,
        session,
    )? {
        if let Some(t) = tasks.get_mut(&s.work_item_id) {
            t.session.get_or_insert(s);
        }
    }
    let running: HashMap<SessionId, Timestamp> = running_since(conn, selection, b)?;
    let latest_sessions: HashMap<SessionId, WorkItemId> = tasks
        .values()
        .filter_map(|t| t.session.as_ref().map(|s| (s.id, t.item.id)))
        .collect();
    for t in tasks.values_mut() {
        if let Some(s) = t
            .session
            .as_mut()
            .filter(|s| s.state == SessionState::Running)
        {
            s.running_since = running.get(&s.id).copied();
        }
    }
    let open_permissions = "SELECT * FROM permission_request";
    for p in rows(
        conn,
        open_permissions,
        selection,
        "requested_at, id",
        b,
        permission,
    )? {
        let wanted = p.state == saka_core::vocab::PermissionState::Pending
            || (p.delivered_at.is_none()
                && matches!(
                    p.state,
                    saka_core::vocab::PermissionState::Allowed
                        | saka_core::vocab::PermissionState::Denied
                ));
        if wanted
            && latest_sessions.get(&p.session_id) == Some(&p.work_item_id)
            && let Some(t) = tasks.get_mut(&p.work_item_id)
        {
            t.permissions.push(p);
        }
    }
    Ok(tasks.into_values().collect())
}

pub fn load_task(conn: &Connection, id: WorkItemId) -> Result<Option<Task>> {
    Ok(load_tasks(conn, TaskSet::Ids(&[id]))?.into_iter().next())
}

pub fn session_work_item(conn: &Connection, session: SessionId) -> Result<Option<WorkItemId>> {
    owner(conn, "session", session.0)
}

pub fn permission_work_item(
    conn: &Connection,
    request: PermissionRequestId,
) -> Result<Option<WorkItemId>> {
    owner(conn, "permission_request", request.0)
}

fn owner(conn: &Connection, table: &'static str, id: Uuid) -> Result<Option<WorkItemId>> {
    let found: Option<String> = conn
        .query_row(
            &format!("SELECT work_item_id FROM {table} WHERE id = ?1"),
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    found
        .map(|s| {
            Uuid::parse_str(&s)
                .map(WorkItemId)
                .map_err(|_| StoreError::Corrupt(format!("{table}.work_item_id")))
        })
        .transpose()
}

fn running_since(
    conn: &Connection,
    selection: &str,
    bind: &[&dyn rusqlite::ToSql],
) -> Result<HashMap<SessionId, Timestamp>> {
    let sql = format!(
        "SELECT e.stream_id, e.occurred_at FROM event e
         JOIN session s ON s.id = e.stream_id AND s.state = 'running' AND s.work_item_id IN ({selection})
         WHERE e.stream_type = 'session' AND e.kind = 'runtime.session.state_changed'
           AND e.seq = (SELECT MAX(seq) FROM event WHERE stream_type = 'session' AND stream_id = e.stream_id
                        AND kind = 'runtime.session.state_changed')"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut q = stmt.query(bind)?;
    let mut out = HashMap::new();
    while let Some(row) = q.next()? {
        out.insert(SessionId(uuid(row, "stream_id")?), ts(row, "occurred_at")?);
    }
    Ok(out)
}

pub(crate) fn stage_run(row: &Row<'_>) -> Result<StageRun> {
    Ok(StageRun {
        id: StageRunId(uuid(row, "id")?),
        work_item_id: WorkItemId(uuid(row, "work_item_id")?),
        stage: vocab(row, "stage")?,
        attempt: u32_col(row, "attempt")?,
        state: vocab(row, "state")?,
        trigger: vocab(row, "trigger")?,
        reason: row.get("reason")?,
        entry_phase: vocab(row, "entry_phase")?,
        head_sha: row.get("head_sha")?,
        failure: opt_json(row, "failure")?,
        started_at: opt_ts(row, "started_at")?,
        ended_at: opt_ts(row, "ended_at")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

fn gate(row: &Row<'_>) -> Result<GateResult> {
    Ok(GateResult {
        id: GateResultId(uuid(row, "id")?),
        stage_run_id: StageRunId(uuid(row, "stage_run_id")?),
        work_item_id: WorkItemId(uuid(row, "work_item_id")?),
        source: vocab(row, "source")?,
        head_sha: row.get("head_sha")?,
        outcome: vocab(row, "outcome")?,
        checks: json(row, "checks")?,
        observed_at: ts(row, "observed_at")?,
    })
}

fn blocker(row: &Row<'_>) -> Result<Blocker> {
    Ok(Blocker {
        id: BlockerId(uuid(row, "id")?),
        work_item_id: WorkItemId(uuid(row, "work_item_id")?),
        stage_run_id: opt_uuid(row, "stage_run_id")?.map(StageRunId),
        code: vocab(row, "code")?,
        params: json(row, "params")?,
        resolution: opt_vocab(row, "resolution")?,
        opened_at: ts(row, "opened_at")?,
        resolved_at: opt_ts(row, "resolved_at")?,
    })
}

fn change_request(row: &Row<'_>) -> Result<ChangeRequest> {
    Ok(ChangeRequest {
        id: ChangeRequestId(uuid(row, "id")?),
        work_item_id: WorkItemId(uuid(row, "work_item_id")?),
        repo_id: RepoId(uuid(row, "repo_id")?),
        number: u64_col(row, "number")?,
        url: row.get("url")?,
        head_branch: row.get("head_branch")?,
        base_branch: row.get("base_branch")?,
        head_sha: row.get("head_sha")?,
        state: vocab(row, "state")?,
        merge_commit_sha: row.get("merge_commit_sha")?,
        merged_at: opt_ts(row, "merged_at")?,
        last_polled_at: opt_ts(row, "last_polled_at")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

fn environment(row: &Row<'_>) -> Result<Environment> {
    Ok(Environment {
        id: EnvironmentId(uuid(row, "id")?),
        work_item_id: WorkItemId(uuid(row, "work_item_id")?),
        repo_id: RepoId(uuid(row, "repo_id")?),
        root_path: row.get("root_path")?,
        worktree_path: row.get("worktree_path")?,
        branch: row.get("branch")?,
        base_sha: row.get("base_sha")?,
        state: vocab(row, "state")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

pub(crate) fn session(row: &Row<'_>) -> Result<Session> {
    Ok(Session {
        id: SessionId(uuid(row, "id")?),
        work_item_id: WorkItemId(uuid(row, "work_item_id")?),
        stage_run_id: StageRunId(uuid(row, "stage_run_id")?),
        environment_id: EnvironmentId(uuid(row, "environment_id")?),
        cli: row.get("cli")?,
        cli_version: row.get("cli_version")?,
        cli_session_id: row.get("cli_session_id")?,
        model: row.get("model")?,
        account: row.get("account")?,
        state: vocab(row, "state")?,
        runner_unit: row.get("runner_unit")?,
        socket_path: row.get("socket_path")?,
        log_path: row.get("log_path")?,
        turns: u32_col(row, "turns")?,
        followups: u32_col(row, "followups")?,
        running_ms: row.get("running_ms")?,
        running_since: None,
        cost_usd_micros: row.get("cost_usd_micros")?,
        input_tokens: row.get("input_tokens")?,
        output_tokens: row.get("output_tokens")?,
        end_reason: opt_json(row, "end_reason")?,
        started_at: opt_ts(row, "started_at")?,
        ended_at: opt_ts(row, "ended_at")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

pub(crate) fn permission(row: &Row<'_>) -> Result<PermissionRequest> {
    Ok(PermissionRequest {
        id: PermissionRequestId(uuid(row, "id")?),
        work_item_id: WorkItemId(uuid(row, "work_item_id")?),
        session_id: SessionId(uuid(row, "session_id")?),
        cli_request_id: row.get("cli_request_id")?,
        tool: row.get("tool")?,
        input: json(row, "input")?,
        summary: row.get("summary")?,
        state: vocab(row, "state")?,
        answer_source: row.get("answer_source")?,
        message: row.get("message")?,
        requested_at: ts(row, "requested_at")?,
        answered_at: opt_ts(row, "answered_at")?,
        delivered_at: opt_ts(row, "delivered_at")?,
    })
}

pub(crate) fn spec_row(row: &Row<'_>) -> Result<Spec> {
    Ok(Spec {
        id: SpecId(uuid(row, "id")?),
        project_id: ProjectId(uuid(row, "project_id")?),
        path: row.get("path")?,
        slug: row.get("slug")?,
        title: row.get("title")?,
        state: vocab(row, "state")?,
        origin: vocab(row, "origin")?,
        priority: row.get("priority")?,
        latest_revision_id: SpecRevisionId(uuid(row, "latest_revision_id")?),
        approved_revision_id: opt_uuid(row, "approved_revision_id")?.map(SpecRevisionId),
        approved_at: opt_ts(row, "approved_at")?,
        shipped_at: opt_ts(row, "shipped_at")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

pub(crate) fn one<T>(
    conn: &Connection,
    sql: &str,
    bind: &[&dyn rusqlite::ToSql],
    map: impl Fn(&Row<'_>) -> Result<T>,
) -> Result<Option<T>> {
    let mut stmt = conn.prepare(sql)?;
    let mut q = stmt.query(bind)?;
    match q.next()? {
        Some(row) => Ok(Some(map(row)?)),
        None => Ok(None),
    }
}

pub fn load_spec(conn: &Connection, id: SpecId) -> Result<Option<Spec>> {
    one(
        conn,
        "SELECT * FROM spec WHERE id = ?1",
        &[&id.0.to_string()],
        spec_row,
    )
}

pub fn spec_by_path(conn: &Connection, project: ProjectId, path: &str) -> Result<Option<Spec>> {
    one(
        conn,
        "SELECT * FROM spec WHERE project_id = ?1 AND path = ?2",
        &[&project.0.to_string(), &path],
        spec_row,
    )
}

pub fn reconcile_specs(conn: &Connection) -> Result<Vec<Spec>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM spec WHERE state = 'approved'
           OR id IN (SELECT depends_on_spec_id FROM spec_dependency)
         ORDER BY approved_at, id",
    )?;
    let mut q = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(spec_row(row)?);
    }
    Ok(out)
}

pub fn project_specs(conn: &Connection, project: ProjectId) -> Result<Vec<Spec>> {
    let mut stmt = conn.prepare("SELECT * FROM spec WHERE project_id = ?1 ORDER BY slug")?;
    let mut q = stmt.query([project.0.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(spec_row(row)?);
    }
    Ok(out)
}

pub fn spec_dependencies(
    conn: &Connection,
    project: Option<ProjectId>,
) -> Result<Vec<SpecDependency>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM spec_dependency WHERE ?1 IS NULL OR project_id = ?1
         ORDER BY spec_id, depends_on_slug",
    )?;
    let mut q = stmt.query([project.map(|p| p.0.to_string())])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(SpecDependency {
            spec_id: SpecId(uuid(row, "spec_id")?),
            depends_on_spec_id: SpecId(uuid(row, "depends_on_spec_id")?),
            depends_on_slug: row.get("depends_on_slug")?,
            revision_id: SpecRevisionId(uuid(row, "revision_id")?),
            project_id: ProjectId(uuid(row, "project_id")?),
        });
    }
    Ok(out)
}

pub fn queued_work_item_dependencies(conn: &Connection) -> Result<Vec<WorkItemDependency>> {
    work_item_dependencies(
        conn,
        "SELECT d.* FROM work_item_dependency d JOIN work_item w ON w.id = d.work_item_id
         WHERE w.state = 'queued' ORDER BY d.work_item_id, d.depends_on_work_item_id",
        &[],
    )
}

pub fn work_item_dependencies_of(
    conn: &Connection,
    ids: &[WorkItemId],
) -> Result<Vec<WorkItemDependency>> {
    let mut out = Vec::new();
    for id in ids {
        out.extend(work_item_dependencies(
            conn,
            "SELECT * FROM work_item_dependency WHERE work_item_id = ?1
             ORDER BY depends_on_work_item_id",
            &[&id.0.to_string()],
        )?);
    }
    Ok(out)
}

fn work_item_dependencies(
    conn: &Connection,
    sql: &str,
    bind: &[&dyn rusqlite::ToSql],
) -> Result<Vec<WorkItemDependency>> {
    let mut stmt = conn.prepare(sql)?;
    let mut q = stmt.query(bind)?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(WorkItemDependency {
            work_item_id: WorkItemId(uuid(row, "work_item_id")?),
            depends_on_work_item_id: WorkItemId(uuid(row, "depends_on_work_item_id")?),
            project_id: ProjectId(uuid(row, "project_id")?),
        });
    }
    Ok(out)
}

pub fn revision_by_hash(
    conn: &Connection,
    spec: SpecId,
    content_hash: &str,
    parser_version: u32,
) -> Result<Option<SpecRevisionId>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM spec_revision WHERE spec_id = ?1 AND content_hash = ?2 AND parser_version = ?3",
            params![spec.0.to_string(), content_hash, parser_version],
            |r| r.get(0),
        )
        .optional()?;
    id.map(|s| {
        Uuid::parse_str(&s)
            .map(SpecRevisionId)
            .map_err(|_| StoreError::Corrupt("spec_revision.id".into()))
    })
    .transpose()
}

pub fn load_revision(conn: &Connection, id: SpecRevisionId) -> Result<Option<SpecRevisionRow>> {
    one(
        conn,
        "SELECT * FROM spec_revision WHERE id = ?1",
        &[&id.0.to_string()],
        |row| {
            Ok(SpecRevisionRow {
                id: uuid(row, "id")?,
                spec_id: uuid(row, "spec_id")?,
                project_id: ProjectId(uuid(row, "project_id")?),
                source_commit: row.get("source_commit")?,
                content_hash: row.get("content_hash")?,
                content: row.get("content")?,
                lint_ok: row.get("lint_ok")?,
                lint_issues: json(row, "lint_issues")?,
                model: json(row, "model")?,
                parser_version: u32_col(row, "parser_version")?,
                created_at: ts(row, "created_at")?,
            })
        },
    )
}

pub fn revision_view(conn: &Connection, id: SpecRevisionId) -> Result<Option<RevisionView>> {
    let Some(r) = load_revision(conn, id)? else {
        return Ok(None);
    };
    let mut steps: Vec<StepPlan> = match r.model.get("steps") {
        Some(steps) => serde_json::from_value(steps.clone())?,
        None => Vec::new(),
    };
    // Version 1 models predate the After column: their steps always ran in order.
    if r.parser_version < 2 {
        for step in steps.iter_mut().filter(|s| s.index > 1) {
            step.after = vec![format!("S{}", step.index - 1)];
        }
    }
    let depends_on = match r.model.get("depends_on") {
        Some(list) => serde_json::from_value(list.clone())?,
        None => Vec::new(),
    };
    Ok(Some(RevisionView {
        id,
        lint_ok: r.lint_ok,
        issues: r.lint_issues,
        steps,
        parser_version: r.parser_version,
        priority: r.model.get("priority").and_then(Value::as_i64).unwrap_or(0),
        depends_on,
    }))
}

pub fn load_repo(conn: &Connection, id: RepoId) -> Result<Option<Repo>> {
    one(
        conn,
        "SELECT * FROM repo WHERE id = ?1",
        &[&id.0.to_string()],
        repo_row,
    )
}

pub fn repo_by_name(
    conn: &Connection,
    integration_id: &str,
    full_name: &str,
) -> Result<Option<Repo>> {
    one(
        conn,
        "SELECT * FROM repo WHERE integration_id = ?1 AND full_name = ?2",
        &[&integration_id, &full_name],
        repo_row,
    )
}

pub(crate) fn repo_row(row: &Row<'_>) -> Result<Repo> {
    Ok(Repo {
        id: RepoId(uuid(row, "id")?),
        integration_id: row.get("integration_id")?,
        provider: row.get("provider")?,
        full_name: row.get("full_name")?,
        url: row.get("url")?,
        default_branch: row.get("default_branch")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

pub fn load_project(conn: &Connection, id: ProjectId) -> Result<Option<Project>> {
    let Some(mut project) = one(
        conn,
        "SELECT * FROM project WHERE id = ?1",
        &[&id.0.to_string()],
        project_row,
    )?
    else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT repo_id, name, is_default, path_scope FROM project_repo WHERE project_id = ?1 ORDER BY is_default DESC, name",
    )?;
    let mut q = stmt.query([id.0.to_string()])?;
    while let Some(row) = q.next()? {
        project.repos.push(ProjectRepo {
            repo_id: RepoId(uuid(row, "repo_id")?),
            name: row.get("name")?,
            is_default: row.get("is_default")?,
            path_scope: json(row, "path_scope")?,
        });
    }
    Ok(Some(project))
}

pub(crate) fn project_row(row: &Row<'_>) -> Result<Project> {
    Ok(Project {
        id: ProjectId(uuid(row, "id")?),
        name: row.get("name")?,
        workspace_repo_id: RepoId(uuid(row, "workspace_repo_id")?),
        project_path: row.get("project_path")?,
        base_branch: row.get("base_branch")?,
        config_commit: row.get("config_commit")?,
        autonomy: vocab(row, "autonomy")?,
        workflow_preset: vocab(row, "workflow_preset")?,
        repos: Vec::new(),
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub seq: i64,
    pub id: Uuid,
    pub kind: String,
    pub version: u32,
    pub stream_type: String,
    pub stream_id: Uuid,
    pub project_id: Option<ProjectId>,
    pub actor: String,
    pub causation_id: Option<Uuid>,
    pub correlation_id: Uuid,
    pub occurred_at: Timestamp,
    pub payload: Value,
}

pub fn events_after(conn: &Connection, after: i64, limit: u32) -> Result<Vec<StoredEvent>> {
    let mut stmt = conn.prepare("SELECT * FROM event WHERE seq > ?1 ORDER BY seq LIMIT ?2")?;
    let mut q = stmt.query(params![after, limit])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(StoredEvent {
            seq: row.get("seq")?,
            id: uuid(row, "id")?,
            kind: row.get("kind")?,
            version: u32_col(row, "version")?,
            stream_type: row.get("stream_type")?,
            stream_id: uuid(row, "stream_id")?,
            project_id: opt_uuid(row, "project_id")?.map(ProjectId),
            actor: row.get("actor")?,
            causation_id: opt_uuid(row, "causation_id")?,
            correlation_id: uuid(row, "correlation_id")?,
            occurred_at: ts(row, "occurred_at")?,
            payload: json(row, "payload")?,
        });
    }
    Ok(out)
}

pub fn find_action(conn: &Connection, actor: &str, key: &str) -> Result<Option<ActionRecord>> {
    one(
        conn,
        "SELECT * FROM action_request WHERE actor = ?1 AND idempotency_key = ?2",
        &[&actor, &key],
        |row| {
            Ok(ActionRecord {
                id: uuid(row, "id")?,
                actor: row.get("actor")?,
                idempotency_key: row.get("idempotency_key")?,
                request_hash: row.get("request_hash")?,
                action: row.get("action")?,
                target_type: row.get("target_type")?,
                target_id: opt_uuid(row, "target_id")?,
                project_id: opt_uuid(row, "project_id")?.map(ProjectId),
                reason: row.get("reason")?,
                response_status: row.get("response_status")?,
                response_body: json(row, "response_body")?,
                created_at: ts(row, "created_at")?,
            })
        },
    )
}

pub fn attempts(conn: &Connection, id: WorkItemId) -> Result<BTreeMap<Stage, u32>> {
    let mut stmt = conn.prepare("SELECT stage, MAX(attempt) AS attempt FROM stage_run WHERE work_item_id = ?1 GROUP BY stage")?;
    let mut q = stmt.query([id.0.to_string()])?;
    let mut out = BTreeMap::new();
    while let Some(row) = q.next()? {
        out.insert(vocab(row, "stage")?, u32_col(row, "attempt")?);
    }
    Ok(out)
}
