import { useQueries, useQuery } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { fetchTail, QUERY_ROOTS, timelineKey } from "@/api/events";
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
export type Session = components["schemas"]["Session"];
export type PermissionRequest = components["schemas"]["PermissionRequest"];
export type Decision = components["schemas"]["Decision"];

const LIMIT = 200;

export const deliveryKeys = {
  projects: () => [QUERY_ROOTS.projects] as const,
  project: (id: string) => [QUERY_ROOTS.projects, id] as const,
  specs: (projectId: string) => [QUERY_ROOTS.specs, projectId] as const,
  spec: (id: string) => [QUERY_ROOTS.specs, "detail", id] as const,
  workItems: (projectId: string) => [QUERY_ROOTS.workItems, projectId] as const,
  workItem: (id: string) => [QUERY_ROOTS.workItems, "detail", id] as const,
  workPacket: (id: string) => [QUERY_ROOTS.workItems, "packet", id] as const,
  sessions: (workItemId: string) => [QUERY_ROOTS.sessions, workItemId] as const,
  pendingPermissions: () => [QUERY_ROOTS.permissions, "pending"] as const,
  taskPermissions: (workItemId: string) =>
    [QUERY_ROOTS.permissions, "pending", workItemId] as const,
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

function byRequestedAt(items: PermissionRequest[]): PermissionRequest[] {
  return items.toSorted((a, b) => a.requested_at.localeCompare(b.requested_at));
}

export function usePendingPermissions() {
  return useQuery({
    queryKey: deliveryKeys.pendingPermissions(),
    queryFn: async ({ signal }) =>
      byRequestedAt(
        unwrap(
          await api.GET("/api/v1/permission-requests", {
            params: { query: { state: "pending", limit: LIMIT } },
            signal,
          }),
        ).items,
      ),
  });
}

export function useTaskPermissions(workItemId: string) {
  return useQuery({
    queryKey: deliveryKeys.taskPermissions(workItemId),
    queryFn: async ({ signal }) =>
      byRequestedAt(
        unwrap(
          await api.GET("/api/v1/permission-requests", {
            params: {
              query: {
                state: "pending",
                work_item_id: workItemId,
                limit: LIMIT,
              },
            },
            signal,
          }),
        ).items,
      ),
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

export function useSessions(workItemId: string) {
  return useQuery({
    queryKey: deliveryKeys.sessions(workItemId),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/sessions", {
          params: { query: { work_item_id: workItemId } },
          signal,
        }),
      ).items,
  });
}

export function useTimeline(sessionId: string) {
  return useQuery({
    queryKey: timelineKey(sessionId),
    queryFn: ({ signal }) => fetchTail(sessionId, signal),
    staleTime: Infinity,
    refetchOnWindowFocus: false,
  });
}

function headers(idempotencyKey: string) {
  return { "Idempotency-Key": idempotencyKey };
}

export async function steerSession(
  sessionId: string,
  text: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/sessions/{session_id}/steer", {
      params: {
        path: { session_id: sessionId },
        header: headers(idempotencyKey),
      },
      body: { text },
    }),
  );
}

export async function interruptSession(
  sessionId: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/sessions/{session_id}/interrupt", {
      params: {
        path: { session_id: sessionId },
        header: headers(idempotencyKey),
      },
      body: {},
    }),
  );
}

export async function stopSession(
  sessionId: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/sessions/{session_id}/stop", {
      params: {
        path: { session_id: sessionId },
        header: headers(idempotencyKey),
      },
      body: { reason },
    }),
  );
}

export async function mergeTask(
  workItemId: string,
  version: number,
  reason: string | null,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/work-items/{work_item_id}/actions/merge", {
      params: {
        path: { work_item_id: workItemId },
        header: { ...headers(idempotencyKey), "If-Match": `W/"${version}"` },
      },
      body: { reason },
    }),
  );
}

export async function retryTask(
  workItemId: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/work-items/{work_item_id}/actions/retry", {
      params: {
        path: { work_item_id: workItemId },
        header: headers(idempotencyKey),
      },
      body: { reason },
    }),
  );
}

export async function cancelTask(
  workItemId: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/work-items/{work_item_id}/actions/cancel", {
      params: {
        path: { work_item_id: workItemId },
        header: headers(idempotencyKey),
      },
      body: { reason },
    }),
  );
}

export async function answerPermission(
  permissionRequestId: string,
  answer: { decision: Decision; message: string | null; remember: boolean },
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST(
      "/api/v1/permission-requests/{permission_request_id}/answer",
      {
        params: {
          path: { permission_request_id: permissionRequestId },
          header: headers(idempotencyKey),
        },
        body: answer,
      },
    ),
  );
}
