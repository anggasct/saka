import type { InboxItem } from "@/features/inbox/queries";

import { PROJECT_ID, SPEC_ID } from "./fixtures";

export const TASK_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a1";
export const BLOCKER_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344b1";
export const REQUEST_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344c1";
export const REVISION_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334477";

let next = 0;

export function inboxItem(overrides: Partial<InboxItem> = {}): InboxItem {
  next += 1;
  const id = `0193f0c4-1b2a-7c3d-9e8f-0011223399${String(next).padStart(2, "0")}`;
  return {
    id,
    project_id: PROJECT_ID,
    project_name: "tasks",
    kind: "blocker",
    subject: "blocker",
    state: "open",
    work_item_id: TASK_ID,
    spec_id: SPEC_ID,
    title: {
      task_key: "tasks-done/s1",
      stage: "dev",
      spec_slug: "tasks-done",
      spec_title: "Mark tasks as done",
      spec_path: "docs/specs/tasks-done.md",
      draft_path: null,
    },
    params: { code: "ci_red", head_sha: "a9e3d95".padEnd(40, "0") },
    action: { kind: "retry", params: { blocker_id: BLOCKER_ID } },
    dismissible: false,
    opened_at: "2026-11-06T10:00:00.000Z",
    closed_at: null,
    ...overrides,
  };
}

export const permissionItem = inboxItem({
  kind: "permission",
  subject: "permission",
  params: { tool: "Bash", summary: "npm install left-pad" },
  action: {
    kind: "answer_permission",
    params: { permission_request_id: REQUEST_ID },
  },
});

export const retryItem = inboxItem();

export const secretItem = inboxItem({
  params: { code: "secret_missing", names: ["NPM_TOKEN"] },
  action: { kind: "set_secret", params: { names: ["NPM_TOKEN"] } },
});

export const unclearItem = inboxItem({
  kind: "spec_unclear",
  params: { code: "spec_unclear", questions: ["Which store?"] },
  action: { kind: "edit_spec", params: { blocker_id: BLOCKER_ID } },
});

export const approvalItem = inboxItem({
  kind: "approval",
  subject: "spec",
  work_item_id: null,
  title: {
    task_key: null,
    stage: null,
    spec_slug: "tasks-done",
    spec_title: "Mark tasks as done",
    spec_path: "docs/specs/tasks-done.md",
    draft_path: null,
  },
  params: { revision: REVISION_ID },
  action: {
    kind: "approve_spec",
    params: { spec_id: SPEC_ID, revision: REVISION_ID },
  },
  dismissible: true,
});

export const INTAKE_ID = "0193f0c4-1b2a-7c3d-9e8f-001122338801";

const ISSUE = {
  url: "https://github.com/acme/tasks/issues/41",
  number: 41,
  title: "Crash on start",
  repo: "acme/tasks",
};

export const issueApprovalItem = inboxItem({
  ...approvalItem,
  params: { revision: REVISION_ID, issue: ISSUE },
});

const INTAKE_PARAMS = {
  issue: ISSUE,
  code: "draft_failed",
  failure: {},
  lane: "draft",
  kind: "feature",
};

export const intakeItem = inboxItem({
  kind: "intake",
  subject: "intake",
  work_item_id: null,
  title: {
    task_key: null,
    stage: null,
    spec_slug: null,
    spec_title: null,
    spec_path: null,
    draft_path: null,
  },
  params: { ...INTAKE_PARAMS, retries: 1, can_retry: true },
  action: { kind: "retry_intake", params: { intake_id: INTAKE_ID } },
});

export const exhaustedIntakeItem = inboxItem({
  ...intakeItem,
  params: { ...INTAKE_PARAMS, retries: 3, can_retry: false },
  action: { kind: "dismiss_intake", params: { intake_id: INTAKE_ID } },
});

export const startItem = inboxItem({
  kind: "approval",
  subject: "step_start",
  params: { head_sha: null },
  action: { kind: "start_step", params: { work_item_id: TASK_ID, version: 3 } },
});

export const mergeItem = inboxItem({
  kind: "approval",
  subject: "merge",
  params: { head_sha: "a9e3d95".padEnd(40, "0") },
  action: { kind: "merge", params: { work_item_id: TASK_ID, version: 9 } },
});

export function summary(
  open: number,
  extra: Partial<{ running: number; merged_today: number }> = {},
) {
  return {
    open,
    by_kind: {
      decision: 0,
      approval: 0,
      permission: open,
      blocker: 0,
      spec_unclear: 0,
    },
    running: 0,
    merged_today: 0,
    ...extra,
  };
}

export const DECISION_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344d1";

export const decisionOptions = [
  { label: "Local disk", detail: "Simple, no cost" },
  { label: "R2", detail: null },
];

export const decisionItem = inboxItem({
  kind: "decision",
  subject: "decision",
  params: {
    question: "Where do we store audio?",
    context: "The recorder needs a place to keep files.",
    options: decisionOptions,
    expires_at: "2026-11-09T10:00:00.000Z",
  },
  action: {
    kind: "answer_decision",
    params: { decision_request_id: DECISION_ID },
  },
});
