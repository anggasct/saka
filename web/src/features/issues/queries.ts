import { useQuery } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { components } from "@/api/schema";

export type IssueIntake = components["schemas"]["IssueIntake"];

const LIMIT = 200;

export const issueKeys = {
  list: (projectId: string) => [QUERY_ROOTS.issueIntakes, projectId] as const,
};

export function useIssueIntakes(projectId: string, enabled: boolean) {
  return useQuery({
    queryKey: issueKeys.list(projectId),
    enabled,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}/issue-intakes", {
          params: {
            path: { project_id: projectId },
            query: { limit: LIMIT },
          },
          signal,
        }),
      ).items,
  });
}

export async function createIssueIntake(
  projectId: string,
  body: { number: number; repo: string | null },
  idempotencyKey: string,
): Promise<IssueIntake> {
  return unwrap(
    await api.POST("/api/v1/projects/{project_id}/issue-intakes", {
      params: {
        path: { project_id: projectId },
        header: { "Idempotency-Key": idempotencyKey },
      },
      body,
    }),
  );
}

export async function dismissIssueIntake(
  intakeId: string,
  reason: string | null,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/issue-intakes/{intake_id}/actions/dismiss", {
      params: {
        path: { intake_id: intakeId },
        header: { "Idempotency-Key": idempotencyKey },
      },
      body: { reason },
    }),
  );
}

export async function retryIssueIntake(
  intakeId: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/issue-intakes/{intake_id}/actions/retry", {
      params: {
        path: { intake_id: intakeId },
        header: { "Idempotency-Key": idempotencyKey },
      },
      body: {},
    }),
  );
}
