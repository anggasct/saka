import type { ActiveSession, Session } from "@/features/sessions/queries";

import { PROJECT_ID } from "./fixtures";

export const WORK_ITEM = "0193f0c4-1b2a-7c3d-9e8f-0011223344b1";
export const STAGE_RUN = "0193f0c4-1b2a-7c3d-9e8f-0011223344c1";
export const DRAFT = "0193f0c4-1b2a-7c3d-9e8f-0011223344b2";
export const ATTEMPT = "0193f0c4-1b2a-7c3d-9e8f-0011223344b3";

export function sessionId(n: number): string {
  return `0193f0c4-1b2a-7c3d-9e8f-00112233450${n}`;
}

export function activeSession(
  n: number,
  extra: Partial<ActiveSession> = {},
): ActiveSession {
  return {
    id: sessionId(n),
    state: "running",
    role: "dev",
    project_id: PROJECT_ID,
    project_name: "tasks",
    owner: {
      kind: "stage_run",
      work_item_id: WORK_ITEM,
      stage_run_id: STAGE_RUN,
    },
    title: `feat-x/s${n}`,
    model: "opus",
    started_at: "2026-11-06T10:00:00.000Z",
    last_activity_at: "2026-11-06T10:05:00.000Z",
    pending: { permissions: 0, decisions: 0 },
    ...extra,
  };
}

export function fullSession(n: number, extra: Partial<Session> = {}): Session {
  return {
    id: sessionId(n),
    project_id: PROJECT_ID,
    project_name: "tasks",
    role: "dev",
    title: `feat-x/s${n}`,
    owner: {
      kind: "stage_run",
      work_item_id: WORK_ITEM,
      stage_run_id: STAGE_RUN,
    },
    environment: {
      branch: "saka/feat-x-s1",
      toolchains: [{ tool: "node", version: "26.13.0" }],
      ports: { start: 41000, count: 4 },
      setup_state: "ready",
    },
    cli: "claude",
    cli_version: "2.1.0",
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
    started_at: "2026-11-06T10:00:00.000Z",
    ended_at: null,
    ...extra,
  };
}
