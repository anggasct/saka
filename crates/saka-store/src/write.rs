use rusqlite::{Transaction, params};
use saka_core::ids::{ProjectId, RepoId, SpecDraftAttemptId, SpecId, WorkItemId};
use saka_core::job::Job;
use saka_core::model::{
    AccountQuota, Blocker, ChangeRequest, ContextRef, Draft, Environment, GateResult, LintRound,
    PermissionRequest, PermissionRule, Project, Repo, Review, Session, SessionActivity, Spec,
    SpecDependency, StageRun, Task, UsageRecord, WorkItemDependency,
};
use saka_core::time::Timestamp;
use saka_core::vocab::GateSource;
use serde_json::Value;
use uuid::Uuid;

use crate::codec::{budget_json, i64_of, owner_columns, to_json, value_json};
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
    pub context_hash: String,
    pub context: Vec<ContextRef>,
    pub content: Vec<u8>,
    pub lint_ok: bool,
    pub lint_issues: Value,
    pub model: Value,
    pub parser_version: u32,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBlobRow {
    pub project_id: ProjectId,
    pub content_hash: String,
    pub content: Vec<u8>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDeliveryRow {
    pub id: Uuid,
    pub integration_id: String,
    pub delivery_id: String,
    pub event: String,
    pub action: Option<String>,
    pub repo_id: Option<saka_core::ids::RepoId>,
    pub refreshed: u32,
    pub received_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRow {
    pub project_id: ProjectId,
    pub name: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeCursorRow {
    pub repo_id: RepoId,
    pub number: u64,
    pub updated_at: Timestamp,
    pub through: Timestamp,
    pub seen_ids: Vec<String>,
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
    pub context_blobs: Vec<ContextBlobRow>,
    pub tasks: Vec<Task>,
    pub drafts: Vec<Draft>,
    pub draft_rounds: Vec<LintRound>,
    pub draft_packets: Vec<(SpecDraftAttemptId, String)>,
    pub spec_dependencies: Vec<SpecDependency>,
    pub work_item_dependencies: Vec<WorkItemDependency>,
    pub replaced_spec_dependencies: Vec<SpecId>,
    pub replaced_work_item_dependencies: Vec<WorkItemId>,
    pub permission_rules: Vec<PermissionRule>,
    pub hook_runs: Vec<saka_core::hook::HookRun>,
    pub activities: Vec<(ProjectId, SessionActivity)>,
    pub session_offsets: Vec<(saka_core::ids::SessionId, u64)>,
    pub jobs: Vec<NewJob>,
    pub secrets: Vec<SecretRow>,
    pub removed_secrets: Vec<(ProjectId, String)>,
    pub webhook_deliveries: Vec<WebhookDeliveryRow>,
    pub prune_webhook_deliveries_before: Option<Timestamp>,
    pub change_cursors: Vec<ChangeCursorRow>,
    pub open_changes_seen: Vec<(RepoId, Vec<u64>)>,
    pub user_link_authorizations: Vec<crate::user_link::AuthorizationRow>,
    pub user_links: Vec<crate::user_link::UserLinkRow>,
    pub removed_user_links: Vec<saka_core::ids::UserLinkId>,
    pub project_authorships: Vec<crate::authorship::ProjectAuthorshipRow>,
    pub inbox_dismissals: Vec<crate::inbox::InboxDismissal>,
}

impl Batch {
    pub fn new(now: Timestamp) -> Batch {
        Batch {
            now,
            ..Batch::default()
        }
    }
}

pub(crate) fn write(
    tx: &Transaction<'_>,
    b: &Batch,
    inbox: &[crate::inbox::InboxSync],
) -> Result<Vec<i64>> {
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
    for s in &b.secrets {
        tx.execute(
            "INSERT INTO secret (project_id, name, nonce, ciphertext, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (project_id, name) DO UPDATE SET
               nonce = excluded.nonce, ciphertext = excluded.ciphertext, updated_at = excluded.updated_at",
            params![
                s.project_id.0.to_string(),
                s.name,
                s.nonce,
                s.ciphertext,
                s.created_at.0,
                s.updated_at.0,
            ],
        )?;
    }
    for a in &b.user_link_authorizations {
        crate::user_link::write_authorization(tx, a)?;
    }
    for id in &b.removed_user_links {
        crate::user_link::remove_link(tx, *id)?;
    }
    for l in &b.user_links {
        crate::user_link::write_link(tx, l)?;
    }
    for a in &b.project_authorships {
        crate::authorship::write_authorship(tx, a)?;
    }
    for (project, name) in &b.removed_secrets {
        tx.execute(
            "DELETE FROM secret WHERE project_id = ?1 AND name = ?2",
            params![project.0.to_string(), name],
        )?;
    }
    for p in &b.projects {
        project(tx, p)?;
    }
    for s in &b.specs {
        spec(tx, s)?;
    }
    for d in &b.webhook_deliveries {
        tx.execute(
            "INSERT INTO webhook_delivery (id, integration_id, delivery_id, event, action, repo_id, outcome,
               change_requests, received_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                d.id.to_string(),
                d.integration_id,
                d.delivery_id,
                d.event,
                d.action,
                d.repo_id.map(|r| r.0.to_string()),
                if d.refreshed > 0 { "refreshed" } else { "ignored" },
                d.refreshed,
                d.received_at.0,
            ],
        )?;
    }
    if let Some(before) = b.prune_webhook_deliveries_before {
        tx.execute(
            "DELETE FROM webhook_delivery WHERE received_at < ?1",
            [before.0],
        )?;
    }
    for c in &b.change_cursors {
        tx.execute(
            "INSERT INTO change_signal_cursor (repo_id, number, updated_at, through, seen_ids)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (repo_id, number) DO UPDATE SET updated_at = excluded.updated_at,
               through = excluded.through, seen_ids = excluded.seen_ids",
            params![
                c.repo_id.0.to_string(),
                i64_of(c.number)?,
                c.updated_at.0,
                c.through.0,
                to_json(&c.seen_ids)?,
            ],
        )?;
    }
    for (repo_id, numbers) in &b.open_changes_seen {
        tx.execute(
            "DELETE FROM change_signal_cursor WHERE repo_id = ?1
               AND number NOT IN (SELECT value FROM json_each(?2))",
            params![repo_id.0.to_string(), to_json(numbers)?],
        )?;
    }
    for blob in &b.context_blobs {
        tx.execute(
            "INSERT INTO context_blob (project_id, content_hash, content, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (project_id, content_hash) DO NOTHING",
            params![
                blob.project_id.0.to_string(),
                blob.content_hash,
                blob.content,
                blob.created_at.0,
            ],
        )?;
    }
    for r in &b.revisions {
        revision(tx, r)?;
    }
    for t in &b.tasks {
        task(tx, t)?;
    }
    for d in &b.drafts {
        draft(tx, d, &b.draft_packets)?;
    }
    for r in &b.draft_rounds {
        let project = b
            .drafts
            .iter()
            .find(|d| d.draft.id == r.spec_draft_id)
            .map(|d| d.draft.project_id)
            .ok_or_else(|| StoreError::Corrupt("lint round without its draft".into()))?;
        lint_round(tx, project, r)?;
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
    for r in &b.hook_runs {
        crate::hook_run::upsert(tx, r)?;
    }
    for (project, a) in &b.activities {
        tx.execute(
            "INSERT INTO session_activity (session_id, \"offset\", work_item_id, spec_draft_id, project_id, at, kind,
               summary, data)
             VALUES (?1, ?2, ?3, ?9, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (session_id, \"offset\") DO NOTHING",
            params![
                a.session_id.to_string(),
                i64_of(a.offset)?,
                owner_columns(&a.owner).work_item_id,
                project.to_string(),
                a.at.0,
                a.kind,
                a.summary,
                value_json(&a.data)?,
                owner_columns(&a.owner).spec_draft_id,
            ],
        )?;
    }
    for (session, offset) in &b.session_offsets {
        tx.execute(
            "UPDATE session SET last_offset = max(last_offset, ?2) WHERE id = ?1",
            params![session.to_string(), i64_of(*offset)?],
        )?;
    }
    for d in &b.inbox_dismissals {
        crate::inbox::dismiss(tx, d, b.now)?;
    }
    for s in inbox {
        crate::inbox::sync(tx, s, b.now)?;
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
    let config_error = p.config_error.as_ref().map(to_json).transpose()?;
    tx.execute(
        "INSERT INTO project (id, name, workspace_repo_id, project_path, base_branch, config_commit, config_sha256,
           config_error, workflow, hook_skip, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT (id) DO UPDATE SET name = excluded.name, base_branch = excluded.base_branch,
           config_commit = excluded.config_commit, config_sha256 = excluded.config_sha256,
           config_error = excluded.config_error, workflow = excluded.workflow, hook_skip = excluded.hook_skip, updated_at = excluded.updated_at",
        params![
            p.id.to_string(),
            p.name,
            p.workspace_repo_id.to_string(),
            p.project_path,
            p.base_branch,
            p.config_commit,
            p.config_sha256,
            config_error,
            to_json(&p.workflow)?,
            to_json(&p.hook_skip)?,
            p.created_at.0,
            p.updated_at.0,
        ],
    )?;
    let created: std::collections::HashMap<String, i64> = {
        let mut stmt =
            tx.prepare("SELECT repo_id, created_at FROM project_repo WHERE project_id = ?1")?;
        let rows = stmt.query_map([p.id.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    // Bindings are replaced as a whole: swapping a default or a name between two rows
    // would trip the unique indexes if rows were updated one at a time.
    tx.execute(
        "DELETE FROM project_repo WHERE project_id = ?1",
        params![p.id.to_string()],
    )?;
    for r in &p.repos {
        let repo_id = r.repo_id.to_string();
        let since = created.get(&repo_id).copied().unwrap_or(p.updated_at.0);
        tx.execute(
            "INSERT INTO project_repo (project_id, repo_id, name, is_default, base_branch, path_scope, autonomy,
               created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                p.id.to_string(),
                repo_id,
                r.name,
                r.is_default,
                r.base_branch,
                to_json(&r.path_scope)?,
                r.autonomy.map(saka_core::vocab::Autonomy::as_str),
                since,
                p.updated_at.0
            ],
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
        "INSERT INTO spec_revision (id, spec_id, project_id, source_commit, content_hash, context_hash, content,
           lint_ok, lint_issues, model, parser_version, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT (id) DO NOTHING",
        params![
            r.id.to_string(),
            r.spec_id.to_string(),
            r.project_id.to_string(),
            r.source_commit,
            r.content_hash,
            r.context_hash,
            r.content,
            r.lint_ok,
            value_json(&r.lint_issues)?,
            value_json(&r.model)?,
            r.parser_version,
            r.created_at.0,
        ],
    )?;
    for (ordinal, doc) in (1u32..).zip(&r.context) {
        tx.execute(
            "INSERT INTO spec_revision_context (revision_id, ordinal, project_id, path, origin, line,
               content_hash, bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (revision_id, ordinal) DO NOTHING",
            params![
                r.id.to_string(),
                ordinal,
                r.project_id.to_string(),
                doc.path,
                doc.origin.as_str(),
                doc.line,
                doc.content_hash,
                i64_of(doc.bytes)?,
            ],
        )?;
    }
    Ok(())
}

fn task(tx: &Transaction<'_>, t: &Task) -> Result<()> {
    let i = &t.item;
    tx.execute(
        "INSERT INTO work_item (id, project_id, origin, spec_id, spec_revision_id, step_id, step_index, task_key, work,
           repo_id, branch, state, stage, stage_phase, priority, version, admitted_at, done_at, start_approved_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
         ON CONFLICT (id) DO UPDATE SET spec_revision_id = excluded.spec_revision_id,
           step_index = excluded.step_index, work = excluded.work, repo_id = excluded.repo_id, state = excluded.state, stage = excluded.stage,
           stage_phase = excluded.stage_phase, priority = excluded.priority, version = excluded.version,
           admitted_at = excluded.admitted_at, done_at = excluded.done_at,
           start_approved_at = excluded.start_approved_at, updated_at = excluded.updated_at",
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
            i.start_approved_at.map(|t| t.0),
            i.created_at.0,
            i.updated_at.0,
        ],
    )?;
    if let Some(x) = &t.external {
        tx.execute(
            "INSERT INTO external_change (work_item_id, project_id, title, author, from_fork, draft, spec_id,
               step_id, cause, actor, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (work_item_id) DO NOTHING",
            params![
                i.id.to_string(),
                i.project_id.to_string(),
                x.title,
                x.author,
                x.from_fork,
                x.draft,
                x.spec.as_ref().map(|s| s.spec_id.to_string()),
                x.spec.as_ref().and_then(|s| s.step_id.clone()),
                x.cause.as_str(),
                x.actor,
                x.created_at.0,
            ],
        )?;
    }
    for run in t.retired_runs.iter().chain(t.stage_run.iter()) {
        stage_run(tx, t, run)?;
    }
    for g in &t.gates {
        gate(tx, t, g)?;
    }
    if let Some(b) = &t.blocker {
        blocker(tx, t, b)?;
    }
    for r in &t.reviews {
        review(tx, t, r)?;
    }
    if let Some(cr) = &t.docs_change {
        change_request(tx, t, cr)?;
    }
    if let Some(cr) = &t.change_request {
        change_request(tx, t, cr)?;
        if let Some(g) = t
            .gates
            .iter()
            .find(|g| g.source == GateSource::Scm && g.head_sha == cr.head_sha)
        {
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
    run(
        tx,
        i.project_id,
        RunRows {
            environment: t.environment.as_ref(),
            sessions: &t.retired_sessions,
            session: t.session.as_ref(),
            permissions: &t.permissions,
            usage_records: &t.usage_records,
            quotas: &t.quota_observations,
        },
    )?;
    for r in &t.added_rules {
        permission_rule(tx, r)?;
    }
    Ok(())
}

struct RunRows<'a> {
    environment: Option<&'a Environment>,
    sessions: &'a [Session],
    session: Option<&'a Session>,
    permissions: &'a [PermissionRequest],
    usage_records: &'a [UsageRecord],
    quotas: &'a [AccountQuota],
}

fn run(tx: &Transaction<'_>, project: ProjectId, rows: RunRows<'_>) -> Result<()> {
    if let Some(e) = rows.environment {
        environment(tx, project, e)?;
    }
    for s in rows.sessions.iter().chain(rows.session) {
        session(tx, project, s)?;
    }
    for p in rows.permissions {
        permission(tx, project, p)?;
    }
    for u in rows.usage_records {
        usage_record(tx, project, u)?;
    }
    for q in rows.quotas {
        account_quota(tx, q)?;
    }
    Ok(())
}

fn draft(tx: &Transaction<'_>, d: &Draft, packets: &[(SpecDraftAttemptId, String)]) -> Result<()> {
    let x = &d.draft;
    tx.execute(
        "INSERT INTO spec_draft (id, project_id, path, slug, idea, locale, context_paths, state, attempt,
           current_attempt_id, retries, queued_at, failure, spec_id, commit_sha, version, created_by, created_at,
           updated_at, finished_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
         ON CONFLICT (id) DO UPDATE SET state = excluded.state, attempt = excluded.attempt,
           current_attempt_id = excluded.current_attempt_id, retries = excluded.retries,
           queued_at = excluded.queued_at, failure = excluded.failure, spec_id = excluded.spec_id,
           commit_sha = excluded.commit_sha, version = excluded.version, updated_at = excluded.updated_at,
           finished_at = excluded.finished_at",
        params![
            x.id.to_string(),
            x.project_id.to_string(),
            x.path,
            x.slug,
            x.idea,
            x.locale.as_str(),
            to_json(&x.context_paths)?,
            x.state.as_str(),
            x.attempt,
            x.current_attempt_id.map(|id| id.to_string()),
            x.retries,
            x.queued_at.0,
            x.failure.as_ref().map(to_json).transpose()?,
            x.spec_id.map(|id| id.to_string()),
            x.commit_sha,
            i64_of(x.version)?,
            x.created_by,
            x.created_at.0,
            x.updated_at.0,
            x.finished_at.map(|t| t.0),
        ],
    )?;
    if let Some(a) = &d.attempt {
        let packet = packets
            .iter()
            .find(|(id, _)| *id == a.id)
            .map(|(_, text)| text.as_str());
        tx.execute(
            "INSERT INTO spec_draft_attempt (id, spec_draft_id, project_id, attempt, trigger, reason, note, state,
               base_sha, budget, budget_warnings, resumed_lost, rounds, packet, packet_sha256, failure, started_at,
               ended_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT (id) DO UPDATE SET state = excluded.state, base_sha = excluded.base_sha,
               budget_warnings = excluded.budget_warnings, resumed_lost = excluded.resumed_lost,
               rounds = excluded.rounds, packet = coalesce(spec_draft_attempt.packet, excluded.packet),
               packet_sha256 = coalesce(spec_draft_attempt.packet_sha256, excluded.packet_sha256),
               failure = excluded.failure, ended_at = excluded.ended_at",
            params![
                a.id.to_string(),
                x.id.to_string(),
                x.project_id.to_string(),
                a.attempt,
                a.trigger.as_str(),
                a.reason,
                a.note,
                a.state.as_str(),
                a.base_sha,
                to_json(&a.budget)?,
                to_json(&a.budget_warnings)?,
                a.resumed_lost,
                a.rounds,
                packet,
                packet.and(a.packet_sha256.as_deref()),
                a.failure.as_ref().map(to_json).transpose()?,
                a.started_at.0,
                a.ended_at.map(|t| t.0),
            ],
        )?;
    }
    for e in &d.retired_environments {
        environment(tx, x.project_id, e)?;
    }
    run(
        tx,
        x.project_id,
        RunRows {
            environment: d.environment.as_ref(),
            sessions: &d.retired_sessions,
            session: d.session.as_ref(),
            permissions: &d.permissions,
            usage_records: &d.usage_records,
            quotas: &d.quota_observations,
        },
    )
}

fn lint_round(tx: &Transaction<'_>, project: ProjectId, r: &LintRound) -> Result<()> {
    tx.execute(
        "INSERT INTO spec_draft_lint_round (attempt_id, round, spec_draft_id, project_id, session_id, turn, outcome,
           bytes, content, content_hash, parser_version, issues, decisions_open, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT DO NOTHING",
        params![
            r.attempt_id.to_string(),
            r.round,
            r.spec_draft_id.to_string(),
            project.to_string(),
            r.session_id.to_string(),
            r.turn,
            r.outcome.as_str(),
            r.bytes.map(i64_of).transpose()?,
            r.content,
            r.content_hash,
            r.parser_version,
            to_json(&r.issues)?,
            r.decisions_open,
            r.created_at.0,
        ],
    )?;
    Ok(())
}

fn usage_record(tx: &Transaction<'_>, project: ProjectId, u: &UsageRecord) -> Result<()> {
    let owner = owner_columns(&u.owner);
    tx.execute(
        "INSERT INTO usage_record (id, project_id, work_item_id, stage_run_id, spec_draft_id, spec_draft_attempt_id,
           session_id, account, turn, status, input_tokens, output_tokens, cost_usd_micros, running_ms, ended_at)
         VALUES (?1, ?2, ?3, ?4, ?14, ?15, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT (session_id, turn) DO NOTHING",
        params![
            u.id.to_string(),
            project.to_string(),
            owner.work_item_id,
            owner.stage_run_id,
            u.session_id.to_string(),
            u.account,
            u.turn,
            u.status,
            u.input_tokens,
            u.output_tokens,
            u.cost_usd_micros,
            u.running_ms,
            u.ended_at.0,
            owner.spec_draft_id,
            owner.spec_draft_attempt_id,
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
        "INSERT INTO permission_rule (id, project_id, effect, tool, match_kind, command, path, note,
           permission_request_id, created_by, created_at, revoked_at, revoke_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT (id) DO UPDATE SET revoked_at = excluded.revoked_at, revoke_reason = excluded.revoke_reason",
        params![
            r.id.to_string(),
            r.project_id.to_string(),
            r.effect.as_str(),
            r.tool,
            r.match_kind.as_str(),
            r.command,
            r.path,
            r.note,
            r.permission_request_id.map(|p| p.to_string()),
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
           head_sha, failure, started_at, ended_at, created_at, updated_at, budget, budget_warnings,
           docs_head_sha, queued_at, queue_native, fix)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
         ON CONFLICT (id) DO UPDATE SET state = excluded.state, head_sha = excluded.head_sha,
           docs_head_sha = excluded.docs_head_sha, queued_at = excluded.queued_at,
           queue_native = excluded.queue_native,
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
            r.docs_head_sha,
            r.queued_at.map(|x| x.0),
            i64::from(r.queue_native),
            r.fix.as_ref().map(to_json).transpose()?,
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

fn review(tx: &Transaction<'_>, t: &Task, r: &Review) -> Result<()> {
    tx.execute(
        "INSERT INTO review (id, work_item_id, stage_run_id, session_id, head_sha, outcome, basis, summary,
           findings, questions, posted_event, posted_url, posted_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (id) DO UPDATE SET posted_event = excluded.posted_event,
           posted_url = excluded.posted_url, posted_at = excluded.posted_at",
        params![
            r.id.to_string(),
            t.item.id.to_string(),
            r.stage_run_id.to_string(),
            r.session_id.to_string(),
            r.head_sha,
            r.outcome.as_str(),
            r.basis.as_str(),
            r.summary,
            to_json(&r.findings)?,
            to_json(&r.questions)?,
            r.posted.as_ref().map(|p| p.event.as_str()),
            r.posted.as_ref().and_then(|p| p.url.clone()),
            r.posted.as_ref().map(|p| p.at.0),
            r.created_at.0,
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
           head_sha, state, merge_commit_sha, merged_at, last_polled_at, created_at, updated_at, role, skipped_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT (id) DO UPDATE SET head_sha = excluded.head_sha, state = excluded.state,
           merge_commit_sha = excluded.merge_commit_sha, merged_at = excluded.merged_at,
           skipped_at = excluded.skipped_at,
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
            c.role.as_str(),
            c.skipped_at.map(|x| x.0),
        ],
    )?;
    Ok(())
}

fn environment(tx: &Transaction<'_>, project: ProjectId, e: &Environment) -> Result<()> {
    let owner = owner_columns(&e.owner);
    tx.execute(
        "INSERT INTO environment (id, project_id, work_item_id, spec_draft_id, spec_draft_attempt_id, repo_id,
           root_path, worktree_path, branch, base_sha, state, created_at, updated_at, port_start, port_count,
           recipe_json, recipe_sha256, toolchains_json, path_scope_json, docs_json)
         VALUES (?1, ?2, ?3, ?17, ?18, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?19,
           ?20)
         ON CONFLICT (id) DO UPDATE SET base_sha = excluded.base_sha, state = excluded.state,
           updated_at = excluded.updated_at, docs_json = excluded.docs_json",
        params![
            e.id.to_string(),
            project.to_string(),
            owner.work_item_id,
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
            owner.spec_draft_id,
            owner.spec_draft_attempt_id,
            serde_json::to_string(&e.path_scope)?,
            e.docs.as_ref().map(serde_json::to_string).transpose()?,
        ],
    )?;
    Ok(())
}

fn session(tx: &Transaction<'_>, project: ProjectId, s: &Session) -> Result<()> {
    let owner = owner_columns(&s.owner);
    tx.execute(
        "INSERT INTO session (id, project_id, work_item_id, stage_run_id, spec_draft_id, spec_draft_attempt_id,
           environment_id, cli, cli_version, cli_session_id, model, account, state, runner_unit, socket_path,
           log_path, turns, followups, running_ms, cost_usd_micros, input_tokens, output_tokens, end_reason,
           started_at, ended_at, created_at, updated_at, resumed_from_session_id, resume_cause, idle_since,
           interrupt_requested)
         VALUES (?1, ?2, ?3, ?4, ?30, ?31, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
           ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29)
         ON CONFLICT (id) DO UPDATE SET cli_session_id = excluded.cli_session_id, model = excluded.model,
           state = excluded.state, turns = excluded.turns, followups = excluded.followups,
           running_ms = excluded.running_ms, cost_usd_micros = excluded.cost_usd_micros,
           input_tokens = excluded.input_tokens, output_tokens = excluded.output_tokens,
           end_reason = excluded.end_reason, ended_at = excluded.ended_at, updated_at = excluded.updated_at,
           idle_since = excluded.idle_since, interrupt_requested = excluded.interrupt_requested",
        params![
            s.id.to_string(),
            project.to_string(),
            owner.work_item_id,
            owner.stage_run_id,
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
            owner.spec_draft_id,
            owner.spec_draft_attempt_id,
        ],
    )?;
    Ok(())
}

fn permission(tx: &Transaction<'_>, project: ProjectId, p: &PermissionRequest) -> Result<()> {
    let owner = owner_columns(&p.owner);
    tx.execute(
        "INSERT INTO permission_request (id, project_id, work_item_id, spec_draft_id, session_id, cli_request_id,
           tool, input, summary, state, answer_source, message, requested_at, answered_at, delivered_at)
         VALUES (?1, ?2, ?3, ?15, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (id) DO UPDATE SET state = excluded.state, answer_source = excluded.answer_source,
           message = excluded.message, answered_at = excluded.answered_at, delivered_at = excluded.delivered_at",
        params![
            p.id.to_string(),
            project.to_string(),
            owner.work_item_id,
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
            owner.spec_draft_id,
        ],
    )?;
    Ok(())
}
