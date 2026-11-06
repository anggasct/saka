import { useQuery } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { components } from "@/api/schema";

export type InboxItem = components["schemas"]["InboxItem"];
export type InboxSummary = components["schemas"]["InboxSummary"];

const LIMIT = 200;

export const inboxKeys = {
  list: () => [QUERY_ROOTS.inbox, "list"] as const,
  summary: () => [QUERY_ROOTS.inbox, "summary"] as const,
};

export function useInbox() {
  return useQuery({
    queryKey: inboxKeys.list(),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/inbox", {
          params: { query: { limit: LIMIT } },
          signal,
        }),
      ).items,
  });
}

export function useInboxSummary() {
  return useQuery({
    queryKey: inboxKeys.summary(),
    queryFn: async ({ signal }) =>
      unwrap(await api.GET("/api/v1/inbox/summary", { signal })),
  });
}

export async function dismissItem(
  itemId: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/inbox/{inbox_item_id}/dismiss", {
      params: {
        path: { inbox_item_id: itemId },
        header: { "Idempotency-Key": idempotencyKey },
      },
      body: { reason },
    }),
  );
}

export async function approveSpec(
  specId: string,
  revisionId: string,
  reason: string | null,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/specs/{spec_id}/approve", {
      params: {
        path: { spec_id: specId },
        header: {
          "Idempotency-Key": idempotencyKey,
          "If-Match": `"${revisionId}"`,
        },
      },
      body: { reason },
    }),
  );
}
