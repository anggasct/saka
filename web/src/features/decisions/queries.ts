import { useQuery } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { components } from "@/api/schema";

export type DecisionRequest = components["schemas"]["DecisionRequest"];
export type DecisionOption = components["schemas"]["DecisionOption"];
export type DecisionRun =
  { kind: "work_item"; id: string } | { kind: "spec_draft"; id: string };

const LIMIT = 200;

export const decisionKeys = {
  run: (run: DecisionRun) => [QUERY_ROOTS.decisions, run.kind, run.id] as const,
};

export function useRunDecisions(run: DecisionRun) {
  return useQuery({
    queryKey: decisionKeys.run(run),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/decision-requests", {
          params: {
            query:
              run.kind === "work_item"
                ? { work_item_id: run.id, limit: LIMIT }
                : { spec_draft_id: run.id, limit: LIMIT },
          },
          signal,
        }),
      ).items,
  });
}

export async function answerDecision(
  id: string,
  answer: { option: number | null; note: string | null },
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/decision-requests/{decision_request_id}/answer", {
      params: {
        path: { decision_request_id: id },
        header: { "Idempotency-Key": idempotencyKey },
      },
      body: answer,
    }),
  );
}
