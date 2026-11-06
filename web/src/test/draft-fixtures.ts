import type { Session } from "@/features/sessions/queries";
import type { SpecDraft, SpecDraftContent } from "@/features/drafts/queries";

import { PROJECT_ID } from "./fixtures";

export const DRAFT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344d1";
export const OTHER_DRAFT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344d2";
export const ATTEMPT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a1";
export const DRAFT_SESSION_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344e1";
export const DRAFT_PATH = "docs/specs/avatar-upload.md";
export const DRAFTS = `/api/v1/projects/${PROJECT_ID}/spec-drafts`;
export const DRAFT = `/api/v1/spec-drafts/${DRAFT_ID}`;
export const DRAFT_CONTENT = `${DRAFT}/content`;
export const AT = "2026-11-03T10:00:00.000Z";

export const DRAFT_TEXT = [
  "---",
  "saka: spec",
  "---",
  "# Avatar upload",
  "",
  "## Acceptance criteria",
  "",
  "- **AC-01**: A user can upload an avatar.",
  "",
  "## Done when",
  "",
  "- `make test`",
  "",
  "## Decisions",
  "",
  "### Q1: Where are uploaded files stored?",
  "",
  "- [ ] **Local disk** (recommended) — one node only.",
  "- [ ] **S3-compatible storage** — scales out.",
  "",
].join("\r\n");

const attempt = {
  id: ATTEMPT_ID,
  number: 1,
  trigger: "auto",
  reason: null,
  note: null,
  state: "running",
  base_sha: "9f1c".padEnd(40, "0"),
  rounds: 1,
  max_rounds: 4,
  packet_sha256: "4b8e".padEnd(64, "0"),
  started_at: AT,
  ended_at: null,
};

const budget = {
  attempt_id: ATTEMPT_ID,
  warn_percent: 80,
  running_minutes: { used: 3.5, limit: 30 },
  turns: { used: 2, limit: 6 },
  tokens: { used: 41000, limit: 0 },
  cost_usd: { used: "0.4100", limit: "0" },
};

const failedRound = {
  attempt: 1,
  round: 1,
  outcome: "failed",
  bytes: 2140,
  content_hash: "sha256:" + "1a2b".padEnd(64, "0"),
  decisions_open: 1,
  parser_version: 4,
  issues: [
    {
      code: "step_ac_unknown",
      severity: "error",
      line: 22,
      params: { step: "S2", ac: "AC-09" },
    },
    {
      code: "decision_unanswered",
      severity: "error",
      line: 16,
      params: { id: "Q1" },
    },
  ],
  created_at: AT,
};

export function draft(extra: Partial<SpecDraft> = {}): SpecDraft {
  return {
    id: DRAFT_ID,
    project_id: PROJECT_ID,
    path: DRAFT_PATH,
    slug: "avatar-upload",
    idea: "Let users upload an avatar.",
    locale: "en",
    mode: "draft",
    target_kind: "new",
    context_paths: ["docs/architecture.md"],
    state: "drafting",
    attempt,
    last_round: failedRound,
    session: {
      id: DRAFT_SESSION_ID,
      state: "running",
      cli: "claude",
      started_at: AT,
      ended_at: null,
    },
    budget,
    waiting_on: [],
    failure: null,
    primary_action: null,
    available_actions: ["cancel"],
    retries: 0,
    spec_id: null,
    commit: null,
    version: 6,
    created_at: AT,
    updated_at: AT,
    finished_at: null,
    ...extra,
  };
}

export const queuedDraft = draft({
  state: "queued",
  attempt: null,
  last_round: null,
  session: null,
  budget: null,
  waiting_on: [
    { kind: "architect_slot" },
    {
      kind: "quota",
      account: "claude-main",
      window: "five_hour",
      utilization: 0.92,
      resets_at: null,
    },
  ],
});

export const preparingDraft = draft({
  state: "preparing",
  last_round: null,
  session: null,
  attempt: { ...attempt, rounds: 0 },
});

export const SESSION_CHANGES = `${DRAFT}/changes`;

export const awaitingDraft = draft({
  mode: "session",
  state: "awaiting_operator",
  last_round: { ...failedRound, round: 2, outcome: "passed", issues: [] },
  session: {
    id: DRAFT_SESSION_ID,
    state: "idle",
    cli: "claude",
    started_at: AT,
    ended_at: null,
  },
  available_actions: ["message", "commit", "discard", "cancel"],
});

export const awaitingFailing = draft({
  ...awaitingDraft,
  last_round: failedRound,
  available_actions: ["message", "discard", "cancel"],
});

export const changes = {
  files: [
    {
      path: "docs/notes/avatar.md",
      status: "added",
      diff: "--- /dev/null\n+++ b/docs/notes/avatar.md\n+# Notes\n",
      truncated: false,
    },
    {
      path: DRAFT_PATH,
      status: "modified",
      diff: "--- a/x\n+++ b/x\n-Old line\n+New line\n",
      truncated: true,
    },
  ],
  lint: { passed: true, findings: [] },
};

export const committingDraft = draft({
  state: "committing",
  available_actions: [],
  last_round: { ...failedRound, round: 2, outcome: "passed", issues: [] },
});

export const succeededDraft = draft({
  state: "succeeded",
  attempt: { ...attempt, state: "succeeded", ended_at: AT },
  budget: null,
  available_actions: [],
  spec_id: "0193f0c4-1b2a-7c3d-9e8f-001122334466",
  commit: "c0ffee".padEnd(40, "1"),
  finished_at: AT,
});

export const cancelledDraft = draft({
  state: "cancelled",
  attempt: { ...attempt, state: "cancelled", ended_at: AT },
  budget: null,
  available_actions: [],
  finished_at: AT,
});

function failed(
  code: string,
  params: Record<string, unknown>,
  primary: string | null,
  actions: string[],
): SpecDraft {
  return draft({
    state: "failed",
    attempt: { ...attempt, state: "failed", rounds: 4, ended_at: AT },
    budget: null,
    failure: { code, params },
    primary_action: primary,
    available_actions: actions,
  });
}

export const failedRetry = failed(
  "session_lost",
  { cause: "runner_gone" },
  "retry",
  ["retry", "cancel"],
);

export const failedOpenWritten = failed(
  "lint_rounds_exhausted",
  { rounds: 4 },
  "open_written",
  ["retry", "cancel"],
);

export const failedOpenFile = failed(
  "register_failed",
  { code: "spec_slug_conflict", params: { slug: "avatar-upload" } },
  "open_file",
  ["cancel"],
);

export function draftSession(extra: Partial<Session> = {}): Session {
  return {
    id: DRAFT_SESSION_ID,
    project_id: PROJECT_ID,
    project_name: "tasks",
    role: "architect",
    title: "export",
    owner: {
      kind: "draft_attempt",
      spec_draft_id: DRAFT_ID,
      attempt_id: ATTEMPT_ID,
    },
    cli: "claude",
    cli_version: "2.1.316",
    model: "opus",
    state: "running",
    turns: 2,
    running_ms: 125_000,
    cost_usd: "0.4100",
    input_tokens: 1200,
    output_tokens: 300,
    resumed_from_session_id: null,
    resume_cause: null,
    end_reason: null,
    available_actions: ["steer", "interrupt", "stop"],
    started_at: AT,
    ended_at: null,
    ...extra,
  };
}

export function draftContent(content = DRAFT_TEXT): SpecDraftContent {
  return {
    attempt: 1,
    round: 4,
    path: DRAFT_PATH,
    bytes: Buffer.byteLength(content),
    content_hash: null,
    content,
  };
}

export function timeline(summaries: readonly string[]) {
  return {
    items: summaries.map((summary, offset) => ({
      offset,
      at: AT,
      kind: "message",
      summary,
      data: {},
    })),
    prev_before: null,
    next_after: null,
  };
}
