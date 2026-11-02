import { useQueries, useQuery } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { components } from "@/api/schema";

export type ProjectItem = components["schemas"]["Project"];
export type SpecItem = components["schemas"]["Spec"];
export type WorkItem = components["schemas"]["WorkItem"];
export type WorkItemDetail = components["schemas"]["WorkItemDetail"];
export type WorkPacket = components["schemas"]["WorkPacket"];
export type WaitingOn = components["schemas"]["WaitingOn"];
export type Gate = components["schemas"]["Gate"];
export type StageRun = components["schemas"]["StageRunItem"];
export type Blocker = components["schemas"]["Blocker"];

const LIMIT = 200;

export const deliveryKeys = {
  projects: () => [QUERY_ROOTS.projects] as const,
  project: (id: string) => [QUERY_ROOTS.projects, id] as const,
  specs: (projectId: string) => [QUERY_ROOTS.specs, projectId] as const,
  spec: (id: string) => [QUERY_ROOTS.specs, "detail", id] as const,
  workItems: (projectId: string) => [QUERY_ROOTS.workItems, projectId] as const,
  workItem: (id: string) => [QUERY_ROOTS.workItems, "detail", id] as const,
  workPacket: (id: string) => [QUERY_ROOTS.workItems, "packet", id] as const,
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

export function useWorkItems(projectId: string | undefined) {
  return useQuery({
    queryKey: deliveryKeys.workItems(projectId ?? ""),
    enabled: projectId !== undefined,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/work-items", {
          params: { query: { project_id: projectId ?? "", limit: LIMIT } },
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

export function useWorkItem(id: string) {
  return useQuery({
    queryKey: deliveryKeys.workItem(id),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/work-items/{work_item_id}", {
          params: { path: { work_item_id: id } },
          signal,
        }),
      ),
  });
}

export function useSpec(id: string | null | undefined) {
  return useQuery({
    queryKey: deliveryKeys.spec(id ?? ""),
    enabled: id !== null && id !== undefined,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/specs/{spec_id}", {
          params: { path: { spec_id: id ?? "" } },
          signal,
        }),
      ),
  });
}

export function useProject(id: string | undefined) {
  return useQuery({
    queryKey: deliveryKeys.project(id ?? ""),
    enabled: id !== undefined,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}", {
          params: { path: { project_id: id ?? "" } },
          signal,
        }),
      ),
  });
}

export function useWorkPacket(id: string, enabled: boolean) {
  return useQuery({
    queryKey: deliveryKeys.workPacket(id),
    enabled,
    retry: false,
    staleTime: Infinity,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/work-items/{work_item_id}/work-packet", {
          params: { path: { work_item_id: id } },
          signal,
        }),
      ),
  });
}
