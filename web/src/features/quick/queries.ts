import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema";

export type QuickSpecBody = components["schemas"]["QuickSpecBody"];
export type QuickSpecCreated = components["schemas"]["QuickSpecCreated"];

export async function createQuickSpec(
  projectId: string,
  body: QuickSpecBody,
  idempotencyKey: string,
): Promise<QuickSpecCreated> {
  return unwrap(
    await api.POST("/api/v1/projects/{project_id}/quick-specs", {
      params: {
        path: { project_id: projectId },
        header: { "Idempotency-Key": idempotencyKey },
      },
      body,
    }),
  );
}
