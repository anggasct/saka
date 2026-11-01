use saka_core::admission::Waiting;
use saka_core::gate::Check;
use saka_core::model::{StageRun, Task};
use saka_core::task::available_actions;
use saka_core::time::Timestamp;
use saka_engine::{PermissionView, ProjectView, SessionView, SpecView, TaskDetail, TaskView};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

pub fn time(t: Timestamp) -> String {
    jiff::Timestamp::from_millisecond(t.0)
        .map(|ts| ts.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

fn opt_time(t: Option<Timestamp>) -> Option<String> {
    t.map(time)
}

/// A list page.
#[derive(Debug, Serialize, ToSchema)]
pub struct Page<T: ToSchema> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Workspace {
    pub repo_id: Uuid,
    pub full_name: String,
    pub project_path: String,
    pub base_branch: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectRepo {
    pub repo_id: Uuid,
    pub name: String,
    pub integration_id: String,
    pub provider: String,
    pub full_name: String,
    pub url: String,
    pub default_branch: String,
    pub is_default: bool,
    pub path_scope: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub workspace: Workspace,
    pub repos: Vec<ProjectRepo>,
    /// `standard` in this version.
    pub autonomy: String,
    /// `full_loop` in this version.
    pub workflow_preset: String,
    pub config_commit: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Project {
    pub fn from_view(v: &ProjectView) -> Project {
        let p = &v.project;
        let repo = |id| v.repos.iter().find(|r| r.id == id);
        let workspace = repo(p.workspace_repo_id);
        Project {
            id: p.id.0,
            name: p.name.clone(),
            workspace: Workspace {
                repo_id: p.workspace_repo_id.0,
                full_name: workspace.map(|r| r.full_name.clone()).unwrap_or_default(),
                project_path: p.project_path.clone(),
                base_branch: p.base_branch.clone(),
            },
            repos: p
                .repos
                .iter()
                .filter_map(|pr| {
                    repo(pr.repo_id).map(|r| ProjectRepo {
                        repo_id: r.id.0,
                        name: pr.name.clone(),
                        integration_id: r.integration_id.clone(),
                        provider: r.provider.clone(),
                        full_name: r.full_name.clone(),
                        url: r.url.clone(),
                        default_branch: r.default_branch.clone(),
                        is_default: pr.is_default,
                        path_scope: pr.path_scope.clone(),
                    })
                })
                .collect(),
            autonomy: p.autonomy.as_str().to_string(),
            workflow_preset: p.workflow_preset.as_str().to_string(),
            config_commit: p.config_commit.clone(),
            created_at: time(p.created_at),
            updated_at: time(p.updated_at),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Revision {
    pub id: Uuid,
    pub source_commit: String,
    pub content_hash: String,
    /// Version of the parser that read this revision.
    pub parser_version: u32,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LintIssue {
    pub code: String,
    /// `error` blocks approve; `warning` does not.
    #[serde(default = "error_severity")]
    pub severity: String,
    /// 1-based line in the spec file.
    pub line: u32,
    pub params: Value,
}

fn error_severity() -> String {
    "error".to_string()
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Lint {
    /// No issue with severity `error`.
    pub ok: bool,
    pub issues: Vec<LintIssue>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct AcceptanceCriterion {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Step {
    pub id: String,
    pub index: u32,
    pub work: String,
    pub ac: Vec<String>,
    pub commands: Vec<String>,
    /// Step ids this step waits for.
    pub after: Vec<String>,
    pub work_item_id: Option<Uuid>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlanStep {
    pub step_id: String,
    /// `keep | repin | restore | add | supersede | conflict`
    pub action: String,
    pub work_item_id: Option<Uuid>,
    /// `changed | removed`, only for `conflict`.
    pub reason: Option<String>,
    /// Changed fields in the order `work`, `ac`, `commands`, `after`; only for `changed`.
    pub fields: Vec<String>,
}

/// What approving the latest revision again would do to each step.
#[derive(Debug, Serialize, ToSchema)]
pub struct RevisionPlan {
    pub from_revision_id: Uuid,
    pub to_revision_id: Uuid,
    /// No conflict, clean lint, and the current parser.
    pub ready: bool,
    pub steps: Vec<PlanStep>,
}

impl RevisionPlan {
    fn from_plan(p: &saka_core::plan::RevisionPlan) -> RevisionPlan {
        RevisionPlan {
            from_revision_id: p.from_revision_id.0,
            to_revision_id: p.to_revision_id.0,
            ready: p.ready,
            steps: p
                .steps
                .iter()
                .map(|st| PlanStep {
                    step_id: st.step_id.clone(),
                    action: st.action.as_str().to_string(),
                    work_item_id: st.work_item_id.map(|id| id.0),
                    reason: st.reason.map(|r| r.as_str().to_string()),
                    fields: st.fields.iter().map(|f| f.as_str().to_string()).collect(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SpecDependency {
    pub slug: String,
    /// Null when no spec in the project has this slug.
    pub spec_id: Option<Uuid>,
    pub state: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Spec {
    pub id: Uuid,
    pub project_id: Uuid,
    pub path: String,
    pub slug: String,
    pub title: String,
    /// `draft | ready | approved | shipped | withdrawn`
    pub state: String,
    pub origin: String,
    pub priority: i64,
    /// Specs that must ship before this one starts, resolved in the project.
    pub depends_on: Vec<SpecDependency>,
    pub revision: Revision,
    pub approved_revision_id: Option<Uuid>,
    /// Null unless the spec is approved and a newer revision is recorded.
    pub revision_plan: Option<RevisionPlan>,
    pub lint: Lint,
    /// Left out of list items.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<Vec<AcceptanceCriterion>>,
    pub steps: Vec<Step>,
    pub approved_at: Option<String>,
    pub shipped_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Deserialize)]
struct ModelStep {
    id: String,
    index: u32,
    work: String,
    #[serde(default)]
    ac: Vec<String>,
    #[serde(default)]
    commands: Vec<String>,
    #[serde(default)]
    after: Vec<String>,
}

impl Spec {
    pub fn from_view(v: &SpecView, with_criteria: bool) -> Spec {
        let s = &v.spec;
        let r = &v.revision;
        let mut steps: Vec<ModelStep> = r
            .model
            .get("steps")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        // Version 1 models predate the After column: their steps always ran in order.
        if r.parser_version < 2 {
            for step in steps.iter_mut().filter(|s| s.index > 1) {
                step.after = vec![format!("S{}", step.index - 1)];
            }
        }
        let criteria: Vec<AcceptanceCriterion> = r
            .model
            .get("acceptance_criteria")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        Spec {
            id: s.id.0,
            project_id: s.project_id.0,
            path: s.path.clone(),
            slug: s.slug.clone(),
            title: s.title.clone(),
            state: s.state.as_str().to_string(),
            origin: s.origin.as_str().to_string(),
            priority: s.priority,
            depends_on: v
                .depends_on
                .iter()
                .map(|d| SpecDependency {
                    slug: d.slug.clone(),
                    spec_id: d.spec_id.map(|id| id.0),
                    state: d.state.map(|st| st.as_str().to_string()),
                })
                .collect(),
            revision: Revision {
                id: r.id.0,
                source_commit: r.source_commit.clone(),
                content_hash: r.content_hash.clone(),
                parser_version: r.parser_version,
                created_at: time(r.created_at),
            },
            approved_revision_id: s.approved_revision_id.map(|id| id.0),
            revision_plan: v.revision_plan.as_ref().map(RevisionPlan::from_plan),
            lint: Lint {
                ok: r.lint_ok,
                issues: r
                    .issues
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|i| serde_json::from_value(i.clone()).ok())
                    .collect(),
            },
            acceptance_criteria: with_criteria.then_some(criteria),
            steps: steps
                .into_iter()
                .map(|st| Step {
                    work_item_id: v
                        .work_items
                        .iter()
                        .find(|(id, _)| *id == st.id)
                        .map(|(_, w)| w.0),
                    id: st.id,
                    index: st.index,
                    work: st.work,
                    ac: st.ac,
                    commands: st.commands,
                    after: st.after,
                })
                .collect(),
            approved_at: opt_time(s.approved_at),
            shipped_at: opt_time(s.shipped_at),
            created_at: time(s.created_at),
            updated_at: time(s.updated_at),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ChangeRequest {
    pub id: Uuid,
    pub number: u64,
    pub url: String,
    pub state: String,
    pub head_sha: String,
    pub base_branch: String,
    pub merge_commit_sha: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GateCheck {
    pub name: String,
    pub kind: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub url: Option<String>,
}

impl From<&Check> for GateCheck {
    fn from(c: &Check) -> GateCheck {
        GateCheck {
            name: c.name.clone(),
            kind: c.kind.as_str().to_string(),
            status: c.status.as_str().to_string(),
            conclusion: c.conclusion.map(|x| x.as_str().to_string()),
            url: c.url.clone(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Gate {
    pub source: String,
    pub head_sha: String,
    /// `pending | passed | failed`
    pub outcome: String,
    pub checks: Vec<GateCheck>,
    pub observed_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Blocker {
    pub id: Uuid,
    pub code: String,
    pub params: Value,
    pub opened_at: String,
    pub stage: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionBrief {
    pub id: Uuid,
    pub state: String,
    pub cli: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WorkItem {
    pub id: Uuid,
    pub project_id: Uuid,
    pub origin: String,
    pub spec_id: Option<Uuid>,
    pub spec_revision_id: Option<Uuid>,
    pub step_id: Option<String>,
    pub step_index: Option<u32>,
    pub task_key: String,
    pub work: String,
    pub repo_id: Uuid,
    pub branch: String,
    /// `queued | awaiting_start | admitted | in_stage | blocked | taken_over | done | cancelled | superseded`
    pub state: String,
    pub stage: Option<String>,
    pub stage_phase: Option<String>,
    pub priority: i64,
    /// Steps and specs this task still waits for; empty unless the task is queued.
    pub waiting_on: Vec<WaitingOn>,
    pub change_request: Option<ChangeRequest>,
    pub gate: Option<Gate>,
    pub blocker: Option<Blocker>,
    pub session: Option<SessionBrief>,
    pub pending_permission_count: u32,
    /// Subset of `merge`, `retry`, `cancel`.
    pub available_actions: Vec<String>,
    pub version: u64,
    pub created_at: String,
    pub updated_at: String,
    pub admitted_at: Option<String>,
    pub done_at: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitingOn {
    /// A step of the same spec that is not done yet.
    Step { step_id: String, work_item_id: Uuid },
    /// A spec that has not shipped yet.
    Spec { slug: String, spec_id: Uuid },
    /// An agent account window at or above `agent.rate_limit_pause` that has not reset yet.
    Quota {
        account: String,
        window: String,
        utilization: f64,
        resets_at: Option<String>,
    },
}

impl From<&Waiting> for WaitingOn {
    fn from(w: &Waiting) -> WaitingOn {
        match w {
            Waiting::Step {
                step_id,
                work_item_id,
            } => WaitingOn::Step {
                step_id: step_id.clone(),
                work_item_id: work_item_id.0,
            },
            Waiting::Spec { slug, spec_id } => WaitingOn::Spec {
                slug: slug.clone(),
                spec_id: spec_id.0,
            },
            Waiting::Quota {
                account,
                window,
                utilization,
                resets_at,
            } => WaitingOn::Quota {
                account: account.clone(),
                window: window.clone(),
                utilization: *utilization,
                resets_at: resets_at.map(time),
            },
        }
    }
}

impl WorkItem {
    pub fn from_view(v: &TaskView) -> WorkItem {
        WorkItem::from_task(&v.task, &v.waiting_on)
    }

    pub fn from_task(t: &Task, waiting_on: &[Waiting]) -> WorkItem {
        let i = &t.item;
        WorkItem {
            id: i.id.0,
            project_id: i.project_id.0,
            origin: i.origin.as_str().to_string(),
            spec_id: i.spec_id.map(|x| x.0),
            spec_revision_id: i.spec_revision_id.map(|x| x.0),
            step_id: i.step_id.clone(),
            step_index: i.step_index,
            task_key: i.task_key.clone(),
            work: i.work.clone(),
            repo_id: i.repo_id.0,
            branch: i.branch.clone(),
            state: i.state.as_str().to_string(),
            stage: i.stage.map(|x| x.as_str().to_string()),
            stage_phase: i.stage_phase.map(|x| x.as_str().to_string()),
            priority: i.priority,
            waiting_on: waiting_on.iter().map(WaitingOn::from).collect(),
            change_request: t.change_request.as_ref().map(|c| ChangeRequest {
                id: c.id.0,
                number: c.number,
                url: c.url.clone(),
                state: c.state.as_str().to_string(),
                head_sha: c.head_sha.clone(),
                base_branch: c.base_branch.clone(),
                merge_commit_sha: c.merge_commit_sha.clone(),
            }),
            gate: t.gate.as_ref().map(|g| Gate {
                source: g.source.as_str().to_string(),
                head_sha: g.head_sha.clone(),
                outcome: g.outcome.as_str().to_string(),
                checks: g.checks.iter().map(GateCheck::from).collect(),
                observed_at: time(g.observed_at),
            }),
            blocker: t.open_blocker().map(|b| Blocker {
                id: b.id.0,
                code: b.code.as_str().to_string(),
                params: b.params.clone(),
                opened_at: time(b.opened_at),
                stage: t.stage_run.as_ref().map(|r| r.stage.as_str().to_string()),
            }),
            session: t.session.as_ref().map(|s| SessionBrief {
                id: s.id.0,
                state: s.state.as_str().to_string(),
                cli: s.cli.clone(),
                started_at: opt_time(s.started_at),
                ended_at: opt_time(s.ended_at),
            }),
            pending_permission_count: u32::try_from(
                t.permissions
                    .iter()
                    .filter(|p| p.state == saka_core::vocab::PermissionState::Pending)
                    .count(),
            )
            .unwrap_or(u32::MAX),
            available_actions: available_actions(t).into_iter().map(String::from).collect(),
            version: i.version,
            created_at: time(i.created_at),
            updated_at: time(i.updated_at),
            admitted_at: opt_time(i.admitted_at),
            done_at: opt_time(i.done_at),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Failure {
    pub code: String,
    pub params: Value,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StageRunItem {
    pub id: Uuid,
    pub stage: String,
    pub attempt: u32,
    pub state: String,
    pub trigger: String,
    pub reason: Option<String>,
    pub head_sha: Option<String>,
    pub failure: Option<Failure>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

impl From<&StageRun> for StageRunItem {
    fn from(r: &StageRun) -> StageRunItem {
        StageRunItem {
            id: r.id.0,
            stage: r.stage.as_str().to_string(),
            attempt: r.attempt,
            state: r.state.as_str().to_string(),
            trigger: r.trigger.as_str().to_string(),
            reason: r.reason.clone(),
            head_sha: r.head_sha.clone(),
            failure: r.failure.as_ref().map(|f| Failure {
                code: f.code.clone(),
                params: f.params.clone(),
            }),
            started_at: opt_time(r.started_at),
            ended_at: opt_time(r.ended_at),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WorkItemDetail {
    #[serde(flatten)]
    pub item: WorkItem,
    pub stage_runs: Vec<StageRunItem>,
}

impl WorkItemDetail {
    pub fn from_detail(d: &TaskDetail) -> WorkItemDetail {
        WorkItemDetail {
            item: WorkItem::from_task(&d.task, &d.waiting_on),
            stage_runs: d.stage_runs.iter().map(StageRunItem::from).collect(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PermissionRequest {
    pub id: Uuid,
    pub project_id: Uuid,
    pub work_item_id: Uuid,
    pub session_id: Uuid,
    pub tool: String,
    pub summary: String,
    /// Redacted.
    pub input: Value,
    /// `pending | allowed | denied | expired`
    pub state: String,
    pub answer_source: Option<String>,
    pub message: Option<String>,
    pub requested_at: String,
    pub answered_at: Option<String>,
}

impl PermissionRequest {
    pub fn from_view(v: &PermissionView) -> PermissionRequest {
        let p = &v.request;
        PermissionRequest {
            id: p.id.0,
            project_id: v.project_id.0,
            work_item_id: p.work_item_id.0,
            session_id: p.session_id.0,
            tool: p.tool.clone(),
            summary: p.summary.clone(),
            input: p.input.clone(),
            state: p.state.as_str().to_string(),
            answer_source: p.answer_source.clone(),
            message: p.message.clone(),
            requested_at: time(p.requested_at),
            answered_at: opt_time(p.answered_at),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Session {
    pub id: Uuid,
    pub project_id: Uuid,
    pub work_item_id: Uuid,
    pub stage_run_id: Uuid,
    pub cli: String,
    pub cli_version: Option<String>,
    pub model: Option<String>,
    pub state: String,
    pub turns: u32,
    pub running_ms: i64,
    /// Decimal string, from micro-dollars.
    pub cost_usd: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

pub fn usd(micros: i64) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let m = micros.unsigned_abs();
    format!("{sign}{}.{:06}", m / 1_000_000, m % 1_000_000)
}

impl Session {
    pub fn from_view(v: &SessionView) -> Session {
        let s = &v.session;
        Session {
            id: s.id.0,
            project_id: v.project_id.0,
            work_item_id: s.work_item_id.0,
            stage_run_id: s.stage_run_id.0,
            cli: s.cli.clone(),
            cli_version: s.cli_version.clone(),
            model: s.model.clone(),
            state: s.state.as_str().to_string(),
            turns: s.turns,
            running_ms: s.running_ms,
            cost_usd: s.cost_usd_micros.map(usd),
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            started_at: opt_time(s.started_at),
            ended_at: opt_time(s.ended_at),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Meta {
    pub server_version: String,
    pub api_version: String,
    pub min_client_api_version: String,
    pub capabilities: Vec<String>,
}

pub const CAPABILITIES: &[&str] = &[
    "projects",
    "spec_approve",
    "work_item_merge",
    "work_item_retry",
    "work_item_cancel",
    "permission_answer",
    "events_sse",
];

#[derive(Debug, Serialize, ToSchema)]
pub struct Health {
    pub status: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReadyChecks {
    pub db: String,
    pub migrations: String,
    pub worker: String,
    pub reconciler: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Ready {
    /// `ready | not_ready`
    pub status: String,
    pub checks: ReadyChecks,
}

/// RFC 9457 problem details.
#[derive(Debug, Serialize, ToSchema)]
pub struct Problem {
    /// `urn:saka:problem:<code>`
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub status: u16,
    pub code: String,
    pub params: Value,
    pub correlation_id: Option<Uuid>,
}

/// One event of `GET /api/v1/events` (the `data` of an SSE message).
#[derive(Debug, Serialize, ToSchema)]
pub struct StreamEvent {
    pub seq: i64,
    pub kind: String,
    pub version: u32,
    pub stream_type: String,
    pub stream_id: Uuid,
    pub project_id: Option<Uuid>,
    pub occurred_at: String,
    pub payload: Value,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddProjectBody {
    pub workspace_url: String,
    pub project_path: String,
    #[serde(default)]
    pub base_branch: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddSpecBody {
    pub path: String,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OptionalReason {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequiredReason {
    pub reason: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerBody {
    /// `allow | deny`
    pub decision: Decision,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_costs_render_as_documented() {
        assert_eq!(
            time(Timestamp(1_767_225_600_123)),
            "2026-01-01T00:00:00.123Z"
        );
        assert_eq!(usd(12_300), "0.012300");
        assert_eq!(usd(2_500_000), "2.500000");
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TemplateQuery {
    /// `en` (default) or `id`.
    pub locale: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SpecTemplate {
    pub locale: String,
    pub parser_version: u32,
    /// Markdown that parses without issues as `example-spec.md`.
    pub content: String,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListQuery {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SpecListQuery {
    /// One or more of `draft, ready, approved, shipped, withdrawn`, comma-separated.
    pub state: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WorkItemQuery {
    pub project_id: Option<String>,
    pub spec_id: Option<String>,
    /// One or more states, comma-separated.
    pub state: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PermissionQuery {
    /// One or more of `pending, allowed, denied, expired`, comma-separated.
    pub state: Option<String>,
    pub project_id: Option<String>,
    pub work_item_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventsQuery {
    /// Start after this `seq` (`Last-Event-ID` wins when both are given).
    pub after: Option<String>,
}
