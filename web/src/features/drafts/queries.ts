import { keepPreviousData, useQuery } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { components } from "@/api/schema";

export type SpecDraft = components["schemas"]["SpecDraft"];
export type SpecDraftContent = components["schemas"]["SpecDraftContent"];
export type CreateSpecDraftBody = components["schemas"]["CreateSpecDraftBody"];

export type DraftFilter = "active" | "all";

const LIMIT = 200;

export const draftKeys = {
  list: (projectId: string, filter: DraftFilter) =>
    [QUERY_ROOTS.specDrafts, "project", projectId, filter] as const,
  detail: (id: string) => [QUERY_ROOTS.specDrafts, "detail", id] as const,
  content: (id: string) => [QUERY_ROOTS.specDrafts, "content", id] as const,
};

export function useProjectDrafts(projectId: string, filter: DraftFilter) {
  return useQuery({
    queryKey: draftKeys.list(projectId, filter),
    placeholderData: keepPreviousData,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}/spec-drafts", {
          params: {
            path: { project_id: projectId },
            query: { state: filter, limit: LIMIT },
          },
          signal,
        }),
      ).items,
  });
}

export function useDraft(id: string) {
  return useQuery({
    queryKey: draftKeys.detail(id),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/spec-drafts/{spec_draft_id}", {
          params: { path: { spec_draft_id: id } },
          signal,
        }),
      ),
  });
}

export function useDraftContent(id: string, enabled: boolean) {
  return useQuery({
    queryKey: draftKeys.content(id),
    enabled,
    retry: false,
    staleTime: Infinity,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/spec-drafts/{spec_draft_id}/content", {
          params: { path: { spec_draft_id: id } },
          signal,
        }),
      ),
  });
}

export async function createDraft(
  projectId: string,
  body: CreateSpecDraftBody,
  idempotencyKey: string,
): Promise<SpecDraft> {
  return unwrap(
    await api.POST("/api/v1/projects/{project_id}/spec-drafts", {
      params: {
        path: { project_id: projectId },
        header: { "Idempotency-Key": idempotencyKey },
      },
      body,
    }),
  );
}

export async function retryDraft(
  id: string,
  reason: string,
  note: string | null,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/spec-drafts/{spec_draft_id}/actions/retry", {
      params: {
        path: { spec_draft_id: id },
        header: { "Idempotency-Key": idempotencyKey },
      },
      body: { reason, note },
    }),
  );
}

export async function cancelDraft(
  id: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/spec-drafts/{spec_draft_id}/actions/cancel", {
      params: {
        path: { spec_draft_id: id },
        header: { "Idempotency-Key": idempotencyKey },
      },
      body: { reason },
    }),
  );
}
