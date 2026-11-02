import type {
  ProjectItem,
  SpecItem,
  WorkItem,
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
  workflow_preset: "full_loop",
  config_commit: "9f1c".padEnd(40, "0"),
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
    parser_version: 3,
    context_hash: "sha256:" + "7d1a".padEnd(64, "0"),
    context: [],
    created_at: "2026-10-31T10:00:00.000Z",
  },
  approved_revision_id: "0193f0c4-1b2a-7c3d-9e8f-001122334477",
  revision_plan: null,
  lint: { ok: true, issues: [] },
  steps: [],
  approved_at: "2026-10-31T10:01:00.000Z",
  shipped_at: null,
  created_at: "2026-10-31T10:00:00.000Z",
  updated_at: "2026-10-31T10:01:00.000Z",
};

function task(step: number, extra: Partial<WorkItem>): WorkItem {
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
    branch: `saka/tasks-done/s${step}`,
    state: "queued",
    stage: null,
    stage_phase: null,
    priority: 0,
    waiting_on: [],
    change_request: null,
    gate: null,
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
  },
  blocker: {
    id: "0193f0c4-1b2a-7c3d-9e8f-001122334488",
    code: "ci_red",
    params: { checks: ["test", "lint"], head_sha: "a1b2".padEnd(40, "0") },
    opened_at: "2026-10-31T10:05:00.000Z",
    stage: "merge",
  },
  available_actions: ["retry", "cancel"],
});

export const queuedTask = task(2, {});

export const supersededTask = task(3, {
  state: "superseded",
  available_actions: [],
});
