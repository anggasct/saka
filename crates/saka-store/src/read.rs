use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, OptionalExtension, Row, params};
use saka_core::ids::{
    BlockerId, ChangeRequestId, EnvironmentId, GateResultId, PermissionRequestId, ProjectId,
    RepoId, SessionId, SpecDraftId, SpecId, SpecRevisionId, StageRunId, WorkItemId,
};
use saka_core::model::{
    AccountQuota, Blocker, ChangeRequest, ContextRef, Environment, ExternalChange, GateResult,
    PermissionRequest, PermissionRule, Project, ProjectRepo, Repo, RevisionView, RunOwner, Session,
    Spec, SpecDependency, SpecLink, StageRun, StepPlan, Task, WorkItem, WorkItemDependency,
};
use saka_core::time::Timestamp;
use saka_core::vocab::{ChangeRole, SessionState, Stage, Trigger};
use serde_json::Value;
use uuid::Uuid;

use crate::codec::{
    budget_col, json, opt_json, opt_uuid, opt_vocab, run_owner, u32_col, u64_col, uuid, vocab,
};
use crate::error::{Result, StoreError};
use crate::write::{ActionRecord, SecretRow, SpecRevisionRow};

#[derive(Debug, Clone, Copy)]
pub enum TaskSet<'a> {
    Ids(&'a [WorkItemId]),
    Reconcile,
    Spec(SpecId),
}

pub(crate) const SESSION_SELECT: &str = "SELECT s.*,
    (SELECT COUNT(*) FROM event e WHERE e.stream_type = 'session' AND e.stream_id = s.id
       AND e.kind = 'runtime.session.steered') AS steers,
    (SELECT ifnull(MAX(turn), 0) FROM usage_record u WHERE u.session_id = s.id) AS recorded_turn,
    (SELECT ifnull(SUM(running_ms), 0) FROM usage_record u WHERE u.session_id = s.id) AS recorded_running_ms
  FROM session s";

pub(crate) const ENVIRONMENT_SELECT: &str = "SELECT e.*,
    CASE WHEN e.work_item_id IS NOT NULL THEN e.id END AS stage_run_id
  FROM environment e";

pub(crate) const PERMISSION_SELECT: &str = "SELECT p.*,
    (SELECT s.stage_run_id FROM session s WHERE s.id = p.session_id) AS stage_run_id,
    (SELECT s.spec_draft_attempt_id FROM session s WHERE s.id = p.session_id) AS spec_draft_attempt_id
  FROM permission_request p";

const ACTIVITY_SELECT: &str = "SELECT a.*,
    (SELECT s.stage_run_id FROM session s WHERE s.id = a.session_id) AS stage_run_id,
    (SELECT s.spec_draft_attempt_id FROM session s WHERE s.id = a.session_id) AS spec_draft_attempt_id
  FROM session_activity a";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOf {
    WorkItem(WorkItemId),
    SpecDraft(SpecDraftId),
}

impl RunOf {
    fn column(self) -> &'static str {
        match self {
            RunOf::WorkItem(_) => "work_item_id",
            RunOf::SpecDraft(_) => "spec_draft_id",
        }
    }

    fn id(self) -> String {
        match self {
            RunOf::WorkItem(id) => id.to_string(),
            RunOf::SpecDraft(id) => id.to_string(),
        }
    }
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
    rows_of(
        conn,
        sql_select,
        "work_item_id",
        selection,
        order,
        bind,
        map,
    )
}

pub(crate) fn rows_of<T>(
    conn: &Connection,
    sql_select: &str,
    column: &str,
    selection: &str,
    order: &str,
    bind: &[&dyn rusqlite::ToSql],
    map: impl Fn(&Row<'_>) -> Result<T>,
) -> Result<Vec<T>> {
    let sql = format!("{sql_select} WHERE {column} IN ({selection}) ORDER BY {order}");
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
               OR spec_id IN (SELECT id FROM spec WHERE state = 'approved')
               OR id IN (SELECT work_item_id FROM environment WHERE state IN ('ready', 'teardown_failed'))",
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
        .map(|item| (item.id, Task::new(item)))
        .collect();
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    let b = bind.as_slice();

    let mut history: BTreeMap<WorkItemId, Vec<(Stage, Trigger, Option<String>)>> = BTreeMap::new();
    let mut previous: HashMap<WorkItemId, StageRunId> = HashMap::new();
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
            history.entry(run.work_item_id).or_default().push((
                run.stage,
                run.trigger,
                run.reason.clone(),
            ));
            if t.stage_run.is_none() {
                t.stage_run = Some(run);
            } else {
                previous.entry(run.work_item_id).or_insert(run.id);
            }
        }
    }
    for (id, runs) in history {
        if let Some(t) = tasks.get_mut(&id) {
            for (stage, trigger, reason) in runs.into_iter().rev() {
                saka_core::task::count_retry(&mut t.retries, stage, trigger, reason.as_deref());
            }
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
            match c.role {
                ChangeRole::Code => t.change_request.get_or_insert(c),
                ChangeRole::Docs => t.docs_change.get_or_insert(c),
            };
        }
    }
    for (id, external) in rows_of(
        conn,
        "SELECT e.*, p.workflow, s.slug AS spec_slug FROM external_change e JOIN project p ON p.id = e.project_id
           LEFT JOIN spec s ON s.id = e.spec_id",
        "e.work_item_id",
        selection,
        "e.created_at, e.work_item_id",
        b,
        external_change,
    )? {
        if let Some(t) = tasks.get_mut(&id) {
            t.external = Some(external);
        }
    }
    let mut seen = std::collections::HashSet::new();
    for g in rows(
        conn,
        "SELECT * FROM gate_result",
        selection,
        "observed_at DESC, id DESC",
        b,
        gate,
    )? {
        let Some(t) = tasks.get_mut(&g.work_item_id) else {
            continue;
        };
        // A sync run that has no verification target yet still carries the evidence it merged on.
        let carried = t
            .stage_run
            .as_ref()
            .is_some_and(|r| r.stage == Stage::Sync && r.head_sha.is_none());
        let owner = if carried {
            previous.get(&g.work_item_id).copied()
        } else {
            t.stage_run.as_ref().map(|r| r.id)
        };
        if owner != Some(g.stage_run_id) || !seen.insert((g.work_item_id, g.source)) {
            continue;
        }
        let merged_head = t.change_request.as_ref().map(|c| c.head_sha.as_str());
        if !carried || merged_head == Some(g.head_sha.as_str()) {
            let at = t.gates.partition_point(|x| x.source < g.source);
            t.gates.insert(at, g);
        }
    }
    for g in rows(
        conn,
        "SELECT * FROM (SELECT * FROM hook_run WHERE mode = 'gate')",
        selection,
        "queued_at, id",
        b,
        |row| {
            Ok((
                WorkItemId(uuid(row, "work_item_id")?),
                saka_core::model::HookGate {
                    run_id: saka_core::ids::HookRunId(uuid(row, "id")?),
                    hook: row.get("hook")?,
                    action: vocab(row, "action")?,
                    state: vocab(row, "state")?,
                    error: opt_json(row, "error")?,
                },
            ))
        },
    )? {
        let (work_item, gate) = g;
        if let Some(t) = tasks
            .get_mut(&work_item)
            .filter(|t| t.stage_run.as_ref().is_some_and(|r| r.stage == Stage::Sync))
        {
            t.hook_gates.retain(|x| x.hook != gate.hook);
            t.hook_gates.push(gate);
        }
    }
    for e in rows(
        conn,
        ENVIRONMENT_SELECT,
        selection,
        "created_at DESC, id DESC",
        b,
        environment,
    )? {
        if let Some(t) = e.owner.work_item_id().and_then(|id| tasks.get_mut(&id)) {
            t.environment.get_or_insert(e);
        }
    }
    for s in rows(
        conn,
        SESSION_SELECT,
        selection,
        "created_at DESC, id DESC",
        b,
        session,
    )? {
        if let Some(t) = s.owner.work_item_id().and_then(|id| tasks.get_mut(&id)) {
            match (&t.session, &mut t.stage_run) {
                (None, _) => t.session = Some(s),
                (Some(_), Some(run)) if s.owner.stage_run_id() == Some(run.id) => {
                    if s.resume_cause == Some(saka_core::vocab::ResumeCause::Lost) {
                        run.resumed_lost = true;
                    }
                    t.prior_usage.add(s.usage_at(Timestamp(0)));
                }
                _ => {}
            }
        }
    }
    for t in tasks.values_mut() {
        if let (Some(s), Some(run)) = (&t.session, &mut t.stage_run)
            && s.owner.stage_run_id() == Some(run.id)
            && s.resume_cause == Some(saka_core::vocab::ResumeCause::Lost)
        {
            run.resumed_lost = true;
        }
    }
    let running: HashMap<SessionId, Timestamp> = running_since(conn, "work_item_id", selection, b)?;
    let latest_sessions: HashMap<SessionId, WorkItemId> = tasks
        .values()
        .filter_map(|t| t.session.as_ref().map(|s| (s.id, t.item.id)))
        .collect();
    for t in tasks.values_mut() {
        if let Some(s) = t
            .session
            .as_mut()
            .filter(|s| matches!(s.state, SessionState::Running | SessionState::Interrupted))
        {
            s.running_since = running.get(&s.id).copied();
        }
    }
    for p in rows(
        conn,
        PERMISSION_SELECT,
        selection,
        "requested_at, id",
        b,
        permission,
    )? {
        let wanted = p.state == saka_core::vocab::PermissionState::Pending
            || (p.delivered_at.is_none() && saka_core::session::awaits_delivery(&p));
        let work_item = p.owner.work_item_id();
        if wanted
            && latest_sessions.get(&p.session_id).copied() == work_item
            && let Some(t) = work_item.and_then(|id| tasks.get_mut(&id))
        {
            t.permissions.push(p);
        }
    }
    Ok(tasks.into_values().collect())
}

pub fn load_task(conn: &Connection, id: WorkItemId) -> Result<Option<Task>> {
    Ok(load_tasks(conn, TaskSet::Ids(&[id]))?.into_iter().next())
}

pub fn blocker_work_item(conn: &Connection, id: BlockerId) -> Result<Option<WorkItemId>> {
    one(
        conn,
        "SELECT work_item_id FROM blocker WHERE id = ?1",
        &[&id.0.to_string()],
        |r| Ok(WorkItemId(uuid(r, "work_item_id")?)),
    )
}

pub fn session_owner(conn: &Connection, session: SessionId) -> Result<Option<RunOwner>> {
    one(
        conn,
        "SELECT work_item_id, stage_run_id, spec_draft_id, spec_draft_attempt_id FROM session WHERE id = ?1",
        &[&session.0.to_string()],
        run_owner,
    )
}

pub fn permission_owner(
    conn: &Connection,
    request: PermissionRequestId,
) -> Result<Option<RunOwner>> {
    one(
        conn,
        &format!("{PERMISSION_SELECT} WHERE p.id = ?1"),
        &[&request.0.to_string()],
        run_owner,
    )
}

pub(crate) fn running_since(
    conn: &Connection,
    column: &str,
    selection: &str,
    bind: &[&dyn rusqlite::ToSql],
) -> Result<HashMap<SessionId, Timestamp>> {
    let sql = format!(
        "SELECT e.stream_id, e.occurred_at FROM event e
         JOIN session s ON s.id = e.stream_id AND s.state IN ('running', 'interrupted')
           AND s.{column} IN ({selection})
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
        docs_head_sha: row.get("docs_head_sha")?,
        failure: opt_json(row, "failure")?,
        budget: budget_col(row, "budget")?,
        budget_warnings: json(row, "budget_warnings")?,
        resumed_lost: false,
        started_at: opt_ts(row, "started_at")?,
        ended_at: opt_ts(row, "ended_at")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

fn external_change(row: &Row<'_>) -> Result<(WorkItemId, ExternalChange)> {
    let spec_id = opt_uuid(row, "spec_id")?.map(SpecId);
    let step_id: Option<String> = row.get("step_id")?;
    Ok((
        WorkItemId(uuid(row, "work_item_id")?),
        ExternalChange {
            title: row.get("title")?,
            author: row.get("author")?,
            from_fork: row.get("from_fork")?,
            draft: row.get("draft")?,
            spec: spec_id.map(|spec_id| SpecLink { spec_id, step_id }),
            cause: vocab(row, "cause")?,
            actor: row.get("actor")?,
            created_at: ts(row, "created_at")?,
            spec_slug: row.get("spec_slug")?,
            workflow: json(row, "workflow")?,
        },
    ))
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
        role: vocab(row, "role")?,
        repo_id: RepoId(uuid(row, "repo_id")?),
        number: u64_col(row, "number")?,
        url: row.get("url")?,
        head_branch: row.get("head_branch")?,
        base_branch: row.get("base_branch")?,
        head_sha: row.get("head_sha")?,
        state: vocab(row, "state")?,
        merge_commit_sha: row.get("merge_commit_sha")?,
        merged_at: opt_ts(row, "merged_at")?,
        skipped_at: opt_ts(row, "skipped_at")?,
        last_polled_at: opt_ts(row, "last_polled_at")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

pub(crate) fn environment(row: &Row<'_>) -> Result<Environment> {
    Ok(Environment {
        id: EnvironmentId(uuid(row, "id")?),
        owner: run_owner(row)?,
        repo_id: RepoId(uuid(row, "repo_id")?),
        root_path: row.get("root_path")?,
        worktree_path: row.get("worktree_path")?,
        branch: row.get("branch")?,
        base_sha: row.get("base_sha")?,
        state: vocab(row, "state")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
        port_start: row.get("port_start")?,
        port_count: row.get("port_count")?,
        recipe: opt_json(row, "recipe_json")?,
        recipe_sha256: row.get("recipe_sha256")?,
        toolchains: json(row, "toolchains_json")?,
        path_scope: json(row, "path_scope_json")?,
        docs: opt_json(row, "docs_json")?,
    })
}

pub(crate) fn session(row: &Row<'_>) -> Result<Session> {
    Ok(Session {
        id: SessionId(uuid(row, "id")?),
        owner: run_owner(row)?,
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
        resumed_from_session_id: opt_uuid(row, "resumed_from_session_id")?.map(SessionId),
        resume_cause: opt_vocab(row, "resume_cause")?,
        idle_since: opt_ts(row, "idle_since")?,
        interrupt_requested: opt_vocab(row, "interrupt_requested")?,
        steers: optional_u32(row, "steers")?,
        last_offset: u64_col(row, "last_offset")?,
        recorded_turn: optional_u32(row, "recorded_turn")?,
        recorded_running_ms: optional_i64(row, "recorded_running_ms")?,
        started_at: opt_ts(row, "started_at")?,
        ended_at: opt_ts(row, "ended_at")?,
        created_at: ts(row, "created_at")?,
        updated_at: ts(row, "updated_at")?,
    })
}

fn optional_i64(row: &Row<'_>, col: &str) -> Result<i64> {
    match row.as_ref().column_index(col) {
        Ok(i) => Ok(row.get(i)?),
        Err(_) => Ok(0),
    }
}

fn optional_u32(row: &Row<'_>, col: &str) -> Result<u32> {
    let v = optional_i64(row, col)?;
    u32::try_from(v).map_err(|_| StoreError::Corrupt(format!("{col}: out of range: {v}")))
}

pub fn sessions(conn: &Connection, of: RunOf) -> Result<Vec<(Session, ProjectId)>> {
    let mut stmt = conn.prepare(&format!(
        "{SESSION_SELECT} WHERE s.{} = ?1 ORDER BY s.created_at, s.id",
        of.column()
    ))?;
    let mut q = stmt.query([of.id()])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push((session(row)?, ProjectId(uuid(row, "project_id")?)));
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineCursor {
    Start,
    After(u64),
    Before(u64),
    Tail,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelinePage {
    pub items: Vec<saka_core::model::SessionActivity>,
    pub prev_before: Option<u64>,
    pub next_after: Option<u64>,
}

fn sql_offset(offset: u64) -> i64 {
    i64::try_from(offset).unwrap_or(i64::MAX)
}

pub fn timeline(
    conn: &Connection,
    session: SessionId,
    cursor: TimelineCursor,
    limit: u32,
) -> Result<TimelinePage> {
    let id = session.0.to_string();
    let (sql, bound) = match cursor {
        TimelineCursor::Start => ("\"offset\" > ?2 ORDER BY \"offset\"", -1),
        TimelineCursor::After(after) => ("\"offset\" > ?2 ORDER BY \"offset\"", sql_offset(after)),
        TimelineCursor::Before(before) => (
            "\"offset\" < ?2 ORDER BY \"offset\" DESC",
            sql_offset(before),
        ),
        TimelineCursor::Tail => ("\"offset\" < ?2 ORDER BY \"offset\" DESC", i64::MAX),
    };
    let mut stmt = conn.prepare(&format!(
        "{ACTIVITY_SELECT} WHERE a.session_id = ?1 AND {sql} LIMIT ?3"
    ))?;
    let mut q = stmt.query(params![id, bound, limit])?;
    let mut items = Vec::new();
    while let Some(row) = q.next()? {
        items.push(activity(row)?);
    }
    if matches!(cursor, TimelineCursor::Before(_) | TimelineCursor::Tail) {
        items.reverse();
    }
    let exists = |cmp: &str, offset: u64| -> Result<bool> {
        Ok(conn.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM session_activity WHERE session_id = ?1 AND \"offset\" {cmp} ?2)"
            ),
            params![id, sql_offset(offset)],
            |r| r.get(0),
        )?)
    };
    let (prev_before, next_after) = match (items.first(), items.last()) {
        (Some(first), Some(last)) => (
            exists("<", first.offset)?.then_some(first.offset),
            exists(">", last.offset)?.then_some(last.offset),
        ),
        _ => (None, None),
    };
    Ok(TimelinePage {
        items,
        prev_before,
        next_after,
    })
}

pub fn activities_after(
    conn: &Connection,
    seq: i64,
    limit: u32,
) -> Result<Vec<(i64, saka_core::model::SessionActivity)>> {
    let mut stmt = conn.prepare(&format!(
        "{ACTIVITY_SELECT} WHERE a.seq > ?1 ORDER BY a.seq LIMIT ?2"
    ))?;
    let mut q = stmt.query(params![seq, limit])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push((row.get("seq")?, activity(row)?));
    }
    Ok(out)
}

pub fn last_activity_seq(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT ifnull(MAX(seq), 0) FROM session_activity",
        [],
        |r| r.get(0),
    )?)
}

fn activity(row: &Row<'_>) -> Result<saka_core::model::SessionActivity> {
    Ok(saka_core::model::SessionActivity {
        session_id: SessionId(uuid(row, "session_id")?),
        offset: u64_col(row, "offset")?,
        owner: run_owner(row)?,
        at: ts(row, "at")?,
        kind: row.get("kind")?,
        summary: row.get("summary")?,
        data: json(row, "data")?,
    })
}

pub fn usage_records(
    conn: &Connection,
    of: Option<RunOf>,
    session: Option<SessionId>,
) -> Result<Vec<(saka_core::model::UsageRecord, ProjectId)>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM usage_record
          WHERE (?1 IS NULL OR work_item_id = ?1) AND (?2 IS NULL OR spec_draft_id = ?2)
            AND (?3 IS NULL OR session_id = ?3)
         ORDER BY ended_at, id",
    )?;
    let (work_item, draft) = match of {
        Some(RunOf::WorkItem(id)) => (Some(id.to_string()), None),
        Some(RunOf::SpecDraft(id)) => (None, Some(id.to_string())),
        None => (None, None),
    };
    let mut q = stmt.query(params![work_item, draft, session.map(|s| s.0.to_string())])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        let project = ProjectId(uuid(row, "project_id")?);
        out.push((
            saka_core::model::UsageRecord {
                id: saka_core::ids::UsageRecordId(uuid(row, "id")?),
                owner: run_owner(row)?,
                session_id: SessionId(uuid(row, "session_id")?),
                account: row.get("account")?,
                turn: u32_col(row, "turn")?,
                status: row.get("status")?,
                input_tokens: row.get("input_tokens")?,
                output_tokens: row.get("output_tokens")?,
                cost_usd_micros: row.get("cost_usd_micros")?,
                running_ms: row.get("running_ms")?,
                ended_at: ts(row, "ended_at")?,
            },
            project,
        ));
    }
    Ok(out)
}

pub fn active_permission_rules(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<PermissionRule>> {
    let mut stmt = conn.prepare(
        "SELECT * FROM permission_rule WHERE project_id = ?1 AND revoked_at IS NULL ORDER BY created_at, id",
    )?;
    let mut q = stmt.query([project.0.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(permission_rule(row)?);
    }
    Ok(out)
}

pub fn load_permission_rule(
    conn: &Connection,
    id: saka_core::ids::PermissionRuleId,
) -> Result<Option<PermissionRule>> {
    one(
        conn,
        "SELECT * FROM permission_rule WHERE id = ?1",
        &[&id.0.to_string()],
        permission_rule,
    )
}

fn permission_rule(row: &Row<'_>) -> Result<PermissionRule> {
    Ok(PermissionRule {
        id: saka_core::ids::PermissionRuleId(uuid(row, "id")?),
        project_id: ProjectId(uuid(row, "project_id")?),
        tool: row.get("tool")?,
        command: row.get("command")?,
        permission_request_id: PermissionRequestId(uuid(row, "permission_request_id")?),
        created_by: row.get("created_by")?,
        created_at: ts(row, "created_at")?,
        revoked_at: opt_ts(row, "revoked_at")?,
        revoke_reason: row.get("revoke_reason")?,
    })
}

pub fn live_port_blocks(conn: &Connection) -> Result<Vec<(u16, u16)>> {
    let mut stmt = conn.prepare(
        "SELECT port_start, port_count FROM environment
          WHERE port_start IS NOT NULL AND state IN ('preparing', 'ready', 'teardown_failed')
          ORDER BY port_start",
    )?;
    let mut q = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push((row.get("port_start")?, row.get("port_count")?));
    }
    Ok(out)
}

pub fn account_quotas(conn: &Connection) -> Result<Vec<AccountQuota>> {
    let mut stmt = conn.prepare("SELECT * FROM account_quota ORDER BY account, window")?;
    let mut q = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(AccountQuota {
            account: row.get("account")?,
            window: row.get("window")?,
            utilization: row.get("utilization")?,
            status: row.get("status")?,
            resets_at: opt_ts(row, "resets_at")?,
            observed_at: ts(row, "observed_at")?,
        });
    }
    Ok(out)
}

pub(crate) fn permission(row: &Row<'_>) -> Result<PermissionRequest> {
    Ok(PermissionRequest {
        id: PermissionRequestId(uuid(row, "id")?),
        owner: run_owner(row)?,
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
    context_hash: &str,
    parser_version: u32,
) -> Result<Option<SpecRevisionId>> {
    let id: Option<String> = conn
        .query_row(
            "SELECT id FROM spec_revision
             WHERE spec_id = ?1 AND content_hash = ?2 AND context_hash = ?3 AND parser_version = ?4",
            params![spec.0.to_string(), content_hash, context_hash, parser_version],
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

pub fn revision_context(conn: &Connection, id: SpecRevisionId) -> Result<Vec<ContextRef>> {
    let mut stmt = conn.prepare(
        "SELECT path, origin, line, bytes, content_hash FROM spec_revision_context
         WHERE revision_id = ?1 ORDER BY ordinal",
    )?;
    let mut rows = stmt.query([id.0.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(ContextRef {
            path: row.get("path")?,
            origin: vocab(row, "origin")?,
            line: u32_col(row, "line")?,
            bytes: u64_col(row, "bytes")?,
            content_hash: row.get("content_hash")?,
        });
    }
    Ok(out)
}

pub fn context_contents(
    conn: &Connection,
    id: SpecRevisionId,
) -> Result<Vec<(ContextRef, Vec<u8>)>> {
    let mut stmt = conn.prepare(
        "SELECT c.path, c.origin, c.line, c.bytes, c.content_hash, b.content
         FROM spec_revision_context c
         JOIN context_blob b ON b.project_id = c.project_id AND b.content_hash = c.content_hash
         WHERE c.revision_id = ?1 ORDER BY c.ordinal",
    )?;
    let mut rows = stmt.query([id.0.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push((
            ContextRef {
                path: row.get("path")?,
                origin: vocab(row, "origin")?,
                line: u32_col(row, "line")?,
                bytes: u64_col(row, "bytes")?,
                content_hash: row.get("content_hash")?,
            },
            row.get("content")?,
        ));
    }
    Ok(out)
}

pub fn load_revision(conn: &Connection, id: SpecRevisionId) -> Result<Option<SpecRevisionRow>> {
    let Some(mut row) = one(
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
                context_hash: row.get("context_hash")?,
                context: Vec::new(),
                content: row.get("content")?,
                lint_ok: row.get("lint_ok")?,
                lint_issues: json(row, "lint_issues")?,
                model: json(row, "model")?,
                parser_version: u32_col(row, "parser_version")?,
                created_at: ts(row, "created_at")?,
            })
        },
    )?
    else {
        return Ok(None);
    };
    row.context = revision_context(conn, id)?;
    Ok(Some(row))
}

pub fn revision_view(conn: &Connection, id: SpecRevisionId) -> Result<Option<RevisionView>> {
    let Some(r) = load_revision(conn, id)? else {
        return Ok(None);
    };
    revision_view_of(id, r.lint_ok, &r.lint_issues, &r.model, r.parser_version).map(Some)
}

pub fn revision_view_of(
    id: SpecRevisionId,
    lint_ok: bool,
    issues: &Value,
    model: &Value,
    parser_version: u32,
) -> Result<RevisionView> {
    let mut steps: Vec<StepPlan> = match model.get("steps") {
        Some(steps) => serde_json::from_value(steps.clone())?,
        None => Vec::new(),
    };
    // Version 1 models predate the After column: their steps always ran in order.
    if parser_version < 2 {
        for step in steps.iter_mut().filter(|s| s.index > 1) {
            step.after = vec![format!("S{}", step.index - 1)];
        }
    }
    let depends_on = match model.get("depends_on") {
        Some(list) => serde_json::from_value(list.clone())?,
        None => Vec::new(),
    };
    Ok(RevisionView {
        id,
        lint_ok,
        issues: issues.clone(),
        steps,
        parser_version,
        priority: model.get("priority").and_then(Value::as_i64).unwrap_or(0),
        depends_on,
    })
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

fn secret_row(row: &Row<'_>) -> Result<SecretRow> {
    Ok(SecretRow {
        project_id: ProjectId(uuid(row, "project_id")?),
        name: row.get("name")?,
        nonce: row.get("nonce")?,
        ciphertext: row.get("ciphertext")?,
        created_at: Timestamp(row.get("created_at")?),
        updated_at: Timestamp(row.get("updated_at")?),
    })
}

pub fn project_secrets(conn: &Connection, project: ProjectId) -> Result<Vec<SecretRow>> {
    let mut stmt = conn.prepare("SELECT * FROM secret WHERE project_id = ?1 ORDER BY name")?;
    let mut q = stmt.query([project.0.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = q.next()? {
        out.push(secret_row(row)?);
    }
    Ok(out)
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
        "SELECT repo_id, name, is_default, base_branch, path_scope FROM project_repo WHERE project_id = ?1
         ORDER BY is_default DESC, name",
    )?;
    let mut q = stmt.query([id.0.to_string()])?;
    while let Some(row) = q.next()? {
        project.repos.push(ProjectRepo {
            repo_id: RepoId(uuid(row, "repo_id")?),
            name: row.get("name")?,
            is_default: row.get("is_default")?,
            base_branch: row.get("base_branch")?,
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
        config_sha256: row.get("config_sha256")?,
        config_error: opt_json(row, "config_error")?,
        autonomy: vocab(row, "autonomy")?,
        workflow: json(row, "workflow")?,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCandidate {
    pub change_request_id: ChangeRequestId,
    pub work_item_id: WorkItemId,
    pub project_id: ProjectId,
    pub repo_id: RepoId,
    pub number: u64,
    pub head_sha: String,
}

pub fn webhook_delivery_seen(
    conn: &Connection,
    integration_id: &str,
    delivery_id: &str,
) -> Result<Option<u32>> {
    one(
        conn,
        "SELECT change_requests FROM webhook_delivery WHERE integration_id = ?1 AND delivery_id = ?2",
        &[&integration_id, &delivery_id],
        |row| u32_col(row, "change_requests"),
    )
}

pub fn last_webhook_received_at(
    conn: &Connection,
    integration_id: &str,
) -> Result<Option<Timestamp>> {
    let at: Option<i64> = conn.query_row(
        "SELECT MAX(received_at) FROM webhook_delivery WHERE integration_id = ?1",
        [integration_id],
        |row| row.get(0),
    )?;
    Ok(at.map(Timestamp))
}

pub fn hook_candidates(conn: &Connection, repo: RepoId) -> Result<Vec<HookCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT cr.id, cr.work_item_id, cr.project_id, cr.repo_id, cr.number, cr.head_sha
         FROM change_request cr JOIN work_item w ON w.id = cr.work_item_id
         WHERE cr.repo_id = ?1 AND cr.state = 'open'
           AND ((w.state = 'in_stage' AND (w.stage = 'dev' OR w.origin = 'external_change'
                  OR (w.stage = 'merge' AND w.stage_phase IN ('awaiting_ci', 'awaiting_merge'))))
                OR w.state = 'blocked')
         ORDER BY cr.number",
    )?;
    let mut rows = stmt.query([repo.0.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(HookCandidate {
            change_request_id: ChangeRequestId(uuid(row, "id")?),
            work_item_id: WorkItemId(uuid(row, "work_item_id")?),
            project_id: ProjectId(uuid(row, "project_id")?),
            repo_id: RepoId(uuid(row, "repo_id")?),
            number: u64_col(row, "number")?,
            head_sha: row.get("head_sha")?,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedChange {
    pub number: u64,
    pub work_item_id: WorkItemId,
    pub origin: saka_core::vocab::WorkItemOrigin,
    pub state: saka_core::vocab::WorkItemState,
}

pub fn tracked_changes(conn: &Connection, repo: RepoId) -> Result<Vec<TrackedChange>> {
    let mut stmt = conn.prepare(
        "SELECT cr.number, w.id, w.origin, w.state FROM change_request cr
         JOIN work_item w ON w.id = cr.work_item_id
         WHERE cr.repo_id = ?1 AND cr.role = 'code' ORDER BY cr.number",
    )?;
    let mut rows = stmt.query([repo.0.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(TrackedChange {
            number: u64_col(row, "number")?,
            work_item_id: WorkItemId(uuid(row, "id")?),
            origin: vocab(row, "origin")?,
            state: vocab(row, "state")?,
        });
    }
    Ok(out)
}

pub fn spec_step_branches(conn: &Connection, repo: RepoId) -> Result<Vec<(String, WorkItemId)>> {
    let mut stmt = conn
        .prepare("SELECT branch, id FROM work_item WHERE repo_id = ?1 AND origin = 'spec_step'")?;
    let mut rows = stmt.query([repo.0.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push((row.get("branch")?, WorkItemId(uuid(row, "id")?)));
    }
    Ok(out)
}

pub fn change_cursors(
    conn: &Connection,
    repo: RepoId,
) -> Result<Vec<crate::write::ChangeCursorRow>> {
    let mut stmt = conn.prepare(
        "SELECT number, updated_at, through, seen_ids FROM change_signal_cursor WHERE repo_id = ?1",
    )?;
    let mut rows = stmt.query([repo.0.to_string()])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(crate::write::ChangeCursorRow {
            repo_id: repo,
            number: u64_col(row, "number")?,
            updated_at: ts(row, "updated_at")?,
            through: ts(row, "through")?,
            seen_ids: json(row, "seen_ids")?,
        });
    }
    Ok(out)
}
