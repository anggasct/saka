import type {
  OperatorRule,
  PermissionHistoryItem,
  PermissionRule,
} from "@/features/settings/queries";

import { PROJECT_ID } from "./fixtures";

export const RULES = `/api/v1/projects/${PROJECT_ID}/permission-rules`;
export const PREVIEW = `${RULES}/preview`;
export const REQUESTS = "/api/v1/permission-requests";

const AT = "2026-10-31T10:00:00.000Z";

export const operatorRule: OperatorRule = {
  id: "safe-commands",
  effect: "allow",
  tools: ["Bash"],
  commands: ["go test *", "ls*"],
  paths: [],
};

export const denyRule: PermissionRule = {
  id: "0193f0c4-1b2a-7c3d-9e8f-0011223344c1",
  project_id: PROJECT_ID,
  effect: "deny",
  tool: "Bash",
  match: "glob",
  command: "rm *",
  path: null,
  note: "never delete",
  permission_request_id: null,
  created_by: "human:operator:local",
  created_at: AT,
  revoked_at: null,
  revoke_reason: null,
};

export const allowRule: PermissionRule = {
  ...denyRule,
  id: "0193f0c4-1b2a-7c3d-9e8f-0011223344c2",
  effect: "allow",
  match: "exact",
  command: "git status",
  note: null,
  permission_request_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344d1",
};

export function historyItem(
  n: number,
  source: string | null,
  summary: string,
): PermissionHistoryItem {
  return {
    id: `0193f0c4-1b2a-7c3d-9e8f-00112233${String(n).padStart(4, "0")}`,
    project_id: PROJECT_ID,
    owner: {
      kind: "stage_run",
      work_item_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344aa",
      stage_run_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344ab",
    },
    session_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344ac",
    tool: "Bash",
    summary,
    input: { command: summary },
    state: source === null ? "pending" : "allowed",
    answer_source: source,
    message: null,
    requested_at: AT,
    answered_at: source === null ? null : AT,
  };
}
