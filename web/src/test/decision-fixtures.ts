import type { DecisionRequest } from "@/features/decisions/queries";

import { PROJECT_ID } from "./fixtures";

export const WORK_ITEM_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a1";

export function decision(
  n: number,
  extra: Partial<DecisionRequest> = {},
): DecisionRequest {
  return {
    id: `0193f0c4-1b2a-7c3d-9e8f-0011223344d${n}`,
    project_id: PROJECT_ID,
    owner: {
      kind: "stage_run",
      work_item_id: WORK_ITEM_ID,
      stage_run_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344c1",
    },
    state: "asked",
    question: "Where do we store audio?",
    context: "The recorder needs a place to keep files.",
    options: [
      { label: "Local disk", detail: "Simple, no cost" },
      { label: "R2", detail: null },
    ],
    answer: null,
    asked_at: "2026-11-06T10:00:00.000Z",
    expires_at: "2026-11-09T10:00:00.000Z",
    ...extra,
  };
}
