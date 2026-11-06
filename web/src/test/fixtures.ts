import type {
  PermissionRequest,
  ProjectItem,
  SpecItem,
  WorkItem,
  WorkItemDetail,
} from "@/features/delivery/queries";

export const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
export const SPEC_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334466";

export const project: ProjectItem = {
  id: PROJECT_ID,
  name: "tasks",
  workspace: {
    repo_id: "0193f0c4-1b2a-7c3d-9e8f-001122334400",
    full_name: "acme/tasks",
    project_path: "docs",
    base_branch: "main",
  },
  repos: [],
  autonomy: "standard",
  authorship_mode: "bot",
  workflow: {
    preset: "full_loop",
    spec: true,
    change_request: null,
    stages: [
      { stage: "dev", trigger: "auto" },
      { stage: "review", trigger: "auto" },
      { stage: "fix", trigger: "auto" },
      { stage: "merge", trigger: "manual" },
    ],
    fix_rounds: 2,
  },
  config_commit: "9f1c".padEnd(40, "0"),
  config_sha256: `sha256:${"4b8e".padEnd(64, "0")}`,
  config_error: null,
  warnings: [],
  created_at: "2026-10-31T10:00:00.000Z",
  updated_at: "2026-10-31T10:00:00.000Z",
};

export const spec: SpecItem = {
  id: SPEC_ID,
  project_id: PROJECT_ID,
  path: "docs/specs/tasks-done.md",
  slug: "tasks-done",
  title: "Mark tasks as done",
  state: "approved",
  origin: "manual",
  priority: 0,
  depends_on: [],
  revision: {
    id: "0193f0c4-1b2a-7c3d-9e8f-001122334477",
    source_commit: "9f1c".padEnd(40, "0"),
    content_hash: "sha256:" + "4b8e".padEnd(64, "0"),
    parser_version: 4,
    context_hash: "sha256:" + "7d1a".padEnd(64, "0"),
    context: [],
    created_at: "2026-10-31T10:00:00.000Z",
  },
  approved_revision: {
    id: "0193f0c4-1b2a-7c3d-9e8f-001122334477",
    source_commit: "9f1c".padEnd(40, "0"),
    content_hash: "sha256:" + "4b8e".padEnd(64, "0"),
  },
  revision_plan: null,
  lint: { ok: true, issues: [] },
  steps: [],
  decisions: [],
  approved_at: "2026-10-31T10:01:00.000Z",
  shipped_at: null,
  created_at: "2026-10-31T10:00:00.000Z",
  updated_at: "2026-10-31T10:01:00.000Z",
};

export function task(step: number, extra: Partial<WorkItem>): WorkItem {
  return {
    id: `0193f0c4-1b2a-7c3d-9e8f-00112233450${step}`,
    project_id: PROJECT_ID,
    origin: "spec_step",
    spec_id: SPEC_ID,
    spec_revision_id: spec.revision.id,
    step_id: `S${step}`,
    step_index: step,
    task_key: `tasks-done/s${step}`,
    work: `Work of step ${step}`,
    repo_id: project.workspace.repo_id,
    repo_name: "app",
    branch: `saka/tasks-done/s${step}`,
    state: "queued",
    stage: null,
    stage_phase: null,
    priority: 0,
    waiting_on: [],
    merge_queue: null,
    change_request: null,
    gates: [],
    blocker: null,
    session: null,
    pending_permission_count: 0,
    available_actions: ["cancel"],
    version: 1,
    created_at: "2026-10-31T10:01:00.000Z",
    updated_at: "2026-10-31T10:01:00.000Z",
    admitted_at: null,
    done_at: null,
    ...extra,
  };
}

export const blockedTask = task(1, {
  state: "blocked",
  change_request: {
    id: "0193f0c4-1b2a-7c3d-9e8f-001122334499",
    number: 12,
    url: "https://github.com/acme/tasks/pull/12",
    state: "open",
    head_sha: "a1b2".padEnd(40, "0"),
    base_branch: "main",
    merge_commit_sha: null,
    merged_at: null,
  },
  blocker: {
    id: "0193f0c4-1b2a-7c3d-9e8f-001122334488",
    code: "ci_red",
    params: { checks: ["test", "lint"], head_sha: "a1b2".padEnd(40, "0") },
    opened_at: "2026-10-31T10:05:00.000Z",
    stage: "merge",
    primary_action: { kind: "retry", params: {} },
  },
  available_actions: ["retry", "cancel"],
});

export const queuedTask = task(2, {});

export const outsideTask = task(4, {
  origin: "external_change",
  spec_id: null,
  spec_revision_id: null,
  step_id: null,
  step_index: null,
  task_key: "app#7",
  work: "Add retries to the poller",
  branch: "feature/retries",
  state: "in_stage",
  stage: "review",
  stage_phase: "awaiting_trigger",
  change_request: {
    id: "0193f0c4-1b2a-7c3d-9e8f-001122334477",
    number: 7,
    url: "https://github.com/acme/tasks/pull/7",
    state: "open",
    head_sha: "c0ffee".padEnd(40, "0"),
    base_branch: "main",
    merge_commit_sha: null,
    merged_at: null,
  },
  external: {
    title: "Add retries to the poller",
    author: "alice",
    from_fork: true,
    draft: false,
    spec: null,
    cause: "auto",
    actor: null,
  },
  available_actions: ["cancel", "review"],
});

export const supersededTask = task(3, {
  state: "superseded",
  available_actions: [],
});

export function detail(
  item: WorkItem,
  extra: Partial<WorkItemDetail> = {},
): WorkItemDetail {
  return { ...item, stage_runs: [], reviews: [], ...extra };
}

export function specWithSteps(steps: [string, string[]][]): SpecItem {
  return {
    ...spec,
    steps: steps.map(([id, after], i) => ({
      id,
      index: i + 1,
      work: `Work of step ${id}`,
      ac: [],
      commands: [],
      after,
      work_item_id: null,
    })),
  };
}

export function permission(
  n: number,
  extra: Partial<PermissionRequest> = {},
): PermissionRequest {
  return {
    id: `0193f0c4-1b2a-7c3d-9e8f-0011223344a${n}`,
    project_id: PROJECT_ID,
    owner: {
      kind: "stage_run",
      work_item_id: task(1, {}).id,
      stage_run_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344c1",
    },
    session_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344f1",
    tool: "Bash",
    summary: "npm install left-pad",
    input: { command: "npm install left-pad" },
    state: "pending",
    requested_at: `2026-10-31T10:0${n}:00.000Z`,
    answer_source: null,
    answered_at: null,
    message: null,
    ...extra,
  };
}
