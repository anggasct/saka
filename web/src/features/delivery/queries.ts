import { useQueries, useQuery } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { components } from "@/api/schema";

export type ProjectItem = components["schemas"]["Project"];
export type SpecItem = components["schemas"]["Spec"];
export type WorkItem = components["schemas"]["WorkItem"];

const LIMIT = 200;

export const deliveryKeys = {
  projects: () => [QUERY_ROOTS.projects] as const,
  specs: (projectId: string) => [QUERY_ROOTS.specs, projectId] as const,
  workItems: (projectId: string) => [QUERY_ROOTS.workItems, projectId] as const,
  pendingPermissions: () => [QUERY_ROOTS.permissions, "pending"] as const,
};

export function useProjects() {
  return useQuery({
    queryKey: deliveryKeys.projects(),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/projects", {
          params: { query: { limit: LIMIT } },
          signal,
        }),
      ).items,
  });
}

export function useSpecsOf(projectIds: readonly string[]) {
  return useQueries({
    queries: projectIds.map((projectId) => ({
      queryKey: deliveryKeys.specs(projectId),
      queryFn: async ({ signal }: { signal: AbortSignal }) =>
        unwrap(
          await api.GET("/api/v1/projects/{project_id}/specs", {
            params: {
              path: { project_id: projectId },
              query: { state: "approved,shipped", limit: LIMIT },
            },
            signal,
          }),
        ).items,
    })),
  });
}

export function useWorkItems(projectId: string) {
  return useQuery({
    queryKey: deliveryKeys.workItems(projectId),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/work-items", {
          params: { query: { project_id: projectId, limit: LIMIT } },
          signal,
        }),
      ).items,
  });
}

export function usePendingPermissionCount() {
  return useQuery({
    queryKey: deliveryKeys.pendingPermissions(),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/permission-requests", {
          params: { query: { state: "pending", limit: LIMIT } },
          signal,
        }),
      ).items.length,
  });
}
