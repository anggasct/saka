import type { Hook, HookRun } from "@/features/settings/queries";

import { PROJECT_ID } from "./fixtures";

export const HOOKS = `/api/v1/projects/${PROJECT_ID}/hooks`;
export const HOOK_RUNS = "/api/v1/hook-runs";
export const TASK_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344aa";

export const deploy: Hook = {
  name: "deploy-staging",
  on: "merged",
  mode: "after",
  action: "run",
  repos: ["app"],
  branches: ["main"],
  codes: [],
  timeout_secs: 300,
  target: {
    command: "./scripts/deploy.sh staging",
    cwd: null,
    url: null,
    secret: null,
    secrets: ["DEPLOY_TOKEN"],
    channel: null,
  },
};

export const announce: Hook = {
  name: "announce",
  on: "task_done",
  mode: "after",
  action: "webhook",
  repos: [],
  branches: [],
  codes: [],
  timeout_secs: 10,
  target: {
    command: null,
    cwd: null,
    url: "https://ci.example.com/saka",
    secret: "HOOK_SIGNING_KEY",
    secrets: [],
    channel: null,
  },
};

export function hookRun(over: Partial<HookRun> = {}): HookRun {
  return {
    id: "0193f0c4-1b2a-7c3d-9e8f-0011223355a1",
    project_id: PROJECT_ID,
    hook: "deploy-staging",
    action: "run",
    mode: "after",
    trigger: "merged",
    state: "succeeded",
    work_item_id: TASK_ID,
    task_key: "tasks/s1",
    spec_id: null,
    repo_id: null,
    sha: null,
    context: {},
    rerun_of: null,
    attempts: 1,
    result: { exit_code: 0, output_tail: "deployed" },
    error: null,
    queued_at: "2026-11-06T10:00:00.000Z",
    finished_at: "2026-11-06T10:00:05.000Z",
    duration_ms: 5000,
    ...over,
  };
}

export const failedRun = hookRun({
  id: "0193f0c4-1b2a-7c3d-9e8f-0011223355a2",
  hook: "announce",
  action: "webhook",
  trigger: "task_done",
  state: "failed",
  result: { http_status: 404 },
  error: { code: "hook_webhook_rejected", params: { status: 404 } },
});

export const pendingRun = hookRun({
  id: "0193f0c4-1b2a-7c3d-9e8f-0011223355a3",
  state: "pending",
  result: null,
  finished_at: null,
  duration_ms: null,
});

export const gateRun = hookRun({
  id: "0193f0c4-1b2a-7c3d-9e8f-0011223355a4",
  hook: "smoke",
  mode: "gate",
  state: "failed",
  error: { code: "hook_command_failed", params: { exit_code: 2 } },
  result: { exit_code: 2, output_tail: "FAIL smoke" },
});
