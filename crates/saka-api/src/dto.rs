use saka_core::admission::Waiting;
use saka_core::blocker::primary_action;
use saka_core::gate::Check;
use saka_core::model::{
    Environment as CoreEnvironment, LintRound, PermissionRule as CoreRule, RunOwner as CoreOwner,
    SessionActivity as CoreActivity, SpecDraftAttempt as CoreAttempt, StageRun, Task,
};
use saka_core::session::session_actions;
use saka_core::task::available_actions;
use saka_core::time::Timestamp;
use saka_core::vocab::{Effect as CoreEffect, EnvironmentState, MatchKind as CoreMatch};
use saka_engine::{
    AccountView, BudgetView, DocContentView, DocTreeView, DocWriteView, DraftBudgetView,
    DraftContentView, DraftPacketView, DraftView, PermissionView, ProjectView, ReloadView,
    SessionView, SpecView, TaskDetail, TaskView, UsageView,
};
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
    /// Branch this binding's worktrees, recipes, change requests, and merge checks use.
    pub base_branch: String,
    pub is_default: bool,
    /// The repo is also the project's workspace repo.
    pub is_workspace: bool,
    pub path_scope: Vec<String>,
    /// `path_scope`, plus `<project_path>/**` for the workspace repo unless `project_path` is `.`.
    pub effective_path_scope: Vec<String>,
    pub autonomy: RepoAutonomy,
}

/// How much the loop may do on its own in this repo: the lowest of what the operator allows and what
/// `saka.toml` asks for.
#[derive(Debug, Serialize, ToSchema)]
pub struct RepoAutonomy {
    /// `strict | standard | autonomous`, or `null` when `saka.toml` asks for nothing.
    pub requested: Option<String>,
    /// The operator's ceiling for this repo, from `[autonomy]` in `config.toml`.
    pub allowed: String,
    /// The level in force.
    pub effective: String,
}

/// The last rejected reload of `saka.toml`; the previous config stays in force while it is set.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectConfigError {
    pub commit: String,
    /// `project_config_invalid | scm_integration_unknown | scm_repo_inaccessible | project_repo_in_use`
    pub code: String,
    pub params: Value,
    pub observed_at: String,
}

/// A warning computed when the project is read; never stored.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectWarning {
    /// `path_scope_overlap`
    pub code: String,
    /// For `path_scope_overlap`: `repo`, `repo_id`, `other_project_id`, `other_project`, `other_repo`.
    pub params: Value,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Workflow {
    /// `full_loop | review_only | build_only | custom`
    pub preset: String,
    /// Whether an approved spec creates tasks.
    pub spec: bool,
    /// The entry point for pull requests opened outside Saka; `null` when the workflow has none.
    pub change_request: Option<WorkflowEntry>,
    /// The stages in pipeline order, each with its trigger.
    pub stages: Vec<WorkflowStage>,
    /// How many fix rounds a task gets at most (`1..=10`); counts only when the workflow has a `fix` stage.
    pub fix_rounds: u32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WorkflowEntry {
    /// `auto | manual`
    pub trigger: String,
    /// A pull request matches when it carries any of these labels; empty matches all.
    pub labels: Vec<String>,
    /// A pull request matches when its author is any of these logins; empty matches all.
    pub authors: Vec<String>,
    /// Glob patterns on the head branch name; empty matches all.
    pub branches: Vec<String>,
    /// Path patterns (`path_scope` syntax); a pull request matches when it touches one included path.
    pub paths: Vec<String>,
    pub include_forks: bool,
    pub include_drafts: bool,
    /// The label that asks for a review when the trigger is `manual`.
    pub trigger_label: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WorkflowStage {
    /// `dev | review | fix | merge`
    pub stage: String,
    /// `auto | manual`
    pub trigger: String,
}

impl From<&saka_core::workflow::Workflow> for Workflow {
    fn from(w: &saka_core::workflow::Workflow) -> Workflow {
        Workflow {
            preset: w.preset.as_str().to_string(),
            spec: w.spec,
            change_request: w.change_request.as_ref().map(|e| WorkflowEntry {
                trigger: e.trigger.as_str().to_string(),
                labels: e.labels.clone(),
                authors: e.authors.clone(),
                branches: e.branches.clone(),
                paths: e.paths.clone(),
                include_forks: e.include_forks,
                include_drafts: e.include_drafts,
                trigger_label: e.trigger_label.clone(),
            }),
            stages: w
                .stages
                .iter()
                .map(|s| WorkflowStage {
                    stage: s.stage.as_str().to_string(),
                    trigger: s.trigger.as_str().to_string(),
                })
                .collect(),
            fix_rounds: w.fix_rounds,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub workspace: Workspace,
    /// Bindings from `saka.toml`, the default first, then by `name`.
    pub repos: Vec<ProjectRepo>,
    /// The workflow in force, from `[workflow]` in `saka.toml`.
    pub workflow: Workflow,
    /// Operator hooks that `[hooks] skip` in `saka.toml` turns off for this project.
    pub hooks_skipped: Vec<String>,
    /// `bot | user | user_pat`: who authors the agent's commits and opens its pull requests.
    pub authorship_mode: String,
    pub config_commit: String,
    /// `sha256:<hex>` of the `saka.toml` content in force.
    pub config_sha256: String,
    pub config_error: Option<ProjectConfigError>,
    pub warnings: Vec<ProjectWarning>,
    pub created_at: String,
    pub updated_at: String,
}

impl RepoAutonomy {
    fn of(l: &saka_core::autonomy::Levels) -> RepoAutonomy {
        RepoAutonomy {
            requested: l.requested.map(|a| a.as_str().to_string()),
            allowed: l.allowed.as_str().to_string(),
            effective: l.effective.as_str().to_string(),
        }
    }
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
                        base_branch: pr.base_branch.clone(),
                        is_default: pr.is_default,
                        is_workspace: pr.repo_id == p.workspace_repo_id,
                        path_scope: pr.path_scope.clone(),
                        effective_path_scope: p.effective_scope(pr),
                        autonomy: RepoAutonomy::of(&v.ceilings.levels(&r.full_name, pr.autonomy)),
                    })
                })
                .collect(),
            workflow: Workflow::from(&p.workflow),
            hooks_skipped: p.hook_skip.clone(),
            authorship_mode: v.authorship_mode.as_str().to_string(),
            config_commit: p.config_commit.clone(),
            config_sha256: p.config_sha256.clone(),
            config_error: p.config_error.as_ref().map(|e| ProjectConfigError {
                commit: e.commit.clone(),
                code: e.code.clone(),
                params: e.params.clone(),
                observed_at: time(e.observed_at),
            }),
            warnings: v
                .warnings
                .iter()
                .map(|w| ProjectWarning {
                    code: "path_scope_overlap".into(),
                    params: serde_json::json!({
                        "repo": w.repo,
                        "repo_id": w.repo_id,
                        "other_project_id": w.other_project_id,
                        "other_project": w.other_project,
                        "other_repo": w.other_repo,
                    }),
                })
                .collect(),
            created_at: time(p.created_at),
            updated_at: time(p.updated_at),
        }
    }
}

/// Result of re-reading `saka.toml`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectReload {
    pub project: Project,
    /// `changed | unchanged`
    pub outcome: String,
}

impl ProjectReload {
    pub fn from_view(v: &ReloadView) -> ProjectReload {
        ProjectReload {
            project: Project::from_view(&v.project),
            outcome: v.outcome.as_str().to_string(),
        }
    }
}

/// Request body of a reload; no fields.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReloadBody {}

/// Request body of a review request; no fields.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewBody {}

/// Request body of a fix request; no fields.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FixBody {}

/// Request body that takes a pull request opened outside Saka and asks for its review.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ExternalChangeRequest {
    /// The name of the repo binding in the project; `null` means the default repo.
    pub repo: Option<String>,
    /// The pull request number, 1 or more.
    pub number: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Revision {
    pub id: Uuid,
    pub source_commit: String,
    pub content_hash: String,
    /// Version of the parser that read this revision.
    pub parser_version: u32,
    /// `sha256:<hex>` over the resolved context references; part of the revision identity.
    pub context_hash: String,
    /// Context documents pinned with this revision, in work packet order (`link` first, then `always`).
    pub context: Vec<ContextDoc>,
    pub created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RevisionRef {
    pub id: Uuid,
    /// Commit of the workspace repo the revision was read at.
    pub source_commit: String,
    /// `sha256:<hex>` over the raw bytes of the spec file.
    pub content_hash: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ContextDoc {
    /// Path relative to the workspace repo root.
    pub path: String,
    /// `link` (linked from the spec) or `always` (marked `context: always` in the project folder).
    pub origin: String,
    /// Line of the first link in the spec; `1` for `always`.
    pub line: u32,
    pub bytes: u64,
    /// `sha256:<hex>` of the raw bytes.
    pub content_hash: String,
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

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SpecDecisionOption {
    /// Markdown source after the checkbox, trimmed, at most 500 characters (cut with `…`).
    pub text: String,
    pub line: u32,
    /// The option text contains `(recommended)`.
    pub recommended: bool,
    /// The checkbox is checked (`[x]` or `[X]`).
    pub chosen: bool,
}

/// One question from the spec's `## Decisions` section.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SpecDecision {
    /// `Q<n>`, unique in the spec.
    pub id: String,
    pub question: String,
    /// Line of the `### Q<n>:` heading.
    pub line: u32,
    pub options: Vec<SpecDecisionOption>,
    /// 1-based number of the checked option; null while the decision is open.
    pub chosen: Option<u32>,
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
    /// The `Repo` cell as written; null when the column is absent or the cell is empty (the project's default repo).
    pub repo: Option<String>,
    /// Repo the step runs in: the task's repo once the step has a task, otherwise `repo` resolved against the project's current repos; null when the name is not bound to the project.
    pub repo_id: Option<Uuid>,
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
    /// Changed fields in the order `work`, `ac`, `commands`, `after`, `repo`; only for `changed`.
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
    /// The revision that was approved; null before the first approval.
    pub approved_revision: Option<RevisionRef>,
    /// Null unless the spec is approved and a newer revision is recorded.
    pub revision_plan: Option<RevisionPlan>,
    pub lint: Lint,
    /// Left out of list items.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<Vec<AcceptanceCriterion>>,
    pub steps: Vec<Step>,
    /// Decisions in the latest revision, in written order; empty in list items.
    pub decisions: Vec<SpecDecision>,
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
    #[serde(default)]
    repo: Option<String>,
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
        let decisions: Vec<SpecDecision> = r
            .model
            .get("decisions")
            .filter(|_| with_criteria)
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
                context_hash: r.context_hash.clone(),
                context: r
                    .context
                    .iter()
                    .map(|c| ContextDoc {
                        path: c.path.clone(),
                        origin: c.origin.as_str().to_string(),
                        line: c.line,
                        bytes: c.bytes,
                        content_hash: c.content_hash.clone(),
                    })
                    .collect(),
                created_at: time(r.created_at),
            },
            approved_revision: v.approved_revision.as_ref().map(|a| RevisionRef {
                id: a.id.0,
                source_commit: a.source_commit.clone(),
                content_hash: a.content_hash.clone(),
            }),
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
                        .find(|w| w.step_id == st.id)
                        .map(|w| w.work_item_id.0),
                    repo_id: v
                        .step_repos
                        .iter()
                        .find(|(id, _)| *id == st.id)
                        .and_then(|(_, r)| r.map(|r| r.0)),
                    id: st.id,
                    index: st.index,
                    work: st.work,
                    ac: st.ac,
                    commands: st.commands,
                    after: st.after,
                    repo: st.repo,
                })
                .collect(),
            decisions,
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
    pub merged_at: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DocsChangeRequest {
    pub id: Uuid,
    pub number: u64,
    pub url: String,
    /// `open`, `merged` or `closed`.
    pub state: String,
    pub head_sha: String,
    pub base_branch: String,
    pub merge_commit_sha: Option<String>,
    pub merged_at: Option<String>,
    /// `true` when it was closed without merge and the task went on without it.
    pub skipped: bool,
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
    /// `scm` or `saka`.
    pub source: String,
    /// Whether this source decides merge readiness under the repo's `[gate] source`.
    pub required: bool,
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
    pub primary_action: PrimaryAction,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PrimaryAction {
    /// `retry | cancel | set_secret | link_account | set_user_token | edit_spec`. Only `retry` and
    /// `cancel` run through `actions/primary`; the others need input the client collects.
    pub kind: String,
    /// `set_secret`: `{ names }`; `link_account`: `{ integration_id }`; otherwise `{}`.
    pub params: Value,
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
pub struct PortBlock {
    /// First port of the block.
    pub start: u16,
    /// Ports the recipe asked for, starting at `start`.
    pub count: u16,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Toolchain {
    /// `go`, `node` or `pnpm`.
    pub tool: String,
    pub version: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EnvironmentDocs {
    /// The workspace repo the checkout is made from.
    pub repo_id: Uuid,
    pub full_name: String,
    /// The commit the task's spec revision is pinned to.
    pub commit: String,
    /// The project folder in the workspace repo; `.` is the whole repo.
    pub project_path: String,
    /// The directory the session reads (`<environment root>/docs/<project_path>`).
    pub path: String,
    /// `read_only` or `writable`.
    pub mode: String,
    /// The docs branch of a `writable` checkout; `null` when `read_only`.
    pub branch: Option<String>,
    /// The commit the checkout started at.
    pub base_sha: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WorkItemEnvironment {
    pub id: Uuid,
    /// `preparing`, `ready`, `failed`, `teardown_failed` or `torn_down`.
    pub state: String,
    /// `null` when the recipe asks for no ports or the environment is torn down.
    pub ports: Option<PortBlock>,
    /// SHA-256 of `.saka/environment.toml` at the base commit; `null` when the repo has no recipe.
    pub recipe_sha256: Option<String>,
    /// Toolchains on `PATH`; empty when the repo declares no versions.
    pub toolchains: Vec<Toolchain>,
    /// Checkout of the project folder from the workspace repo; `null` when the task's repo is
    /// the workspace repo.
    pub docs: Option<EnvironmentDocs>,
}

impl WorkItemEnvironment {
    fn from_env(e: &CoreEnvironment) -> WorkItemEnvironment {
        let wanted = e
            .recipe
            .as_ref()
            .and_then(|r| r.get("ports"))
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok());
        let ports = match (e.port_start, e.port_count, e.state) {
            (_, _, EnvironmentState::TornDown) => None,
            (Some(start), Some(block), _) => match wanted.unwrap_or(block) {
                0 => None,
                count => Some(PortBlock { start, count }),
            },
            _ => None,
        };
        WorkItemEnvironment {
            id: e.id.0,
            state: e.state.as_str().to_string(),
            ports,
            recipe_sha256: e.recipe_sha256.clone(),
            toolchains: e
                .toolchains
                .iter()
                .map(|t| Toolchain {
                    tool: t.tool.as_str().to_string(),
                    version: t.version.clone(),
                })
                .collect(),
            docs: e.docs.as_ref().map(|d| EnvironmentDocs {
                repo_id: d.repo_id.0,
                full_name: d.full_name.clone(),
                commit: d.commit.clone(),
                project_path: d.project_path.clone(),
                path: d.path.clone(),
                mode: d.mode.as_str().to_string(),
                branch: d.branch.clone(),
                base_sha: d.base_sha.clone(),
            }),
        }
    }
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
    /// Name of the binding of `repo_id` in the project now; `null` once a reload removed it.
    pub repo_name: Option<String>,
    pub branch: String,
    /// `queued | awaiting_start | admitted | in_stage | blocked | taken_over | done | cancelled | superseded`
    pub state: String,
    pub stage: Option<String>,
    pub stage_phase: Option<String>,
    pub priority: i64,
    /// Steps and specs this task still waits for; empty unless the task is queued.
    pub waiting_on: Vec<WaitingOn>,
    /// Where the task's merge request stands in the queue of its target branch; `null` when it has
    /// none, and in the answer to an action.
    pub merge_queue: Option<MergeQueue>,
    /// `strict | standard | autonomous`: the level in force for the task's repo, the lowest of the
    /// operator's ceiling and what `saka.toml` asks for. `null` in the answer to an action.
    pub autonomy: Option<String>,
    /// `true` when Saka will request the merge itself once review and checks pass: autonomy
    /// `autonomous`, trigger `merge = "auto"`, and a review stage. `null` in the answer to an action.
    pub merge_auto: Option<bool>,
    pub change_request: Option<ChangeRequest>,
    /// The docs pull request in the workspace repo, for a task in another repo; `null` when none
    /// was opened. Gates and merge readiness never read it.
    pub docs_change_request: Option<DocsChangeRequest>,
    /// What is known about a pull request opened outside Saka; `null` for a task made from a spec step.
    pub external: Option<ExternalChange>,
    /// The newest reviewer verdict of the task; `null` before any review ran.
    pub review: Option<ReviewSummary>,
    /// The latest fix round of the task; `null` before a round ran and when the workflow has no `fix` stage.
    pub fix: Option<FixProgress>,
    /// Latest result per gate source for the current change request head, `scm` first.
    pub gates: Vec<Gate>,
    pub blocker: Option<Blocker>,
    pub session: Option<SessionBrief>,
    pub pending_permission_count: u32,
    /// Subset of `merge`, `retry`, `cancel`, `review`.
    pub available_actions: Vec<String>,
    /// Use and limits of the running stage run; `null` when no stage run with a session runs.
    pub budget: Option<WorkItemBudget>,
    /// The latest environment of the task; `null` before one was prepared.
    pub environment: Option<WorkItemEnvironment>,
    pub version: u64,
    pub created_at: String,
    pub updated_at: String,
    pub admitted_at: Option<String>,
    pub done_at: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MergeQueue {
    pub base_branch: String,
    /// `0` when the task holds the turn, otherwise its place among those waiting, starting at `1`.
    pub position: u32,
    /// Tasks holding the turn plus tasks waiting for it.
    pub size: u32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ExternalChange {
    pub title: String,
    /// The login of the pull request's author.
    pub author: String,
    /// The pull request comes from a fork of the repo.
    pub from_fork: bool,
    pub draft: bool,
    /// The spec step the pull request names; `null` means the review has no spec behind it.
    pub spec: Option<ExternalSpec>,
    /// `auto | comment | label | operator`: what made Saka take the pull request.
    pub cause: String,
    /// The login of who asked for it, or `null` when Saka took it on its own.
    pub actor: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FixProgress {
    /// The highest round started. It passes `max_rounds` only when the operator granted another round after the limit.
    pub round: u32,
    /// The rounds the workflow allows a task.
    pub max_rounds: u32,
    /// `review | conflict`: what the round answered.
    pub cause: String,
}

impl FixProgress {
    fn of(t: &Task) -> Option<FixProgress> {
        if !t.workflow.has_stage(saka_core::vocab::Stage::Fix) {
            return None;
        }
        let latest = t
            .retired_runs
            .iter()
            .chain(t.stage_run.iter())
            .filter_map(|r| r.fix.as_ref())
            .max_by_key(|f| f.number)?;
        Some(FixProgress {
            round: latest.number,
            max_rounds: t.workflow.fix_rounds,
            cause: if latest.cause.code == "merge_conflict" {
                "conflict"
            } else {
                "review"
            }
            .to_string(),
        })
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReviewSummary {
    pub id: Uuid,
    /// `approved | changes_requested | spec_unclear`
    pub outcome: String,
    /// `step | spec | none`; `none` means the reviewer judged without a spec.
    pub basis: String,
    /// The head the verdict judged.
    pub head_sha: String,
    /// The verdict judged the head the change request has now.
    pub current: bool,
    pub summary: String,
    pub finding_count: u32,
    /// Where the review was posted; `null` before it was posted and for `spec_unclear`.
    pub posted: Option<ReviewPostedInfo>,
    pub created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReviewPostedInfo {
    /// `approve | request_changes | comment`
    pub event: String,
    pub url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReviewFinding {
    pub path: Option<String>,
    pub line: Option<u32>,
    /// `blocker | major | minor | nit`
    pub severity: String,
    pub body: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReviewDetail {
    #[serde(flatten)]
    pub summary: ReviewSummary,
    pub findings: Vec<ReviewFinding>,
    /// What the reviewer could not judge; only for `spec_unclear`.
    pub questions: Vec<String>,
}

fn latest_review(t: &Task) -> Option<&saka_core::model::Review> {
    t.reviews.iter().max_by_key(|r| r.created_at)
}

impl ReviewSummary {
    fn of(r: &saka_core::model::Review, t: &Task) -> ReviewSummary {
        ReviewSummary {
            id: r.id.0,
            outcome: r.outcome.as_str().to_string(),
            basis: r.basis.as_str().to_string(),
            head_sha: r.head_sha.clone(),
            current: t
                .change_request
                .as_ref()
                .is_some_and(|c| c.head_sha == r.head_sha),
            summary: r.summary.clone(),
            finding_count: u32::try_from(r.findings.len()).unwrap_or(u32::MAX),
            posted: r.posted.as_ref().map(|p| ReviewPostedInfo {
                event: p.event.as_str().to_string(),
                url: p.url.clone(),
            }),
            created_at: time(r.created_at),
        }
    }
}

impl ReviewDetail {
    fn of(r: &saka_core::model::Review, t: &Task) -> ReviewDetail {
        ReviewDetail {
            summary: ReviewSummary::of(r, t),
            findings: r
                .findings
                .iter()
                .map(|f| ReviewFinding {
                    path: f.path.clone(),
                    line: f.line,
                    severity: f.severity.as_str().to_string(),
                    body: f.body.clone(),
                })
                .collect(),
            questions: r.questions.clone(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ExternalSpec {
    pub spec_id: Uuid,
    pub slug: Option<String>,
    /// `S1`, `S2`, ...; `null` when the pull request names the spec but no step that exists.
    pub step_id: Option<String>,
}

impl From<&saka_core::model::ExternalChange> for ExternalChange {
    fn from(x: &saka_core::model::ExternalChange) -> ExternalChange {
        ExternalChange {
            title: x.title.clone(),
            author: x.author.clone(),
            from_fork: x.from_fork,
            draft: x.draft,
            spec: x.spec.as_ref().map(|s| ExternalSpec {
                spec_id: s.spec_id.0,
                slug: x.spec_slug.clone(),
                step_id: s.step_id.clone(),
            }),
            cause: x.cause.as_str().to_string(),
            actor: x.actor.clone(),
        }
    }
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
    /// Every port block in `env.port_range` is held by a live environment.
    PortBlock,
    /// Every architect slot (`architect.max_active`) is held by another draft.
    ArchitectSlot,
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
            Waiting::PortBlock => WaitingOn::PortBlock,
            Waiting::ArchitectSlot => WaitingOn::ArchitectSlot,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Measure {
    pub used: Value,
    /// `0` means no limit.
    pub limit: Value,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WorkItemBudget {
    pub stage_run_id: Uuid,
    pub warn_percent: u32,
    /// Minutes; `used` is a decimal number.
    pub running_minutes: Measure,
    pub turns: Measure,
    pub tokens: Measure,
    /// Decimal strings in dollars.
    pub cost_usd: Measure,
}

impl WorkItemBudget {
    pub fn from_view(v: &BudgetView) -> WorkItemBudget {
        let minutes = (v.used.running_ms as f64 / 60_000.0 * 10.0).round() / 10.0;
        WorkItemBudget {
            stage_run_id: v.stage_run_id.0,
            warn_percent: v.warn_percent,
            running_minutes: Measure {
                used: Value::from(minutes),
                limit: Value::from(v.limit.running_minutes),
            },
            turns: Measure {
                used: Value::from(v.used.turns),
                limit: Value::from(v.limit.turns),
            },
            tokens: Measure {
                used: Value::from(v.used.tokens),
                limit: Value::from(v.limit.tokens),
            },
            cost_usd: Measure {
                used: Value::from(usd(v.used.cost_usd_micros)),
                limit: Value::from(usd(v.limit.cost_usd_micros)),
            },
        }
    }
}

impl WorkItem {
    pub fn from_view(v: &TaskView) -> WorkItem {
        WorkItem::from_task(
            &v.task,
            v.repo_name.as_deref(),
            &v.waiting_on,
            v.budget.as_ref(),
        )
        .merging(v.merges)
        .queued(v.merge_queue.as_ref())
        .levelled(v.autonomy, v.merge_auto)
    }

    fn levelled(mut self, autonomy: saka_core::vocab::Autonomy, merge_auto: bool) -> WorkItem {
        self.autonomy = Some(autonomy.as_str().to_string());
        self.merge_auto = Some(merge_auto);
        self
    }

    fn queued(mut self, place: Option<&saka_core::queue::Position>) -> WorkItem {
        self.merge_queue = place.map(|p| MergeQueue {
            base_branch: p.base_branch.clone(),
            position: p.position,
            size: p.size,
        });
        self
    }

    fn merging(mut self, merges: bool) -> WorkItem {
        if !merges {
            self.available_actions.retain(|a| a != "merge");
        }
        self
    }

    pub fn from_task(
        t: &Task,
        repo_name: Option<&str>,
        waiting_on: &[Waiting],
        budget: Option<&BudgetView>,
    ) -> WorkItem {
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
            repo_name: repo_name.map(str::to_string),
            branch: i.branch.clone(),
            state: i.state.as_str().to_string(),
            stage: i.stage.map(|x| x.as_str().to_string()),
            stage_phase: i.stage_phase.map(|x| x.as_str().to_string()),
            priority: i.priority,
            waiting_on: waiting_on.iter().map(WaitingOn::from).collect(),
            merge_queue: None,
            autonomy: None,
            merge_auto: None,
            change_request: t.change_request.as_ref().map(|c| ChangeRequest {
                id: c.id.0,
                number: c.number,
                url: c.url.clone(),
                state: c.state.as_str().to_string(),
                head_sha: c.head_sha.clone(),
                base_branch: c.base_branch.clone(),
                merge_commit_sha: c.merge_commit_sha.clone(),
                merged_at: opt_time(c.merged_at),
            }),
            docs_change_request: t.docs_change.as_ref().map(|c| DocsChangeRequest {
                id: c.id.0,
                number: c.number,
                url: c.url.clone(),
                state: c.state.as_str().to_string(),
                head_sha: c.head_sha.clone(),
                base_branch: c.base_branch.clone(),
                merge_commit_sha: c.merge_commit_sha.clone(),
                merged_at: opt_time(c.merged_at),
                skipped: c.skipped_at.is_some(),
            }),
            external: t.external.as_ref().map(ExternalChange::from),
            review: latest_review(t).map(|r| ReviewSummary::of(r, t)),
            fix: FixProgress::of(t),
            gates: gates_at_head(t),
            blocker: t
                .open_blocker()
                .zip(primary_action(t))
                .map(|(b, action)| Blocker {
                    id: b.id.0,
                    code: b.code.as_str().to_string(),
                    params: b.params.clone(),
                    opened_at: time(b.opened_at),
                    stage: t.stage_run.as_ref().map(|r| r.stage.as_str().to_string()),
                    primary_action: PrimaryAction {
                        kind: action.kind.as_str().to_string(),
                        params: action.params,
                    },
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
            budget: budget.map(WorkItemBudget::from_view),
            environment: t.environment.as_ref().map(WorkItemEnvironment::from_env),
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
    /// The fix round the run belongs to; `null` for every other run.
    pub round: Option<u32>,
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
            round: r.fix.as_ref().map(|f| f.number),
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
    /// Every verdict of the task, newest first.
    pub reviews: Vec<ReviewDetail>,
}

impl WorkItemDetail {
    pub fn from_detail(d: &TaskDetail) -> WorkItemDetail {
        WorkItemDetail {
            item: WorkItem::from_task(
                &d.task,
                d.repo_name.as_deref(),
                &d.waiting_on,
                d.budget.as_ref(),
            )
            .merging(d.merges)
            .queued(d.merge_queue.as_ref())
            .levelled(d.autonomy, d.merge_auto),
            stage_runs: d.stage_runs.iter().map(StageRunItem::from).collect(),
            reviews: {
                let mut all: Vec<&saka_core::model::Review> = d.task.reviews.iter().collect();
                all.sort_by_key(|r| std::cmp::Reverse(r.created_at));
                all.into_iter()
                    .map(|r| ReviewDetail::of(r, &d.task))
                    .collect()
            },
        }
    }
}

/// A spec that the architect drafts from an idea, until Saka's lint passes and the file is committed.
#[derive(Debug, Serialize, ToSchema)]
pub struct SpecDraft {
    pub id: Uuid,
    pub project_id: Uuid,
    /// The new spec file, from the repo root.
    pub path: String,
    /// The file name without its extension.
    pub slug: String,
    /// As the operator wrote it; `null` in lists.
    pub idea: Option<String>,
    /// `en | id`
    pub locale: String,
    pub context_paths: Vec<String>,
    /// `queued | preparing | drafting | committing | succeeded | failed | cancelled`
    pub state: String,
    /// The latest attempt; `null` before the first admission.
    pub attempt: Option<SpecDraftAttempt>,
    /// The latest lint round of any attempt; `issues` is empty in lists.
    pub last_round: Option<SpecDraftLastRound>,
    /// The latest session of the latest attempt.
    pub session: Option<SessionBrief>,
    /// Use and limits of the running attempt; `null` when no attempt runs.
    pub budget: Option<SpecDraftBudget>,
    /// Only while `queued`: an architect slot or an agent account window. Empty in action responses.
    pub waiting_on: Vec<WaitingOn>,
    /// `{code, params}` while `failed`.
    pub failure: Option<Failure>,
    /// `retry | open_written | open_file`; `null` unless `failed`.
    pub primary_action: Option<String>,
    /// Subset of `retry`, `cancel`.
    pub available_actions: Vec<String>,
    /// Manual retries so far, at most 3.
    pub retries: u32,
    /// The registered spec once `succeeded`.
    pub spec_id: Option<Uuid>,
    /// The docs commit once `succeeded`, and for `failed` with `register_failed`.
    pub commit: Option<String>,
    pub version: u64,
    pub created_at: String,
    pub updated_at: String,
    pub finished_at: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SpecDraftAttempt {
    pub id: Uuid,
    pub number: u32,
    /// `auto` (first) or `manual` (retry).
    pub trigger: String,
    pub reason: Option<String>,
    pub note: Option<String>,
    /// `running | succeeded | failed | cancelled`
    pub state: String,
    /// The base branch commit the attempt's environment was prepared at.
    pub base_sha: Option<String>,
    pub rounds: u32,
    /// `architect.lint_rounds`.
    pub max_rounds: u32,
    pub packet_sha256: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SpecDraftLastRound {
    /// Attempt number.
    pub attempt: u32,
    pub round: u32,
    /// `passed | failed | missing`
    pub outcome: String,
    /// `null` when the file was missing.
    pub bytes: Option<u64>,
    /// `sha256:<hex>`; `null` when the file was missing or too large.
    pub content_hash: Option<String>,
    pub decisions_open: u32,
    pub parser_version: u32,
    pub issues: Vec<LintIssue>,
    pub created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SpecDraftLintRound {
    /// Attempt number.
    pub attempt: u32,
    pub round: u32,
    pub session_id: Uuid,
    pub turn: u32,
    /// `passed | failed | missing`
    pub outcome: String,
    pub bytes: Option<u64>,
    pub content_hash: Option<String>,
    pub parser_version: u32,
    pub decisions_open: u32,
    /// Same shape as `Spec.lint.issues`, context issues included.
    pub issues: Vec<LintIssue>,
    pub created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SpecDraftLintRoundList {
    /// Ascending by attempt, then round.
    pub items: Vec<SpecDraftLintRound>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SpecDraftBudget {
    pub attempt_id: Uuid,
    pub warn_percent: u32,
    /// Minutes; `used` is a decimal number.
    pub running_minutes: Measure,
    pub turns: Measure,
    pub tokens: Measure,
    /// Decimal strings in dollars.
    pub cost_usd: Measure,
}

/// The latest file content the architect wrote.
#[derive(Debug, Serialize, ToSchema)]
pub struct SpecDraftContent {
    pub attempt: u32,
    pub round: u32,
    pub path: String,
    pub bytes: u64,
    pub content_hash: Option<String>,
    /// The bytes as written, as UTF-8 (line endings kept).
    pub content: String,
}

/// The packet of the latest attempt, as sent to the architect.
#[derive(Debug, Serialize, ToSchema)]
pub struct SpecDraftPacket {
    pub attempt: u32,
    pub base_sha: String,
    /// SHA-256 hex of `text`.
    pub sha256: String,
    pub documents: Vec<SpecDraftPacketDocument>,
    pub text: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SpecDraftPacketDocument {
    pub index: u32,
    /// What the packet names the document by, for example `docs/prd.md @ <commit>`.
    pub source: String,
    /// `null` for the template and the rules.
    pub path: Option<String>,
    /// `template | rules | selected | always`
    pub origin: String,
    pub bytes: u64,
    pub content_hash: String,
}

fn issues_of(values: &[Value]) -> Vec<LintIssue> {
    values
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect()
}

impl SpecDraftBudget {
    fn from_view(v: &DraftBudgetView) -> SpecDraftBudget {
        let minutes = (v.used.running_ms as f64 / 60_000.0 * 10.0).round() / 10.0;
        SpecDraftBudget {
            attempt_id: v.attempt_id.0,
            warn_percent: v.warn_percent,
            running_minutes: Measure {
                used: Value::from(minutes),
                limit: Value::from(v.limit.running_minutes),
            },
            turns: Measure {
                used: Value::from(v.used.turns),
                limit: Value::from(v.limit.turns),
            },
            tokens: Measure {
                used: Value::from(v.used.tokens),
                limit: Value::from(v.limit.tokens),
            },
            cost_usd: Measure {
                used: Value::from(usd(v.used.cost_usd_micros)),
                limit: Value::from(usd(v.limit.cost_usd_micros)),
            },
        }
    }
}

impl SpecDraftAttempt {
    fn from_attempt(a: &CoreAttempt, max_rounds: u32) -> SpecDraftAttempt {
        SpecDraftAttempt {
            id: a.id.0,
            number: a.attempt,
            trigger: a.trigger.as_str().to_string(),
            reason: a.reason.clone(),
            note: a.note.clone(),
            state: a.state.as_str().to_string(),
            base_sha: a.base_sha.clone(),
            rounds: a.rounds,
            max_rounds,
            packet_sha256: a.packet_sha256.clone(),
            started_at: time(a.started_at),
            ended_at: opt_time(a.ended_at),
        }
    }
}

impl SpecDraftLintRound {
    pub fn from_round(attempt: u32, r: &LintRound) -> SpecDraftLintRound {
        SpecDraftLintRound {
            attempt,
            round: r.round,
            session_id: r.session_id.0,
            turn: r.turn,
            outcome: r.outcome.as_str().to_string(),
            bytes: r.bytes,
            content_hash: r.content_hash.clone(),
            parser_version: r.parser_version,
            decisions_open: r.decisions_open,
            issues: issues_of(&r.issues),
            created_at: time(r.created_at),
        }
    }
}

impl SpecDraft {
    pub fn from_view(v: &DraftView) -> SpecDraft {
        SpecDraft::build(v, true)
    }

    pub fn list_item(v: &DraftView) -> SpecDraft {
        SpecDraft::build(v, false)
    }

    fn build(v: &DraftView, full: bool) -> SpecDraft {
        let d = &v.draft.draft;
        let owner = v.draft.run_owner();
        SpecDraft {
            id: d.id.0,
            project_id: d.project_id.0,
            path: d.path.clone(),
            slug: d.slug.clone(),
            idea: full.then(|| d.idea.clone()),
            locale: d.locale.as_str().to_string(),
            context_paths: d.context_paths.clone(),
            state: d.state.as_str().to_string(),
            attempt: v
                .draft
                .attempt
                .as_ref()
                .map(|a| SpecDraftAttempt::from_attempt(a, v.max_rounds)),
            last_round: v
                .last_round
                .as_ref()
                .map(|(attempt, r)| SpecDraftLastRound {
                    attempt: *attempt,
                    round: r.round,
                    outcome: r.outcome.as_str().to_string(),
                    bytes: r.bytes,
                    content_hash: r.content_hash.clone(),
                    decisions_open: r.decisions_open,
                    parser_version: r.parser_version,
                    issues: if full {
                        issues_of(&r.issues)
                    } else {
                        Vec::new()
                    },
                    created_at: time(r.created_at),
                }),
            session: v
                .draft
                .session
                .as_ref()
                .filter(|s| owner.is_some_and(|o| s.owner == o))
                .map(|s| SessionBrief {
                    id: s.id.0,
                    state: s.state.as_str().to_string(),
                    cli: s.cli.clone(),
                    started_at: opt_time(s.started_at),
                    ended_at: opt_time(s.ended_at),
                }),
            budget: v.budget.as_ref().map(SpecDraftBudget::from_view),
            waiting_on: v.waiting_on.iter().map(WaitingOn::from).collect(),
            failure: d.failure.as_ref().map(|f| Failure {
                code: f.code.clone(),
                params: f.params.clone(),
            }),
            primary_action: v.primary_action.map(|a| a.as_str().to_string()),
            available_actions: v.actions.iter().map(|a| (*a).to_string()).collect(),
            retries: d.retries,
            spec_id: d.spec_id.map(|x| x.0),
            commit: d.commit_sha.clone(),
            version: d.version,
            created_at: time(d.created_at),
            updated_at: time(d.updated_at),
            finished_at: opt_time(d.finished_at),
        }
    }
}

impl SpecDraftContent {
    pub fn from_view(v: &DraftContentView) -> SpecDraftContent {
        SpecDraftContent {
            attempt: v.attempt,
            round: v.round,
            path: v.path.clone(),
            bytes: v.bytes,
            content_hash: v.content_hash.clone(),
            content: v.content.clone(),
        }
    }
}

impl SpecDraftPacket {
    pub fn from_view(v: &DraftPacketView) -> SpecDraftPacket {
        SpecDraftPacket {
            attempt: v.attempt,
            base_sha: v.base_sha.clone(),
            sha256: v.sha256.clone(),
            documents: v
                .documents
                .iter()
                .map(|d| SpecDraftPacketDocument {
                    index: d.document.index,
                    source: d.source.clone(),
                    path: d.document.path.clone(),
                    origin: d.document.origin.as_str().to_string(),
                    bytes: d.document.bytes,
                    content_hash: d.document.content_hash.clone(),
                })
                .collect(),
            text: v.text.clone(),
        }
    }
}

/// The run that owns a session, its permission requests, and its usage records.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunOwner {
    /// A stage run of a task.
    StageRun {
        work_item_id: Uuid,
        stage_run_id: Uuid,
    },
    /// An attempt of a spec draft.
    DraftAttempt {
        spec_draft_id: Uuid,
        attempt_id: Uuid,
    },
}

impl From<&CoreOwner> for RunOwner {
    fn from(o: &CoreOwner) -> RunOwner {
        match o {
            CoreOwner::StageRun {
                work_item_id,
                stage_run_id,
            } => RunOwner::StageRun {
                work_item_id: work_item_id.0,
                stage_run_id: stage_run_id.0,
            },
            CoreOwner::DraftAttempt {
                spec_draft_id,
                attempt_id,
            } => RunOwner::DraftAttempt {
                spec_draft_id: spec_draft_id.0,
                attempt_id: attempt_id.0,
            },
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PermissionRequest {
    pub id: Uuid,
    pub project_id: Uuid,
    pub owner: RunOwner,
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
            owner: RunOwner::from(&p.owner),
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
    pub owner: RunOwner,
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
    /// The session this one continues.
    pub resumed_from_session_id: Option<Uuid>,
    /// `lost | retry | null`
    pub resume_cause: Option<String>,
    pub end_reason: Option<Failure>,
    /// Subset of `steer`, `interrupt`, `stop`.
    pub available_actions: Vec<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionList {
    pub items: Vec<Session>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SessionActivity {
    /// Offset of the runner event.
    pub offset: u64,
    pub at: String,
    /// `system | thought | message | action | output | permission | result | steer`
    pub kind: String,
    /// Redacted, at most 500 characters.
    pub summary: String,
    pub data: Value,
}

impl From<&CoreActivity> for SessionActivity {
    fn from(a: &CoreActivity) -> SessionActivity {
        SessionActivity {
            offset: a.offset,
            at: time(a.at),
            kind: a.kind.clone(),
            summary: a.summary.clone(),
            data: a.data.clone(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Timeline {
    /// Ascending by `offset`.
    pub items: Vec<SessionActivity>,
    /// Offset of the last item when newer activities existed at read time; pass as `after`. `null` otherwise.
    pub next_after: Option<u64>,
    /// Offset of the first item when older activities exist; pass as `before`. `null` otherwise.
    pub prev_before: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleEffect {
    Allow,
    Deny,
    Ask,
}

impl From<CoreEffect> for RuleEffect {
    fn from(e: CoreEffect) -> RuleEffect {
        match e {
            CoreEffect::Allow => RuleEffect::Allow,
            CoreEffect::Deny => RuleEffect::Deny,
            CoreEffect::Ask => RuleEffect::Ask,
        }
    }
}

impl From<RuleEffect> for CoreEffect {
    fn from(e: RuleEffect) -> CoreEffect {
        match e {
            RuleEffect::Allow => CoreEffect::Allow,
            RuleEffect::Deny => CoreEffect::Deny,
            RuleEffect::Ask => CoreEffect::Ask,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleMatch {
    Exact,
    Glob,
}

impl From<CoreMatch> for RuleMatch {
    fn from(m: CoreMatch) -> RuleMatch {
        match m {
            CoreMatch::Exact => RuleMatch::Exact,
            CoreMatch::Glob => RuleMatch::Glob,
        }
    }
}

impl From<RuleMatch> for CoreMatch {
    fn from(m: RuleMatch) -> CoreMatch {
        match m {
            RuleMatch::Exact => CoreMatch::Exact,
            RuleMatch::Glob => CoreMatch::Glob,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PermissionRule {
    pub id: Uuid,
    pub project_id: Uuid,
    pub effect: RuleEffect,
    pub tool: String,
    /// `exact` compares text verbatim; `glob` is a glob (`*` does not cross `/` in `path`).
    #[serde(rename = "match")]
    pub match_kind: RuleMatch,
    /// One part of a Bash command (see `match`); `null` for other tools.
    pub command: Option<String>,
    /// Worktree-relative path the rule applies to; `null` means every path.
    pub path: Option<String>,
    pub note: Option<String>,
    /// The request whose answer created the rule; `null` for a rule written by hand.
    pub permission_request_id: Option<Uuid>,
    pub created_by: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
    pub revoke_reason: Option<String>,
}

impl From<&CoreRule> for PermissionRule {
    fn from(r: &CoreRule) -> PermissionRule {
        PermissionRule {
            id: r.id.0,
            project_id: r.project_id.0,
            effect: r.effect.into(),
            tool: r.tool.clone(),
            match_kind: r.match_kind.into(),
            command: r.command.clone(),
            path: r.path.clone(),
            note: r.note.clone(),
            permission_request_id: r.permission_request_id.map(|p| p.0),
            created_by: r.created_by.clone(),
            created_at: time(r.created_at),
            revoked_at: opt_time(r.revoked_at),
            revoke_reason: r.revoke_reason.clone(),
        }
    }
}

/// A rule from the operator's `config.toml`; read-only here.
#[derive(Debug, Serialize, ToSchema)]
pub struct OperatorPermissionRule {
    pub id: String,
    pub effect: RuleEffect,
    pub tools: Vec<String>,
    /// Globs relative to the worktree; empty means any path.
    pub paths: Vec<String>,
    /// Globs matched against one part of a Bash command; empty means any command.
    pub commands: Vec<String>,
}

impl From<&saka_core::policy::RuleSpec> for OperatorPermissionRule {
    fn from(r: &saka_core::policy::RuleSpec) -> OperatorPermissionRule {
        OperatorPermissionRule {
            id: r.id.clone(),
            effect: r.effect.into(),
            tools: r.tools.clone(),
            paths: r.paths.clone(),
            commands: r.commands.clone(),
        }
    }
}

/// Who decided: `builtin`, `rule` (operator config), `project` (a project rule, `name` is its id), `default` (nothing matched, so ask).
#[derive(Debug, Serialize, ToSchema)]
pub struct DecisionOrigin {
    pub kind: String,
    pub name: Option<String>,
}

impl From<&saka_core::policy::DecisionSource> for DecisionOrigin {
    fn from(s: &saka_core::policy::DecisionSource) -> DecisionOrigin {
        use saka_core::policy::DecisionSource as S;
        let (kind, name) = match s {
            S::Builtin(n) => ("builtin", Some(n.clone())),
            S::Rule(n) => ("rule", Some(n.clone())),
            S::Project(n) => ("project", Some(n.clone())),
            S::Architect(n) => ("architect", Some(n.clone())),
            S::Reviewer(n) => ("reviewer", Some(n.clone())),
            S::Default => ("default", None),
        };
        DecisionOrigin {
            kind: kind.into(),
            name,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EvaluatedPart {
    /// One part of a compound command, without the redirects Saka accepts.
    pub command: String,
    pub effect: RuleEffect,
    pub source: DecisionOrigin,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PermissionEvaluation {
    pub effect: RuleEffect,
    pub source: DecisionOrigin,
    /// Empty for a tool other than `Bash` and for a command that cannot be split safely.
    pub parts: Vec<EvaluatedPart>,
}

impl From<&saka_core::policy::Decision> for PermissionEvaluation {
    fn from(d: &saka_core::policy::Decision) -> PermissionEvaluation {
        PermissionEvaluation {
            effect: d.effect.into(),
            source: (&d.source).into(),
            parts: d
                .parts
                .iter()
                .map(|p| EvaluatedPart {
                    command: p.command.clone(),
                    effect: p.effect.into(),
                    source: (&p.source).into(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvaluateBody {
    pub tool: String,
    /// The Bash command; ignored for other tools.
    pub command: Option<String>,
    /// Worktree-relative path of a file tool; always treated as inside the worktree.
    pub path: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PreviewChange {
    pub permission_request_id: Uuid,
    pub tool: String,
    pub command: Option<String>,
    pub before: RuleEffect,
    pub after: RuleEffect,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PermissionPreview {
    /// How many of the project's latest task permission requests were re-judged (at most 200).
    pub scanned: u32,
    /// The requests whose answer would change if the rule existed.
    pub changed: Vec<PreviewChange>,
}

impl From<&saka_engine::PreviewView> for PermissionPreview {
    fn from(v: &saka_engine::PreviewView) -> PermissionPreview {
        PermissionPreview {
            scanned: u32::try_from(v.scanned).unwrap_or(u32::MAX),
            changed: v
                .changed
                .iter()
                .map(|c| PreviewChange {
                    permission_request_id: c.permission_request_id.0,
                    tool: c.tool.clone(),
                    command: c.command.clone(),
                    before: c.before.into(),
                    after: c.after.into(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PermissionRuleList {
    /// Active project rules, oldest first.
    pub items: Vec<PermissionRule>,
    /// The operator's rules in evaluation order; they cap what a project rule can do.
    pub operator_rules: Vec<OperatorPermissionRule>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewPermissionRule {
    pub effect: RuleEffect,
    /// 1 to 100 characters, the agent CLI's tool name (`Bash`, `Edit`, ...).
    pub tool: String,
    /// `exact` or `glob`.
    #[serde(rename = "match")]
    pub match_kind: RuleMatch,
    /// Required for `Bash`, at most 2000 characters, matched against one part of a compound command; not allowed for other tools. An `allow` glob made only of `*` is refused as too broad.
    pub command: Option<String>,
    /// Worktree-relative path, not allowed for `Bash`; omit to cover every path.
    pub path: Option<String>,
    /// At most 500 characters.
    pub note: Option<String>,
}

impl From<NewPermissionRule> for saka_core::rule::NewRule {
    fn from(r: NewPermissionRule) -> saka_core::rule::NewRule {
        saka_core::rule::NewRule {
            effect: r.effect.into(),
            tool: r.tool,
            match_kind: r.match_kind.into(),
            command: r.command,
            path: r.path,
            note: r.note,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct QuotaWindow {
    pub window: String,
    /// 0..1
    pub utilization: f64,
    /// As the CLI reports it, e.g. `allowed`, `allowed_warning`, `rejected`.
    pub status: String,
    pub resets_at: Option<String>,
    pub observed_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AgentAccount {
    pub cli: String,
    pub name: String,
    /// A window is at or above `agent.rate_limit_pause` and has not reset; new tasks wait.
    pub paused: bool,
    pub windows: Vec<QuotaWindow>,
}

impl From<&AccountView> for AgentAccount {
    fn from(a: &AccountView) -> AgentAccount {
        AgentAccount {
            cli: a.cli.clone(),
            name: a.name.clone(),
            paused: a.paused,
            windows: a
                .windows
                .iter()
                .map(|w| QuotaWindow {
                    window: w.window.clone(),
                    utilization: w.utilization,
                    status: w.status.clone(),
                    resets_at: opt_time(w.resets_at),
                    observed_at: time(w.observed_at),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AgentAccountList {
    pub items: Vec<AgentAccount>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UsageRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub owner: RunOwner,
    pub session_id: Uuid,
    /// `<cli>:<account name>`
    pub account: String,
    pub turn: u32,
    /// The CLI result subtype, e.g. `success`.
    pub status: String,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    /// Decimal string in dollars, or `null` when the CLI did not report it.
    pub cost_usd: Option<String>,
    pub running_ms: i64,
    pub ended_at: String,
}

impl From<&UsageView> for UsageRecord {
    fn from(v: &UsageView) -> UsageRecord {
        let u = &v.record;
        UsageRecord {
            id: u.id.0,
            project_id: v.project_id.0,
            owner: RunOwner::from(&u.owner),
            session_id: u.session_id.0,
            account: u.account.clone(),
            turn: u.turn,
            status: u.status.clone(),
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cost_usd: u.cost_usd_micros.map(usd),
            running_ms: u.running_ms,
            ended_at: time(u.ended_at),
        }
    }
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
            owner: RunOwner::from(&s.owner),
            cli: s.cli.clone(),
            cli_version: s.cli_version.clone(),
            model: s.model.clone(),
            state: s.state.as_str().to_string(),
            turns: s.turns,
            running_ms: s.running_ms,
            cost_usd: s.cost_usd_micros.map(usd),
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            resumed_from_session_id: s.resumed_from_session_id.map(|x| x.0),
            resume_cause: s.resume_cause.map(|c| c.as_str().to_string()),
            end_reason: s.end_reason.as_ref().map(|e| Failure {
                code: e.code.clone(),
                params: e.params.clone(),
            }),
            available_actions: session_actions(s).into_iter().map(String::from).collect(),
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

pub const API_VERSION: &str = "1";

pub const CAPABILITIES: &[&str] = &[
    "projects",
    "spec_approve",
    "work_item_merge",
    "work_item_retry",
    "work_item_cancel",
    "permission_answer",
    "events_sse",
    "spec_draft",
    "work_item_primary_action",
    "work_item_takeover",
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

/// The `data` of a `runtime.session.activity.v1` SSE message: a new timeline row to read with the timeline cursor.
#[derive(Debug, Serialize, ToSchema)]
pub struct ActivityNotice {
    pub session_id: Uuid,
    pub owner: RunOwner,
    pub offset: u64,
    pub kind: String,
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

/// Body of `PUT /api/v1/projects/{project_id}/secrets/{name}`. The value is write-only: no response, event, or log carries it, and it cannot be read back.
// No `Debug`: the value must not be printable by accident.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetSecretBody {
    /// 8 to 65536 bytes of UTF-8 without NUL. Shorter values are refused (`secret_too_short`) so output redaction always applies.
    pub value: String,
}

/// A project secret, by name only.
#[derive(Debug, Serialize, ToSchema)]
pub struct SecretName {
    /// `^[A-Z_][A-Z0-9_]{0,63}$`, never a name Saka sets itself.
    pub name: String,
    pub updated_at: String,
}

impl From<&saka_engine::SecretInfo> for SecretName {
    fn from(s: &saka_engine::SecretInfo) -> SecretName {
        SecretName {
            name: s.name.clone(),
            updated_at: time(s.updated_at),
        }
    }
}

/// The secrets of a project, ordered by name. At most 200, so there is no paging.
#[derive(Debug, Serialize, ToSchema)]
pub struct SecretList {
    pub items: Vec<SecretName>,
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
pub struct CheckConfigBody {
    /// HTTPS URL of the repo that holds the project folder; same rules as `POST /projects`.
    pub workspace_url: String,
    /// `.` for the repo root, or a relative folder path; same rules as `POST /projects`.
    pub project_path: String,
    /// `null` = the default branch of the workspace repo.
    #[serde(default)]
    pub base_branch: Option<String>,
    /// The `saka.toml` text to check, 1 to 65 536 bytes.
    pub content: String,
}

/// The workspace repo as the check resolved it.
#[derive(Debug, Serialize, ToSchema)]
pub struct CheckedWorkspace {
    pub integration_id: String,
    pub full_name: String,
    pub default_branch: String,
    /// The effective base branch: the requested one, or the default branch.
    pub base_branch: String,
}

/// A `[[repo]]` binding that resolved; same shape as `Project.repos[]` without ids.
#[derive(Debug, Serialize, ToSchema)]
pub struct CheckedRepo {
    pub name: String,
    pub integration_id: String,
    pub full_name: String,
    pub url: String,
    pub is_workspace: bool,
    pub is_default: bool,
    pub base_branch: String,
    pub path_scope: Vec<String>,
    /// `path_scope`, plus `<project_path>/**` for the workspace repo unless `project_path` is `.`.
    pub effective_path_scope: Vec<String>,
}

/// A problem that would refuse registration.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConfigProblem {
    /// `project_config_invalid | scm_integration_unknown | scm_repo_inaccessible`
    pub code: String,
    pub params: Value,
}

/// `saka.toml` at the tip of the workspace base branch.
#[derive(Debug, Serialize, ToSchema)]
pub struct RemoteConfig {
    /// Commit at the tip of the base branch.
    pub commit: String,
    /// `false` when the file does not exist at that commit.
    pub present: bool,
    /// `sha256:<hex>` of the remote file; `null` when it is missing or larger than 65 536 bytes.
    pub config_sha256: Option<String>,
    /// `config_sha256 == content_sha256`.
    pub matches: bool,
}

/// The project already registered for this workspace repo and folder.
#[derive(Debug, Serialize, ToSchema)]
pub struct CheckedProject {
    pub id: Uuid,
    pub name: String,
    pub config_commit: String,
    pub config_sha256: String,
}

/// Result of checking a `saka.toml` without registering it.
#[derive(Debug, Serialize, ToSchema)]
pub struct ConfigCheck {
    /// `true` when `problems` is empty.
    pub ok: bool,
    /// `sha256:<hex>` of `content`.
    pub content_sha256: String,
    /// `null` when the workspace host has no integration or the App cannot read the repo.
    pub workspace: Option<CheckedWorkspace>,
    /// Resolved bindings, default first, then by `name`.
    pub repos: Vec<CheckedRepo>,
    /// In order: content issues, the workspace, then repos in `[[repo]]` order.
    pub problems: Vec<ConfigProblem>,
    /// `null` when `workspace` is `null` or its base branch does not exist.
    pub remote: Option<RemoteConfig>,
    /// `null` when no project is registered for this workspace repo and folder.
    pub project: Option<CheckedProject>,
}

impl ConfigCheck {
    pub fn from_view(v: &saka_engine::ConfigCheckView) -> ConfigCheck {
        ConfigCheck {
            ok: v.ok,
            content_sha256: v.content_sha256.clone(),
            workspace: v.workspace.as_ref().map(|w| CheckedWorkspace {
                integration_id: w.integration_id.clone(),
                full_name: w.full_name.clone(),
                default_branch: w.default_branch.clone(),
                base_branch: w.base_branch.clone(),
            }),
            repos: v
                .repos
                .iter()
                .map(|r| CheckedRepo {
                    name: r.name.clone(),
                    integration_id: r.integration_id.clone(),
                    full_name: r.full_name.clone(),
                    url: r.url.clone(),
                    is_workspace: r.is_workspace,
                    is_default: r.is_default,
                    base_branch: r.base_branch.clone(),
                    path_scope: r.path_scope.clone(),
                    effective_path_scope: r.effective_path_scope.clone(),
                })
                .collect(),
            problems: v
                .problems
                .iter()
                .map(|p| ConfigProblem {
                    code: p.code.clone(),
                    params: p.params.clone(),
                })
                .collect(),
            remote: v.remote.as_ref().map(|r| RemoteConfig {
                commit: r.commit.clone(),
                present: r.present,
                config_sha256: r.config_sha256.clone(),
                matches: r.matches,
            }),
            project: v.project.as_ref().map(|p| CheckedProject {
                id: p.id.0,
                name: p.name.clone(),
                config_commit: p.config_commit.clone(),
                config_sha256: p.config_sha256.clone(),
            }),
        }
    }
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
pub struct ReturnBody {
    /// `dev`, `review`, `fix`, or `merge`: the stage the task re-enters. Empty means the stage it left when that is `dev` or `merge`, otherwise `dev`.
    #[serde(default)]
    pub stage: Option<String>,
    /// 1 to 500 characters.
    pub reason: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PrimaryActionBody {
    /// The blocker the operator saw; a blocker that is no longer the open one of the task is refused.
    pub blocker_id: Uuid,
    /// The kind of the primary action the caller was shown (`primary_action.kind`). When it differs from the current one, nothing runs and the answer is `409 blocker_action_changed`.
    #[serde(default)]
    pub action: Option<String>,
    pub reason: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSpecDraftBody {
    /// 1 to 8000 characters after trimming; stored as written.
    pub idea: String,
    /// The new spec file from the repo root, inside the project folder, ending in `.md`.
    pub path: String,
    /// Up to 16 distinct docs in the project folder the architect reads first.
    #[serde(default)]
    pub context_paths: Vec<String>,
    /// `en` (default) or `id`: the template and the language of the prose.
    #[serde(default)]
    pub locale: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetrySpecDraftBody {
    /// 1 to 500 characters.
    pub reason: String,
    /// 1 to 4000 characters, sent to the architect in the packet of the new attempt.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerBody {
    /// `allow | deny`
    pub decision: Decision,
    #[serde(default)]
    pub message: Option<String>,
    /// With `allow` only: also allow the same tool (and, for Bash, the same whole command) in this project from now on.
    #[serde(default)]
    pub remember: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SteerBody {
    /// 1 to 4000 characters, sent to the agent as a user message.
    pub text: String,
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyBody {}

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

/// Markdown files of the project folder at the tip of the base branch.
#[derive(Debug, Serialize, ToSchema)]
pub struct FileTree {
    /// Tip of the base branch the list was read at.
    pub commit: String,
    pub base_branch: String,
    pub project_path: String,
    /// Regular `.md`/`.markdown` files under the project folder, sorted by path bytes.
    pub files: Vec<FileEntry>,
    /// More than 5000 files exist; `files` holds the first 5000.
    pub truncated: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FileEntry {
    /// Path relative to the workspace repo root.
    pub path: String,
    /// Git blob sha of the file at the tip.
    pub blob: String,
    pub bytes: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FileContent {
    pub path: String,
    /// Tip of the base branch the file was read at.
    pub commit: String,
    /// Git blob sha of the file; also the strong `ETag`.
    pub blob: String,
    pub bytes: u64,
    /// The exact file content (a leading BOM is U+FEFF; line endings are kept as they are).
    pub content: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FileWrite {
    pub path: String,
    /// The new commit, or the tip when the content was already there (`committed: false`).
    pub commit: String,
    /// Git blob sha of the file; also the strong `ETag`.
    pub blob: String,
    pub bytes: u64,
    /// False when the content equals the file at the tip: no commit was made.
    pub committed: bool,
    /// The spec registered from this file in the same request, with its acceptance criteria.
    pub spec: Option<Spec>,
    /// Why the file was not registered as a spec, although it should have been.
    pub spec_problem: Option<SpecProblem>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SpecProblem {
    /// Same codes as registering a spec: `spec_slug_conflict`, `validation_failed`, `scm_unavailable`.
    pub code: String,
    pub params: Value,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteFileBody {
    /// The exact new file content, at most 1 MiB as UTF-8.
    pub content: String,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct FileQuery {
    /// Path of the file relative to the workspace repo root, inside the project folder.
    pub path: Option<String>,
}

impl FileTree {
    pub fn from_view(v: &DocTreeView) -> FileTree {
        FileTree {
            commit: v.commit.clone(),
            base_branch: v.base_branch.clone(),
            project_path: v.project_path.clone(),
            files: v
                .files
                .iter()
                .map(|f| FileEntry {
                    path: f.path.clone(),
                    blob: f.blob.clone(),
                    bytes: f.bytes,
                })
                .collect(),
            truncated: v.truncated,
        }
    }
}

impl FileContent {
    pub fn from_view(v: &DocContentView) -> FileContent {
        FileContent {
            path: v.path.clone(),
            commit: v.commit.clone(),
            blob: v.blob.clone(),
            bytes: v.bytes,
            content: v.content.clone(),
        }
    }
}

impl FileWrite {
    pub fn from_view(v: &DocWriteView) -> FileWrite {
        FileWrite {
            path: v.path.clone(),
            commit: v.commit.clone(),
            blob: v.blob.clone(),
            bytes: v.bytes,
            committed: v.committed,
            spec: v.spec.as_ref().map(|s| Spec::from_view(s, true)),
            spec_problem: v.spec_problem.as_ref().map(|p| SpecProblem {
                code: p.code.clone(),
                params: p.params.clone(),
            }),
        }
    }
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
pub struct SpecDraftQuery {
    /// `active` (default: queued, preparing, drafting, committing, failed), `finished` (succeeded, cancelled), or `all`.
    pub state: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SpecListQuery {
    /// One or more of `draft, ready, approved, shipped, withdrawn`, comma-separated.
    pub state: Option<String>,
    /// Only the spec registered at exactly this path (at most 1024 bytes).
    pub path: Option<String>,
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
    /// `policy` (answered by a rule) or `human` (answered by the operator).
    pub source: Option<String>,
    /// Exclusive with `spec_draft_id`.
    pub work_item_id: Option<String>,
    pub spec_draft_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SessionsQuery {
    /// Exactly one of `work_item_id` and `spec_draft_id` is required.
    pub work_item_id: Option<String>,
    pub spec_draft_id: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TimelineQuery {
    /// Return the activities right after this offset. At most one of `after`, `before`, and `tail`.
    pub after: Option<u64>,
    /// Return the activities right before this offset, still in ascending order.
    pub before: Option<u64>,
    /// `true` returns the last `limit` activities, in ascending order.
    pub tail: Option<String>,
    /// 1 to 500, default 200.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct UsageQuery {
    /// One of `work_item_id`, `spec_draft_id`, and `session_id` is required; `work_item_id` and `spec_draft_id` are exclusive.
    pub work_item_id: Option<String>,
    pub spec_draft_id: Option<String>,
    pub session_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventsQuery {
    /// Start after this `seq` (`Last-Event-ID` wins when both are given).
    pub after: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WorkPacket {
    pub work_item_id: Uuid,
    /// Revision the task is pinned to.
    pub revision_id: Uuid,
    pub source_commit: String,
    /// The `ready` environment whose ports, toolchains, and session secret names are in the packet.
    pub environment_id: Uuid,
    /// UTF-8 length of `text`.
    pub bytes: u64,
    /// `sha256:<hex>` of `text`.
    pub sha256: String,
    pub documents: Vec<WorkPacketDocument>,
    /// The first turn exactly as the session receives it, without a CI retry paragraph.
    pub text: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WorkPacketDocument {
    pub index: u32,
    /// `spec` for document 1, `context` for the rest.
    pub kind: String,
    pub path: String,
    /// `link` or `always`; `null` for the spec.
    pub origin: Option<String>,
    pub bytes: u64,
    pub content_hash: String,
}

impl WorkPacket {
    pub fn from_packet(
        task: &saka_core::model::Task,
        packet: &saka_engine::packet::Packet,
    ) -> WorkPacket {
        WorkPacket {
            work_item_id: task.item.id.0,
            revision_id: task.item.spec_revision_id.map(|r| r.0).unwrap_or_default(),
            source_commit: packet.source_commit.clone(),
            environment_id: task.ready_environment().map(|e| e.id.0).unwrap_or_default(),
            bytes: packet.text.len() as u64,
            sha256: packet.sha256.clone(),
            documents: packet
                .documents
                .iter()
                .map(|d| WorkPacketDocument {
                    index: d.index,
                    kind: if d.origin.is_none() {
                        "spec"
                    } else {
                        "context"
                    }
                    .to_string(),
                    path: d.path.clone(),
                    origin: d.origin.map(|o| o.as_str().to_string()),
                    bytes: d.bytes,
                    content_hash: d.content_hash.clone(),
                })
                .collect(),
            text: packet.text.clone(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookAccepted {
    pub delivery_id: String,
    /// `refreshed`, `ignored`, or `duplicate`.
    pub outcome: String,
    /// Change requests refreshed by this delivery; for `duplicate`, the count from the first receipt.
    pub change_requests: u32,
}

impl WebhookAccepted {
    pub fn from_accepted(a: &saka_engine::webhooks::AcceptedWebhook) -> WebhookAccepted {
        WebhookAccepted {
            delivery_id: a.delivery_id.clone(),
            outcome: a.outcome.as_str().to_string(),
            change_requests: a.change_requests,
        }
    }
}

fn gates_at_head(t: &saka_core::model::Task) -> Vec<Gate> {
    let Some(cr) = &t.change_request else {
        return Vec::new();
    };
    let target = t
        .stage_run
        .as_ref()
        .filter(|r| r.stage == saka_core::vocab::Stage::Sync)
        .and_then(|r| r.head_sha.as_deref());
    let head = target.unwrap_or(&cr.head_sha);
    let policy = saka_core::gate::GatePolicy::from_recipe(
        t.environment.as_ref().and_then(|e| e.recipe.as_ref()),
    );
    [
        saka_core::vocab::GateSource::Scm,
        saka_core::vocab::GateSource::Saka,
    ]
    .into_iter()
    .filter_map(|source| t.gate_for(source, head))
    .map(|g| Gate {
        source: g.source.as_str().to_string(),
        required: policy.required(g.source, &g.checks),
        head_sha: g.head_sha.clone(),
        outcome: g.outcome.as_str().to_string(),
        checks: g.checks.iter().map(GateCheck::from).collect(),
        observed_at: time(g.observed_at),
    })
    .collect()
}

/// Request body that sets a project's authorship mode.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetAuthorshipBody {
    /// `bot` (Saka authors and opens pull requests), `user` (the linked account of every
    /// integration of the project authors commits and opens pull requests) or `user_pat` (the
    /// owner of the project's user token).
    pub mode: String,
}

/// Request body that stores a project's user token. The value is write-only.
// No `Debug`: the value must not be printable by accident.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetUserTokenBody {
    /// A GitHub fine-grained personal access token (`github_pat_…`), 40 to 255 bytes of
    /// printable ASCII without spaces. It needs read access to every repo of the project and
    /// write access to their pull requests.
    pub value: String,
}

/// Who authors a project's agent changes. Tokens are never part of it.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectAuthorship {
    pub project_id: Uuid,
    /// `bot | user | user_pat`; `bot` for a project whose mode was never set.
    pub mode: String,
    /// When the mode last changed; `null` for a project whose mode was never set.
    pub mode_updated_at: Option<String>,
    /// The identity the next session commits as, for the project's default repo; `null` when
    /// the mode's credential is not usable.
    pub author: Option<AuthorshipAuthor>,
    /// The integrations of the repos bound to the project, each with its linked account state.
    pub integrations: Vec<AuthorshipIntegration>,
    /// The project's user token; `null` without one. Its value is never returned.
    pub user_token: Option<ProjectUserToken>,
}

/// A commit author identity.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuthorshipAuthor {
    /// `saka | user_link | user_token`
    pub source: String,
    pub name: String,
    pub email: String,
    /// The account login; `null` for `saka`.
    pub login: Option<String>,
}

/// An integration of a project and the state of its linked account.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuthorshipIntegration {
    /// `[[scm]].id` from the operator config.
    pub integration_id: String,
    /// `active | needs_relink | missing`
    pub user_link_state: String,
}

/// A stored user token, without its value.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProjectUserToken {
    pub integration_id: String,
    pub login: String,
    pub provider_user_id: u64,
    /// `null` for a token without an expiry.
    pub expires_at: Option<String>,
    pub set_at: String,
    /// `active | invalid`
    pub state: String,
    /// Only with `invalid`: `expired | rejected`.
    pub invalid_cause: Option<String>,
}

impl ProjectAuthorship {
    pub fn from_view(v: &saka_engine::AuthorshipView) -> ProjectAuthorship {
        ProjectAuthorship {
            project_id: v.project_id.0,
            mode: v.mode.as_str().to_string(),
            mode_updated_at: opt_time(v.mode_updated_at),
            author: v.author.as_ref().map(|a| AuthorshipAuthor {
                source: a.source.as_str().to_string(),
                name: a.identity.name.clone(),
                email: a.identity.email.clone(),
                login: a.login.clone(),
            }),
            integrations: v
                .integrations
                .iter()
                .map(|i| AuthorshipIntegration {
                    integration_id: i.integration_id.clone(),
                    user_link_state: match i.problem {
                        None => "active",
                        Some(saka_core::vocab::UserLinkCause::Missing) => "missing",
                        Some(_) => "needs_relink",
                    }
                    .to_string(),
                })
                .collect(),
            user_token: v.user_token.as_ref().map(|u| ProjectUserToken {
                integration_id: u.token.integration_id.clone(),
                login: u.token.identity.login.clone(),
                provider_user_id: u.token.identity.provider_user_id,
                expires_at: opt_time(u.token.expires_at),
                set_at: time(u.token.set_at),
                state: match u.problem {
                    None => saka_core::vocab::UserTokenState::Active,
                    Some(_) => saka_core::vocab::UserTokenState::Invalid,
                }
                .as_str()
                .to_string(),
                invalid_cause: u.problem.map(|c| c.as_str().to_string()),
            }),
        }
    }
}

/// Request body that starts linking an account; no fields.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StartUserLinkBody {}

/// The SCM account linked to an integration, so that changes can be authored as its user.
/// Tokens are never part of it.
#[derive(Debug, Serialize, ToSchema)]
pub struct UserLink {
    pub id: Uuid,
    /// `[[scm]].id` from the operator config.
    pub integration_id: String,
    /// `active | needs_relink`
    pub state: String,
    /// Only with `needs_relink`: `refresh_failed | revoked | expired`.
    pub relink_cause: Option<String>,
    /// The account id at the provider (`GET /user` `id`).
    pub provider_user_id: u64,
    pub login: String,
    /// The profile name, or `null` when the profile has none; commits then use `login`.
    pub name: Option<String>,
    /// The noreply address `<id>+<login>@users.noreply.<host of web_url>`, used as the commit
    /// author email.
    pub email: String,
    pub linked_at: String,
    /// The last successful token refresh.
    pub refreshed_at: Option<String>,
    /// When the user access token expires; it is refreshed before use.
    pub access_expires_at: String,
    /// When the refresh token expires; after it the account has to be linked again.
    pub refresh_expires_at: String,
}

impl From<&saka_core::authorship::UserLink> for UserLink {
    fn from(l: &saka_core::authorship::UserLink) -> UserLink {
        UserLink {
            id: l.id.0,
            integration_id: l.integration_id.clone(),
            state: l.state.as_str().to_string(),
            relink_cause: l.relink_cause.map(|c| c.as_str().to_string()),
            provider_user_id: l.provider_user_id,
            login: l.login.clone(),
            name: l.display_name.clone(),
            email: l.email.clone(),
            linked_at: time(l.linked_at),
            refreshed_at: opt_time(l.refreshed_at),
            access_expires_at: time(l.access_expires_at),
            refresh_expires_at: time(l.refresh_expires_at),
        }
    }
}

/// One device-flow attempt to link an account. The user opens `verification_uri` and enters
/// `user_code`; the server polls the provider every `interval_secs` until the attempt ends.
#[derive(Debug, Serialize, ToSchema)]
pub struct UserLinkAuthorization {
    pub id: Uuid,
    /// `[[scm]].id` from the operator config.
    pub integration_id: String,
    /// `pending | completed | expired | denied | failed`
    pub state: String,
    /// The code the user enters at `verification_uri`, for example `WDJB-MJHT`.
    pub user_code: String,
    pub verification_uri: String,
    /// Seconds between polls; raised when the provider asks to slow down.
    pub interval_secs: u32,
    /// After this time a pending attempt ends as `expired`.
    pub expires_at: String,
    /// Only with `failed`. `code`: `device_flow_disabled | incorrect_client_credentials |
    /// incorrect_device_code | unsupported_grant_type | bad_verification_code |
    /// token_not_expiring | identity_unavailable`.
    pub failure: Option<Failure>,
    /// The linked account once `completed`, otherwise `null`.
    pub link: Option<UserLink>,
}

impl UserLinkAuthorization {
    pub fn from_view(v: &saka_engine::AuthorizationView) -> UserLinkAuthorization {
        let a = &v.authorization;
        UserLinkAuthorization {
            id: a.id.0,
            integration_id: a.integration_id.clone(),
            state: a.state.as_str().to_string(),
            user_code: a.user_code.clone(),
            verification_uri: a.verification_uri.clone(),
            interval_secs: a.interval_secs,
            expires_at: time(a.expires_at),
            failure: a.failure.as_ref().map(|f| Failure {
                code: f.code.clone(),
                params: f.params.clone(),
            }),
            link: v.link.as_ref().map(UserLink::from),
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DiagnosticsQuery {
    /// Only this project's `project`, `project_repo`, and `repo` checks; instance, agent account, and integration checks always run.
    pub project_id: Option<String>,
    /// A check id from the catalog, for example `agent.login`; repeat for more. Without it, every check runs.
    pub check: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiagnosticSubject {
    /// `instance`, `agent_account`, `integration`, `repo`, `project`, or `project_repo`.
    pub kind: String,
    /// For example `instance`, `claude:default`, or an integration, repo, or project id.
    pub id: String,
    /// For display, for example `claude:default` or `acme/shop`.
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiagnosticFix {
    pub code: String,
    /// `command` holds the command that fixes it, when there is one.
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiagnosticCheck {
    /// Stable dotted id, for example `agent.login`.
    pub id: String,
    pub subject: DiagnosticSubject,
    /// `pass`, `warn`, `fail`, or `skip`.
    pub status: String,
    /// What was found; the client turns it and `params` into text. Never holds a token, key, secret, or account email.
    pub code: String,
    pub params: Value,
    /// `null` for `pass` and `skip`.
    pub fix: Option<DiagnosticFix>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
pub struct DiagnosticSummary {
    pub pass: u32,
    pub warn: u32,
    pub fail: u32,
    pub skip: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiagnosticReport {
    pub generated_at: String,
    pub server_version: String,
    /// `online` from the server; `saka doctor` writes `offline` when it ran the local checks itself.
    pub mode: String,
    pub summary: DiagnosticSummary,
    /// In catalog order, then by `subject.label`.
    pub checks: Vec<DiagnosticCheck>,
}

impl From<&saka_core::diagnostics::CheckResult> for DiagnosticCheck {
    fn from(c: &saka_core::diagnostics::CheckResult) -> DiagnosticCheck {
        DiagnosticCheck {
            id: c.id.as_str().into(),
            subject: DiagnosticSubject {
                kind: c.subject.kind.as_str().into(),
                id: c.subject.id.clone(),
                label: c.subject.label.clone(),
            },
            status: c.finding.status.as_str().into(),
            code: c.finding.code.clone(),
            params: c.finding.params.clone(),
            fix: c.finding.fix.as_ref().map(|f| DiagnosticFix {
                code: f.code.clone(),
                params: f.params.clone(),
            }),
            duration_ms: c.duration_ms,
        }
    }
}

impl From<&saka_core::diagnostics::Report> for DiagnosticReport {
    fn from(r: &saka_core::diagnostics::Report) -> DiagnosticReport {
        DiagnosticReport {
            generated_at: time(r.generated_at),
            server_version: r.server_version.clone(),
            mode: r.mode.as_str().into(),
            summary: DiagnosticSummary {
                pass: r.summary.pass,
                warn: r.summary.warn,
                fail: r.summary.fail,
                skip: r.summary.skip,
            },
            checks: r.checks.iter().map(DiagnosticCheck::from).collect(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HookTarget {
    /// The shell command of a `run` or `run_host` hook.
    pub command: Option<String>,
    /// The working directory of a `run_host` hook.
    pub cwd: Option<String>,
    /// The URL of a `webhook` hook without its query or fragment.
    pub url: Option<String>,
    /// The project secret that signs a `webhook` hook.
    pub secret: Option<String>,
    /// The project secrets a `run` hook receives.
    pub secrets: Vec<String>,
    /// The channel of a `notify` hook.
    pub channel: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Hook {
    pub name: String,
    /// `change_opened`, `review_done`, `merged`, `task_done`, `spec_shipped`, `blocked`, or `inbox_item_opened`.
    pub on: String,
    /// `after` or `gate`.
    pub mode: String,
    /// `run`, `run_host`, `webhook`, or `notify`.
    pub action: String,
    pub repos: Vec<String>,
    pub branches: Vec<String>,
    pub codes: Vec<String>,
    pub timeout_secs: u64,
    pub target: HookTarget,
}

impl From<&saka_core::hook::Hook> for Hook {
    fn from(h: &saka_core::hook::Hook) -> Hook {
        use saka_core::hook::HookAction;
        let mut target = HookTarget {
            command: None,
            cwd: None,
            url: None,
            secret: None,
            secrets: Vec::new(),
            channel: None,
        };
        match &h.action {
            HookAction::Run { command, secrets } => {
                target.command = Some(command.clone());
                target.secrets = secrets.clone();
            }
            HookAction::RunHost { command, cwd } => {
                target.command = Some(command.clone());
                target.cwd = cwd.clone();
            }
            HookAction::Webhook { url, secret } => {
                target.url = Some(saka_core::hook::display_url(url));
                target.secret = Some(secret.clone());
            }
            HookAction::Notify { channel } => target.channel = Some(channel.clone()),
        }
        Hook {
            name: h.name.clone(),
            on: h.on.as_str().into(),
            mode: h.mode.as_str().into(),
            action: h.action.kind().as_str().into(),
            repos: h.filter.repos.clone(),
            branches: h.filter.branches.clone(),
            codes: h.filter.codes.iter().map(|c| c.as_str().into()).collect(),
            timeout_secs: h.timeout_ms / 1000,
            target,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HookList {
    pub items: Vec<Hook>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HookRunError {
    pub code: String,
    pub params: Value,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct HookRun {
    pub id: Uuid,
    pub project_id: Uuid,
    pub hook: String,
    /// `run`, `run_host`, `webhook`, or `notify`.
    pub action: String,
    /// `after` or `gate`.
    pub mode: String,
    /// The event that triggered the run.
    pub trigger: String,
    /// `pending`, `succeeded`, or `failed`.
    pub state: String,
    pub work_item_id: Option<Uuid>,
    pub task_key: Option<String>,
    pub spec_id: Option<Uuid>,
    pub repo_id: Option<Uuid>,
    pub sha: Option<String>,
    /// What triggered the run: event, project, repo, task, spec, change, branch, sha, and blocker.
    pub context: Value,
    /// The run this one repeats.
    pub rerun_of: Option<Uuid>,
    pub attempts: u32,
    /// `exit_code`, `timed_out`, `http_status`, `log`, `output_tail`, and `skipped`; `null` while pending.
    pub result: Option<Value>,
    pub error: Option<HookRunError>,
    pub queued_at: String,
    pub finished_at: Option<String>,
    pub duration_ms: Option<i64>,
}

impl From<&saka_core::hook::HookRun> for HookRun {
    fn from(r: &saka_core::hook::HookRun) -> HookRun {
        HookRun {
            id: r.id.0,
            project_id: r.project_id.0,
            hook: r.hook.clone(),
            action: r.action.as_str().into(),
            mode: r.mode.as_str().into(),
            trigger: r.trigger.as_str().into(),
            state: r.state.as_str().into(),
            work_item_id: r.work_item_id.map(|x| x.0),
            task_key: r.context.task.as_ref().map(|t| t.key.clone()),
            spec_id: r.spec_id.map(|x| x.0),
            repo_id: r.repo_id.map(|x| x.0),
            sha: r.sha.clone(),
            context: serde_json::to_value(&r.context).unwrap_or(Value::Null),
            rerun_of: r.rerun_of.map(|x| x.0),
            attempts: r.attempts,
            result: r.result.clone(),
            error: r.error.as_ref().map(|e| HookRunError {
                code: e.code.clone(),
                params: e.params.clone(),
            }),
            queued_at: time(r.queued_at),
            finished_at: opt_time(r.finished_at),
            duration_ms: r.duration_ms,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HookRunQuery {
    pub project_id: Option<String>,
    pub hook: Option<String>,
    /// `pending`, `succeeded`, or `failed`.
    pub state: Option<String>,
    pub work_item_id: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InboxTitle {
    /// The task the item is about, when it is about one.
    pub task_key: Option<String>,
    /// The stage the task is at.
    pub stage: Option<String>,
    pub spec_slug: Option<String>,
    pub spec_title: Option<String>,
    /// Where the spec lives in the docs, so a link can open it in the editor.
    pub spec_path: Option<String>,
    /// The path of the spec draft whose session asked, for a permission that belongs to a draft.
    pub draft_path: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InboxAction {
    /// A blocker action, or `answer_permission`, `approve_spec`, `start_step`, `merge`, or `answer_decision`.
    pub kind: String,
    /// What the existing endpoint for the action needs: ids, a revision, or a task version.
    pub params: Value,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InboxItem {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_name: String,
    /// `decision`, `approval`, `permission`, `blocker`, or `spec_unclear`.
    pub kind: String,
    /// `spec`, `step_start`, `merge`, `permission`, `blocker`, or `decision`.
    pub subject: String,
    /// `open`, `answered`, `resolved`, or `dismissed`.
    pub state: String,
    pub work_item_id: Option<Uuid>,
    pub spec_id: Option<Uuid>,
    pub title: InboxTitle,
    /// What the item is about. Never holds a secret value.
    pub params: Value,
    /// The one primary action. It points at an endpoint that already exists; the inbox has none of its own.
    pub action: InboxAction,
    /// Only a spec approval can be dismissed.
    pub dismissible: bool,
    pub opened_at: String,
    pub closed_at: Option<String>,
}

impl From<&saka_engine::InboxItem> for InboxItem {
    fn from(i: &saka_engine::InboxItem) -> InboxItem {
        InboxItem {
            id: i.id.0,
            project_id: i.project_id.0,
            project_name: i.project_name.clone(),
            kind: i.kind.as_str().into(),
            subject: i.subject.as_str().into(),
            state: i.state.as_str().into(),
            work_item_id: i.work_item_id.map(|x| x.0),
            spec_id: i.spec_id.map(|x| x.0),
            title: InboxTitle {
                task_key: i.task_key.clone(),
                stage: i.stage.clone(),
                spec_slug: i.spec_slug.clone(),
                spec_title: i.spec_title.clone(),
                spec_path: i.spec_path.clone(),
                draft_path: i.draft_path.clone(),
            },
            params: i.params.clone(),
            action: InboxAction {
                kind: i.action.kind.as_str().into(),
                params: i.action.params.clone(),
            },
            dismissible: i.dismissible(),
            opened_at: time(i.opened_at),
            closed_at: opt_time(i.closed_at),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InboxKinds {
    pub decision: i64,
    pub approval: i64,
    pub permission: i64,
    pub blocker: i64,
    pub spec_unclear: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InboxSummary {
    /// Items waiting for the operator.
    pub open: i64,
    pub by_kind: InboxKinds,
    /// Tasks that are in a stage now.
    pub running: i64,
    /// Changes merged since midnight UTC.
    pub merged_today: i64,
}

impl From<&saka_engine::InboxSummary> for InboxSummary {
    fn from(s: &saka_engine::InboxSummary) -> InboxSummary {
        use saka_core::vocab::InboxKind as K;
        let count = |k: K| s.by_kind.get(&k).copied().unwrap_or(0);
        InboxSummary {
            open: s.open,
            by_kind: InboxKinds {
                decision: count(K::Decision),
                approval: count(K::Approval),
                permission: count(K::Permission),
                blocker: count(K::Blocker),
                spec_unclear: count(K::SpecUnclear),
            },
            running: s.running,
            merged_today: s.merged_today,
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct InboxQuery {
    pub project_id: Option<String>,
    /// `decision`, `approval`, `permission`, `blocker`, or `spec_unclear`.
    pub kind: Option<String>,
    /// `open` (default) or `closed`.
    pub state: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

/// What a pairing code is for. `setup` lets the first passkey be registered and is only issued to the local CLI; `passkey` registers another passkey; `token` pairs a client that holds a device token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PairingPurpose {
    Setup,
    Passkey,
    Token,
}

impl From<PairingPurpose> for saka_core::vocab::PairingPurpose {
    fn from(p: PairingPurpose) -> saka_core::vocab::PairingPurpose {
        match p {
            PairingPurpose::Setup => saka_core::vocab::PairingPurpose::Setup,
            PairingPurpose::Passkey => saka_core::vocab::PairingPurpose::Passkey,
            PairingPurpose::Token => saka_core::vocab::PairingPurpose::Token,
        }
    }
}

impl From<saka_core::vocab::PairingPurpose> for PairingPurpose {
    fn from(p: saka_core::vocab::PairingPurpose) -> PairingPurpose {
        match p {
            saka_core::vocab::PairingPurpose::Setup => PairingPurpose::Setup,
            saka_core::vocab::PairingPurpose::Passkey => PairingPurpose::Passkey,
            saka_core::vocab::PairingPurpose::Token => PairingPurpose::Token,
        }
    }
}

/// Whether the server asks for credentials and who the caller is.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuthStatus {
    pub enabled: bool,
    /// No passkey is registered yet: the first one needs a `setup` code from the host.
    pub setup_required: bool,
    pub authenticated: bool,
    /// `operator:passkey:<id>` or `operator:device:<id>`; `null` when not authenticated.
    pub actor: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PairingBody {
    pub purpose: PairingPurpose,
}

/// A pairing code. Only the response that creates it carries `code`; a replay of the same request does not.
#[derive(Debug, Serialize, ToSchema)]
pub struct PairingCreated {
    pub id: Uuid,
    pub purpose: PairingPurpose,
    pub expires_at: String,
    /// Ten characters of Crockford base32, valid once until `expires_at`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl PairingCreated {
    pub fn from_core(p: &saka_core::identity::Pairing) -> PairingCreated {
        PairingCreated {
            id: p.id.0,
            purpose: p.purpose.into(),
            expires_at: time(p.expires_at),
            code: None,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PairBody {
    /// A `token` pairing code.
    pub code: String,
    /// 1 to 100 characters.
    pub name: String,
}

/// A device token. It is shown once and cannot be read again.
#[derive(Debug, Serialize, ToSchema)]
pub struct PairedDevice {
    pub device_id: Uuid,
    pub token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    LocalCli,
    Client,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Device {
    pub id: Uuid,
    pub name: String,
    /// `local_cli` is the token of the CLI on the host; it cannot be revoked.
    pub kind: DeviceKind,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
}

impl From<&saka_core::identity::Device> for Device {
    fn from(d: &saka_core::identity::Device) -> Device {
        Device {
            id: d.id.0,
            name: d.name.clone(),
            kind: match d.kind {
                saka_core::vocab::DeviceKind::LocalCli => DeviceKind::LocalCli,
                saka_core::vocab::DeviceKind::Client => DeviceKind::Client,
            },
            created_at: time(d.created_at),
            last_used_at: opt_time(d.last_used_at),
            revoked_at: opt_time(d.revoked_at),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PasskeyCredential {
    pub id: Uuid,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

impl From<&saka_core::identity::Credential> for PasskeyCredential {
    fn from(c: &saka_core::identity::Credential) -> PasskeyCredential {
        PasskeyCredential {
            id: c.id.0,
            name: c.name.clone(),
            created_at: time(c.created_at),
            last_used_at: opt_time(c.last_used_at),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuthDevices {
    pub devices: Vec<Device>,
    pub credentials: Vec<PasskeyCredential>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PasskeyRegisterStartBody {
    /// A `setup` or `passkey` pairing code. Not needed with a signed-in session.
    pub code: Option<String>,
    /// A name for the passkey, 1 to 100 characters.
    pub name: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginStartBody {}

/// A WebAuthn ceremony the server holds in memory for five minutes. It can be finished once.
#[derive(Debug, Serialize, ToSchema)]
pub struct CeremonyStarted {
    pub ceremony_id: String,
    /// The WebAuthn options for `navigator.credentials.create` or `.get`, in the JSON form of the WebAuthn spec.
    pub options: Value,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CeremonyFinishBody {
    pub ceremony_id: String,
    /// The credential the browser returned, in the JSON form of the WebAuthn spec.
    pub credential: Value,
}
