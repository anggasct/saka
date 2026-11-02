use rusqlite::{Transaction, params};
use saka_core::ids::{ProjectId, SpecId, WorkItemId};
use saka_core::job::Job;
use saka_core::model::{
    AccountQuota, Blocker, ChangeRequest, Environment, GateResult, PermissionRequest,
    PermissionRule, Project, Repo, Session, SessionActivity, Spec, SpecDependency, StageRun, Task,
    UsageRecord, WorkItemDependency,
};
use saka_core::time::Timestamp;
use serde_json::Value;
use uuid::Uuid;

use crate::codec::{budget_json, i64_of, to_json, value_json};
use crate::error::{Result, StoreError};

#[derive(Debug, Clone, PartialEq)]
pub struct EventRecord {
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

#[derive(Debug, Clone, PartialEq)]
pub struct ActionRecord {
    pub id: Uuid,
    pub actor: String,
    pub idempotency_key: String,
    pub request_hash: String,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<Uuid>,
    pub project_id: Option<ProjectId>,
    pub reason: Option<String>,
    pub response_status: u16,
    pub response_body: Value,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpecRevisionRow {
    pub id: Uuid,
    pub spec_id: Uuid,
    pub project_id: ProjectId,
    pub source_commit: String,
    pub content_hash: String,
    pub content: Vec<u8>,
    pub lint_ok: bool,
    pub lint_issues: Value,
    pub model: Value,
    pub parser_version: u32,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewJob {
    pub id: Uuid,
    pub job: Job,
    pub project_id: Option<ProjectId>,
    pub causation_id: Option<Uuid>,
    pub correlation_id: Uuid,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobOutcome {
    Succeeded {
        id: Uuid,
        owner: String,
    },
    Retry {
        id: Uuid,
        owner: String,
        error: Value,
        run_at: Timestamp,
    },
    Dead {
        id: Uuid,
        owner: String,
        error: Value,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Batch {
    pub now: Timestamp,
    pub job_outcome: Option<JobOutcome>,
    pub action: Option<ActionRecord>,
    pub events: Vec<EventRecord>,
    pub repos: Vec<Repo>,
    pub projects: Vec<Project>,
    pub specs: Vec<Spec>,
    pub revisions: Vec<SpecRevisionRow>,
    pub tasks: Vec<Task>,
    pub spec_dependencies: Vec<SpecDependency>,
    pub work_item_dependencies: Vec<WorkItemDependency>,
    pub replaced_spec_dependencies: Vec<SpecId>,
    pub replaced_work_item_dependencies: Vec<WorkItemId>,
    pub permission_rules: Vec<PermissionRule>,
    pub activities: Vec<(ProjectId, SessionActivity)>,
    pub session_offsets: Vec<(saka_core::ids::SessionId, u64)>,
    pub jobs: Vec<NewJob>,
}

impl Batch {
    pub fn new(now: Timestamp) -> Batch {
        Batch {
            now,
            ..Batch::default()
        }
    }
}

pub(crate) fn write(tx: &Transaction<'_>, b: &Batch) -> Result<Vec<i64>> {
    // Events are written before the rows they describe; foreign keys are checked at commit.
    tx.pragma_update(None, "defer_foreign_keys", "ON")?;
    if let Some(outcome) = &b.job_outcome {
        job_outcome(tx, outcome, b.now)?;
    }
    if let Some(a) = &b.action {
        tx.execute(
            "INSERT INTO action_request (id, actor, idempotency_key, request_hash, action, target_type, target_id,
               project_id, reason, response_status, response_body, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                a.id.to_string(),
                a.actor,
                a.idempotency_key,
                a.request_hash,
                a.action,
                a.target_type,
                a.target_id.map(|id| id.to_string()),
                a.project_id.map(|id| id.to_string()),
                a.reason,
                a.response_status,
                value_json(&a.response_body)?,
                a.created_at.0,
            ],
        )?;
    }
    let mut seqs = Vec::with_capacity(b.events.len());
    for e in &b.events {
        tx.execute(
            "INSERT INTO event (id, kind, version, stream_type, stream_id, project_id, actor, causation_id,
               correlation_id, occurred_at, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                e.id.to_string(),
                e.kind,
                e.version,
                e.stream_type,
                e.stream_id.to_string(),
                e.project_id.map(|id| id.to_string()),
                e.actor,
                e.causation_id.map(|id| id.to_string()),
                e.correlation_id.to_string(),
                e.occurred_at.0,
                value_json(&e.payload)?,
            ],
        )?;
        seqs.push(tx.last_insert_rowid());
    }
    for r in &b.repos {
        repo(tx, r)?;
    }
    for p in &b.projects {
        project(tx, p)?;
    }
    for s in &b.specs {
        spec(tx, s)?;
    }
    for r in &b.revisions {
        revision(tx, r)?;
    }
    for t in &b.tasks {
        task(tx, t)?;
    }
    for id in &b.replaced_spec_dependencies {
        tx.execute(
            "DELETE FROM spec_dependency WHERE spec_id = ?1",
            [id.to_string()],
        )?;
    }
    for id in &b.replaced_work_item_dependencies {
        tx.execute(
            "DELETE FROM work_item_dependency WHERE work_item_id = ?1",
            [id.to_string()],
        )?;
    }
    for d in &b.spec_dependencies {
        tx.execute(
            "INSERT INTO spec_dependency (spec_id, depends_on_spec_id, depends_on_slug, revision_id, project_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (spec_id, depends_on_spec_id) DO NOTHING",
            params![
                d.spec_id.to_string(),
                d.depends_on_spec_id.to_string(),
                d.depends_on_slug,
                d.revision_id.to_string(),
                d.project_id.to_string(),
                b.now.0,
            ],
        )?;
    }
    for d in &b.work_item_dependencies {
        tx.execute(
            "INSERT INTO work_item_dependency (work_item_id, depends_on_work_item_id, project_id, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (work_item_id, depends_on_work_item_id) DO NOTHING",
            params![
                d.work_item_id.to_string(),
                d.depends_on_work_item_id.to_string(),
                d.project_id.to_string(),
                b.now.0,
            ],
        )?;
    }
    for r in &b.permission_rules {
        permission_rule(tx, r)?;
    }
    for (project, a) in &b.activities {
        tx.execute(
            "INSERT INTO session_activity (session_id, \"offset\", work_item_id, project_id, at, kind, summary, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (session_id, \"offset\") DO NOTHING",
            params![
                a.session_id.to_string(),
                i64_of(a.offset)?,
                a.work_item_id.to_string(),
                project.to_string(),
                a.at.0,
                a.kind,
                a.summary,
                value_json(&a.data)?,
            ],
        )?;
    }
    for (session, offset) in &b.session_offsets {
        tx.execute(
            "UPDATE session SET last_offset = max(last_offset, ?2) WHERE id = ?1",
            params![session.to_string(), i64_of(*offset)?],
        )?;
    }
    for j in &b.jobs {
        enqueue(tx, j, b.now)?;
    }
    Ok(seqs)
}

fn job_outcome(tx: &Transaction<'_>, outcome: &JobOutcome, now: Timestamp) -> Result<()> {
    let (id, changed) = match outcome {
        JobOutcome::Succeeded { id, owner } => (
            id,
            tx.execute(
                "UPDATE job SET state = 'succeeded', lease_owner = NULL, lease_until = NULL, updated_at = ?3
                 WHERE id = ?1 AND state = 'leased' AND lease_owner = ?2",
                params![id.to_string(), owner, now.0],
            )?,
        ),
        JobOutcome::Retry { id, owner, error, run_at } => (
            id,
            tx.execute(
                "UPDATE job SET state = 'failed', lease_owner = NULL, lease_until = NULL, run_at = ?3,
                   last_error = ?4, updated_at = ?5
                 WHERE id = ?1 AND state = 'leased' AND lease_owner = ?2",
                params![id.to_string(), owner, run_at.0, value_json(error)?, now.0],
            )?,
        ),
        JobOutcome::Dead { id, owner, error } => (
            id,
            tx.execute(
                "UPDATE job SET state = 'dead', lease_owner = NULL, lease_until = NULL, last_error = ?3, updated_at = ?4
                 WHERE id = ?1 AND state = 'leased' AND lease_owner = ?2",
                params![id.to_string(), owner, value_json(error)?, now.0],
            )?,
        ),
    };
    if changed == 1 {
        Ok(())
    } else {
        Err(StoreError::LeaseLost {
            job: id.to_string(),
        })
    }
}

fn enqueue(tx: &Transaction<'_>, j: &NewJob, now: Timestamp) -> Result<()> {
    tx.execute(
        "INSERT INTO job (id, kind, idempotency_key, project_id, payload, state, run_at, attempts, max_attempts,
           causation_id, correlation_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, 0, ?7, ?8, ?9, ?10, ?10)
         ON CONFLICT (idempotency_key) DO NOTHING",
        params![
            j.id.to_string(),
            j.job.kind.as_str(),
            j.job.idempotency_key,
            j.project_id.map(|id| id.to_string()),
            value_json(&j.job.payload)?,
            j.job.run_at.0,
            j.job.max_attempts(),
            j.causation_id.map(|id| id.to_string()),
            j.correlation_id.to_string(),
            now.0,
        ],
    )?;
    Ok(())
}

fn repo(tx: &Transaction<'_>, r: &Repo) -> Result<()> {
    tx.execute(
        "INSERT INTO repo (id, integration_id, provider, full_name, url, default_branch, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (id) DO UPDATE SET url = excluded.url, default_branch = excluded.default_branch,
           updated_at = excluded.updated_at",
        params![r.id.to_string(), r.integration_id, r.provider, r.full_name, r.url, r.default_branch, r.created_at.0, r.updated_at.0],
    )?;
    Ok(())
}

fn project(tx: &Transaction<'_>, p: &Project) -> Result<()> {
    tx.execute(
        "INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, autonomy,
           workflow_preset, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (id) DO UPDATE SET name = excluded.name, base_branch = excluded.base_branch,
           config_commit = excluded.config_commit, autonomy = excluded.autonomy,
           workflow_preset = excluded.workflow_preset, updated_at = excluded.updated_at",
        params![
            p.id.to_string(),
            p.name,
            p.workspace_repo_id.to_string(),
            p.project_path,
            p.base_branch,
            p.config_commit,
            p.autonomy.as_str(),
            p.workflow_preset.as_str(),
            p.created_at.0,
            p.updated_at.0,
        ],
    )?;
    for r in &p.repos {
        tx.execute(
            "INSERT INTO project_repo (project_id, repo_id, name, is_default, path_scope, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT (project_id, repo_id) DO UPDATE SET name = excluded.name, is_default = excluded.is_default,
               path_scope = excluded.path_scope, updated_at = excluded.updated_at",
            params![p.id.to_string(), r.repo_id.to_string(), r.name, r.is_default, to_json(&r.path_scope)?, p.updated_at.0],
        )?;
    }
    Ok(())
}

fn spec(tx: &Transaction<'_>, s: &Spec) -> Result<()> {
    tx.execute(
        "INSERT INTO spec (id, project_id, path, slug, title, state, origin, priority, latest_revision_id,
           approved_revision_id, approved_at, shipped_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (id) DO UPDATE SET title = excluded.title, state = excluded.state, priority = excluded.priority,
           latest_revision_id = excluded.latest_revision_id, approved_revision_id = excluded.approved_revision_id,
           approved_at = excluded.approved_at, shipped_at = excluded.shipped_at, updated_at = excluded.updated_at",
        params![
            s.id.to_string(),
            s.project_id.to_string(),
            s.path,
            s.slug,
            s.title,
            s.state.as_str(),
            s.origin.as_str(),
            s.priority,
            s.latest_revision_id.to_string(),
            s.approved_revision_id.map(|id| id.to_string()),
            s.approved_at.map(|t| t.0),
            s.shipped_at.map(|t| t.0),
            s.created_at.0,
            s.updated_at.0,
        ],
    )?;
    Ok(())
}

fn revision(tx: &Transaction<'_>, r: &SpecRevisionRow) -> Result<()> {
    tx.execute(
        "INSERT INTO spec_revision (id, spec_id, project_id, source_commit, content_hash, content, lint_ok,
           lint_issues, model, parser_version, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (id) DO NOTHING",
        params![
            r.id.to_string(),
            r.spec_id.to_string(),
            r.project_id.to_string(),
            r.source_commit,
            r.content_hash,
            r.content,
            r.lint_ok,
            value_json(&r.lint_issues)?,
            value_json(&r.model)?,
            r.parser_version,
            r.created_at.0,
        ],
    )?;
    Ok(())
}

fn task(tx: &Transaction<'_>, t: &Task) -> Result<()> {
    let i = &t.item;
    tx.execute(
        "INSERT INTO work_item (id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work,
           repo_id, branch, state, stage, stage_phase, priority, version, admitted_at, done_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
         ON CONFLICT (id) DO UPDATE SET spec_revision_id = excluded.spec_revision_id,
           step_index = excluded.step_index, work = excluded.work, state = excluded.state, stage = excluded.stage,
           stage_phase = excluded.stage_phase, priority = excluded.priority, version = excluded.version,
           admitted_at = excluded.admitted_at, done_at = excluded.done_at, updated_at = excluded.updated_at",
        params![
            i.id.to_string(),
            i.project_id.to_string(),
            i.origin.as_str(),
            i.spec_id.map(|id| id.to_string()),
            i.spec_revision_id.map(|id| id.to_string()),
            i.step_id,
            i.step_index,
            i.task_key,
            i.work,
            i.repo_id.to_string(),
            i.branch,
            i.state.as_str(),
            i.stage.map(|s| s.as_str()),
            i.stage_phase.map(|p| p.as_str()),
            i.priority,
            i64_of(i.version)?,
            i.admitted_at.map(|t| t.0),
            i.done_at.map(|t| t.0),
            i.created_at.0,
            i.updated_at.0,
        ],
    )?;
    for run in t.retired_runs.iter().chain(t.stage_run.iter()) {
        stage_run(tx, t, run)?;
    }
    if let Some(g) = &t.gate {
        gate(tx, t, g)?;
    }
    if let Some(b) = &t.blocker {
        blocker(tx, t, b)?;
    }
    if let Some(cr) = &t.change_request {
        change_request(tx, t, cr)?;
        if let Some(g) = t.gate.as_ref().filter(|g| g.head_sha == cr.head_sha) {
            for c in &g.checks {
                tx.execute(
                    "INSERT INTO ci_check (change_request_id, head_sha, kind, name, status, conclusion, url, observed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT (change_request_id, head_sha, kind, name) DO UPDATE SET status = excluded.status,
                       conclusion = excluded.conclusion, url = excluded.url, observed_at = excluded.observed_at",
                    params![
                        cr.id.to_string(),
                        g.head_sha,
                        c.kind.as_str(),
                        c.name,
                        c.status.as_str(),
                        c.conclusion.map(|x| x.as_str()),
                        c.url,
                        g.observed_at.0,
                    ],
                )?;
            }
        }
    }
    if let Some(e) = &t.environment {
        environment(tx, t, e)?;
    }
    for s in t.retired_sessions.iter().chain(t.session.iter()) {
        session(tx, t, s)?;
    }
    for p in &t.permissions {
        permission(tx, t, p)?;
    }
    for u in &t.usage_records {
        usage_record(tx, t, u)?;
    }
    for q in &t.quota_observations {
        account_quota(tx, q)?;
    }
    for r in &t.added_rules {
        permission_rule(tx, r)?;
    }
    Ok(())
}

fn usage_record(tx: &Transaction<'_>, t: &Task, u: &UsageRecord) -> Result<()> {
    tx.execute(
        "INSERT INTO usage_record (id, project_id, work_item_id, stage_run_id, session_id, account, turn, status,
           input_tokens, output_tokens, cost_usd_micros, running_ms, ended_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT (session_id, turn) DO NOTHING",
        params![
            u.id.to_string(),
            t.item.project_id.to_string(),
            u.work_item_id.to_string(),
            u.stage_run_id.to_string(),
            u.session_id.to_string(),
            u.account,
            u.turn,
            u.status,
            u.input_tokens,
            u.output_tokens,
            u.cost_usd_micros,
            u.running_ms,
            u.ended_at.0,
        ],
    )?;
    Ok(())
}

fn account_quota(tx: &Transaction<'_>, q: &AccountQuota) -> Result<()> {
    tx.execute(
        "INSERT INTO account_quota (account, window, utilization, status, resets_at, observed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (account, window) DO UPDATE SET utilization = excluded.utilization, status = excluded.status,
           resets_at = excluded.resets_at, observed_at = excluded.observed_at
         WHERE excluded.observed_at >= account_quota.observed_at",
        params![
            q.account,
            q.window,
            q.utilization.max(0.0),
            q.status,
            q.resets_at.map(|x| x.0),
            q.observed_at.0,
        ],
    )?;
    Ok(())
}

fn permission_rule(tx: &Transaction<'_>, r: &PermissionRule) -> Result<()> {
    tx.execute(
        "INSERT INTO permission_rule (id, project_id, effect, tool, command, permission_request_id, created_by,
           created_at, revoked_at, revoke_reason)
         VALUES (?1, ?2, 'allow', ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (id) DO UPDATE SET revoked_at = excluded.revoked_at, revoke_reason = excluded.revoke_reason",
        params![
            r.id.to_string(),
            r.project_id.to_string(),
            r.tool,
            r.command,
            r.permission_request_id.to_string(),
            r.created_by,
            r.created_at.0,
            r.revoked_at.map(|x| x.0),
            r.revoke_reason,
        ],
    )?;
    Ok(())
}

fn stage_run(tx: &Transaction<'_>, t: &Task, r: &StageRun) -> Result<()> {
    tx.execute(
        "INSERT INTO stage_run (id, work_item_id, project_id, stage, attempt, state, trigger, reason, entry_phase,
           head_sha, failure, started_at, ended_at, created_at, updated_at, budget, budget_warnings)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT (id) DO UPDATE SET state = excluded.state, head_sha = excluded.head_sha,
           failure = excluded.failure, started_at = excluded.started_at, ended_at = excluded.ended_at,
           updated_at = excluded.updated_at, budget_warnings = excluded.budget_warnings",
        params![
            r.id.to_string(),
            r.work_item_id.to_string(),
            t.item.project_id.to_string(),
            r.stage.as_str(),
            r.attempt,
            r.state.as_str(),
            r.trigger.as_str(),
            r.reason,
            r.entry_phase.as_str(),
            r.head_sha,
            r.failure.as_ref().map(to_json).transpose()?,
            r.started_at.map(|x| x.0),
            r.ended_at.map(|x| x.0),
            r.created_at.0,
            r.updated_at.0,
            budget_json(r.budget.as_ref())?,
            to_json(&r.budget_warnings)?,
        ],
    )?;
    Ok(())
}

fn gate(tx: &Transaction<'_>, t: &Task, g: &GateResult) -> Result<()> {
    tx.execute(
        "INSERT INTO gate_result (id, stage_run_id, work_item_id, project_id, source, head_sha, outcome, checks, observed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (id) DO NOTHING",
        params![
            g.id.to_string(),
            g.stage_run_id.to_string(),
            g.work_item_id.to_string(),
            t.item.project_id.to_string(),
            g.source.as_str(),
            g.head_sha,
            g.outcome.as_str(),
            to_json(&g.checks)?,
            g.observed_at.0,
        ],
    )?;
    Ok(())
}

fn blocker(tx: &Transaction<'_>, t: &Task, b: &Blocker) -> Result<()> {
    tx.execute(
        "INSERT INTO blocker (id, work_item_id, stage_run_id, project_id, code, params, state, resolution, opened_at, resolved_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (id) DO UPDATE SET state = excluded.state, resolution = excluded.resolution,
           resolved_at = excluded.resolved_at",
        params![
            b.id.to_string(),
            b.work_item_id.to_string(),
            b.stage_run_id.map(|id| id.to_string()),
            t.item.project_id.to_string(),
            b.code.as_str(),
            value_json(&b.params)?,
            if b.resolution.is_some() { "resolved" } else { "open" },
            b.resolution.map(|r| r.as_str()),
            b.opened_at.0,
            b.resolved_at.map(|x| x.0),
        ],
    )?;
    Ok(())
}

fn change_request(tx: &Transaction<'_>, t: &Task, c: &ChangeRequest) -> Result<()> {
    tx.execute(
        "INSERT INTO change_request (id, project_id, work_item_id, repo_id, number, url, head_branch, base_branch,
           head_sha, state, merge_commit_sha, merged_at, last_polled_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT (id) DO UPDATE SET head_sha = excluded.head_sha, state = excluded.state,
           merge_commit_sha = excluded.merge_commit_sha, merged_at = excluded.merged_at,
           last_polled_at = excluded.last_polled_at, updated_at = excluded.updated_at",
        params![
            c.id.to_string(),
            t.item.project_id.to_string(),
            c.work_item_id.to_string(),
            c.repo_id.to_string(),
            i64_of(c.number)?,
            c.url,
            c.head_branch,
            c.base_branch,
            c.head_sha,
            c.state.as_str(),
            c.merge_commit_sha,
            c.merged_at.map(|x| x.0),
            c.last_polled_at.map(|x| x.0),
            c.created_at.0,
            c.updated_at.0,
        ],
    )?;
    Ok(())
}

fn environment(tx: &Transaction<'_>, t: &Task, e: &Environment) -> Result<()> {
    tx.execute(
        "INSERT INTO environment (id, project_id, work_item_id, repo_id, root_path, worktree_path, branch, base_sha,
           state, created_at, updated_at, port_start, port_count, recipe_json, recipe_sha256, toolchains_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT (id) DO UPDATE SET base_sha = excluded.base_sha, state = excluded.state,
           updated_at = excluded.updated_at",
        params![
            e.id.to_string(),
            t.item.project_id.to_string(),
            e.work_item_id.to_string(),
            e.repo_id.to_string(),
            e.root_path,
            e.worktree_path,
            e.branch,
            e.base_sha,
            e.state.as_str(),
            e.created_at.0,
            e.updated_at.0,
            e.port_start,
            e.port_count,
            e.recipe.as_ref().map(Value::to_string),
            e.recipe_sha256,
            serde_json::to_string(&e.toolchains)?,
        ],
    )?;
    Ok(())
}

fn session(tx: &Transaction<'_>, t: &Task, s: &Session) -> Result<()> {
    tx.execute(
        "INSERT INTO session (id, project_id, work_item_id, stage_run_id, environment_id, cli, cli_version,
           cli_session_id, model, account, state, runner_unit, socket_path, log_path, turns, followups, running_ms,
           cost_usd_micros, input_tokens, output_tokens, end_reason, started_at, ended_at, created_at, updated_at,
           resumed_from_session_id, resume_cause, idle_since, interrupt_requested)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22,
           ?23, ?24, ?25, ?26, ?27, ?28, ?29)
         ON CONFLICT (id) DO UPDATE SET cli_session_id = excluded.cli_session_id, model = excluded.model,
           state = excluded.state, turns = excluded.turns, followups = excluded.followups,
           running_ms = excluded.running_ms, cost_usd_micros = excluded.cost_usd_micros,
           input_tokens = excluded.input_tokens, output_tokens = excluded.output_tokens,
           end_reason = excluded.end_reason, ended_at = excluded.ended_at, updated_at = excluded.updated_at,
           idle_since = excluded.idle_since, interrupt_requested = excluded.interrupt_requested",
        params![
            s.id.to_string(),
            t.item.project_id.to_string(),
            s.work_item_id.to_string(),
            s.stage_run_id.to_string(),
            s.environment_id.to_string(),
            s.cli,
            s.cli_version,
            s.cli_session_id,
            s.model,
            s.account,
            s.state.as_str(),
            s.runner_unit,
            s.socket_path,
            s.log_path,
            s.turns,
            s.followups,
            s.running_ms,
            s.cost_usd_micros,
            s.input_tokens,
            s.output_tokens,
            s.end_reason.as_ref().map(to_json).transpose()?,
            s.started_at.map(|x| x.0),
            s.ended_at.map(|x| x.0),
            s.created_at.0,
            s.updated_at.0,
            s.resumed_from_session_id.map(|id| id.to_string()),
            s.resume_cause.map(|c| c.as_str()),
            s.idle_since.map(|x| x.0),
            s.interrupt_requested.map(|i| i.as_str()),
        ],
    )?;
    Ok(())
}

fn permission(tx: &Transaction<'_>, t: &Task, p: &PermissionRequest) -> Result<()> {
    tx.execute(
        "INSERT INTO permission_request (id, project_id, work_item_id, session_id, cli_request_id, tool, input, summary,
           state, answer_source, message, requested_at, answered_at, delivered_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (id) DO UPDATE SET state = excluded.state, answer_source = excluded.answer_source,
           message = excluded.message, answered_at = excluded.answered_at, delivered_at = excluded.delivered_at",
        params![
            p.id.to_string(),
            t.item.project_id.to_string(),
            p.work_item_id.to_string(),
            p.session_id.to_string(),
            p.cli_request_id,
            p.tool,
            value_json(&p.input)?,
            p.summary,
            p.state.as_str(),
            p.answer_source,
            p.message,
            p.requested_at.0,
            p.answered_at.map(|x| x.0),
            p.delivered_at.map(|x| x.0),
        ],
    )?;
    Ok(())
}
