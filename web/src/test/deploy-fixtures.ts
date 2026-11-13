import type {
  DeployEnvironment,
  Deployment,
  DeploymentCheck,
} from "@/features/deployments/queries";

import { PROJECT_ID } from "./fixtures";

export const DEPLOYMENT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344d1";
export const OTHER_DEPLOYMENT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344d2";
export const DRAFT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344d3";
export const ENVIRONMENTS = `/api/v1/projects/${PROJECT_ID}/environments`;
export const DEPLOYMENTS = `/api/v1/projects/${PROJECT_ID}/deployments`;
export const REDEPLOY = `${ENVIRONMENTS}/staging/actions/deploy`;
export const detailUrl = (id: string) => `/api/v1/deployments/${id}`;

const REPO = {
  id: "0193f0c4-1b2a-7c3d-9e8f-001122334400",
  name: "app",
  full_name: "acme/app",
};

export function deployment(over: Partial<Deployment> = {}): Deployment {
  return {
    id: DEPLOYMENT_ID,
    project_id: PROJECT_ID,
    environment: "staging",
    repo: REPO,
    branch: "main",
    sha: "feedface".padEnd(40, "0"),
    previous_sha: "c0ffee".padEnd(40, "0"),
    cause: "merge",
    state: "succeeded",
    phase: null,
    error: null,
    verification: null,
    actor: "operator",
    reason: null,
    queued_at: "2026-11-06T10:00:00.000Z",
    started_at: "2026-11-06T10:00:05.000Z",
    finished_at: "2026-11-06T10:03:00.000Z",
    ...over,
  };
}

export function check(over: Partial<DeploymentCheck> = {}): DeploymentCheck {
  return {
    phase: "verify",
    ordinal: 0,
    command: "curl -f https://staging.example.com/health",
    state: "passed",
    exit_code: 0,
    http_status: null,
    attempts: 1,
    duration_ms: 2000,
    output_tail: "ok",
    started_at: "2026-11-06T10:01:00.000Z",
    finished_at: "2026-11-06T10:01:02.000Z",
    ...over,
  };
}

export const failedDetail = deployment({
  state: "failed",
  phase: "verify",
  error: { code: "verify_failed", params: { failed: 1, total: 2 } },
  verification: {
    ready: "curl -f https://staging.example.com/ready",
    commands: ["npm run smoke", "npm run e2e"],
    mode: "commands",
  },
  checks: [
    check({
      phase: "deploy",
      command: null,
      exit_code: 0,
      output_tail: "deploy log line",
      ordinal: 0,
    }),
    check({
      phase: "ready",
      command: "curl -f https://staging.example.com/ready",
      http_status: 200,
      exit_code: null,
      attempts: 3,
      output_tail: "",
      ordinal: 0,
    }),
    check({
      phase: "verify",
      command: "npm run smoke",
      state: "passed",
      ordinal: 0,
      output_tail: "smoke ok",
    }),
    check({
      phase: "verify",
      command: "npm run e2e",
      state: "failed",
      exit_code: 1,
      ordinal: 1,
      output_tail: "<b>FAIL</b> e2e",
    }),
  ],
  merges: [
    {
      change_request_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344e1",
      number: 12,
      url: "https://github.com/acme/app/pull/12",
      merge_commit_sha: "abc123".padEnd(40, "0"),
    },
  ],
  tasks: [
    {
      id: "0193f0c4-1b2a-7c3d-9e8f-001122334501",
      key: "tasks-done/s1",
      gate: "failed",
    },
  ],
  fix_draft: { id: DRAFT_ID, state: "drafting" },
});

export function environment(
  over: Partial<DeployEnvironment> = {},
): DeployEnvironment {
  return {
    name: "staging",
    repo: REPO,
    branch: "main",
    action: "run",
    target_url: "https://staging.example.com",
    gate: true,
    on_failure: "revert",
    verification: { ready: true, commands: 2, warnings: [] },
    queued: null,
    active: null,
    last_succeeded: {
      id: OTHER_DEPLOYMENT_ID,
      state: "succeeded",
      sha: "c0ffee".padEnd(40, "0"),
      phase: null,
      finished_at: "2026-11-05T10:00:00.000Z",
    },
    ...over,
  };
}
