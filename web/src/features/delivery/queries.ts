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
export type PermissionRequest = components["schemas"]["PermissionRequest"];
export type Decision = components["schemas"]["Decision"];
export type Measure = components["schemas"]["Measure"];

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

export function useTaskPermissions(workItemId: string | undefined) {
  return useQuery({
    queryKey: deliveryKeys.taskPermissions(workItemId ?? ""),
    enabled: workItemId !== undefined,
    queryFn: async ({ signal }) =>
      byRequestedAt(
        unwrap(
          await api.GET("/api/v1/permission-requests", {
            params: {
              query: {
                state: "pending",
                work_item_id: workItemId ?? "",
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

function headers(idempotencyKey: string) {
  return { "Idempotency-Key": idempotencyKey };
}

export async function startTask(
  workItemId: string,
  version: number,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/work-items/{work_item_id}/actions/start", {
      params: {
        path: { work_item_id: workItemId },
        header: { ...headers(idempotencyKey), "If-Match": `W/"${version}"` },
      },
      body: {},
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

export async function revertTask(
  workItemId: string,
  version: number,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/work-items/{work_item_id}/actions/revert", {
      params: {
        path: { work_item_id: workItemId },
        header: { ...headers(idempotencyKey), "If-Match": `W/"${version}"` },
      },
      body: { reason },
    }),
  );
}

export async function redoTask(
  workItemId: string,
  version: number,
  reason: string,
  note: string | null,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/work-items/{work_item_id}/actions/redo", {
      params: {
        path: { work_item_id: workItemId },
        header: { ...headers(idempotencyKey), "If-Match": `W/"${version}"` },
      },
      body: { reason, note },
    }),
  );
}

export async function withdrawSpec(
  specId: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/specs/{spec_id}/withdraw", {
      params: {
        path: { spec_id: specId },
        header: headers(idempotencyKey),
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

export async function runPrimaryAction(
  workItemId: string,
  blockerId: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/work-items/{work_item_id}/actions/primary", {
      params: {
        path: { work_item_id: workItemId },
        header: headers(idempotencyKey),
      },
      body: { blocker_id: blockerId, reason },
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

export async function takeoverTask(
  workItemId: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/work-items/{work_item_id}/actions/takeover", {
      params: {
        path: { work_item_id: workItemId },
        header: headers(idempotencyKey),
      },
      body: { reason },
    }),
  );
}

export async function returnTask(
  workItemId: string,
  stage: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/work-items/{work_item_id}/actions/return", {
      params: {
        path: { work_item_id: workItemId },
        header: headers(idempotencyKey),
      },
      body: { stage, reason },
    }),
  );
}

export async function requestReview(
  workItemId: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/work-items/{work_item_id}/actions/review", {
      params: {
        path: { work_item_id: workItemId },
        header: headers(idempotencyKey),
      },
      body: {},
    }),
  );
}

export async function requestFix(
  workItemId: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/work-items/{work_item_id}/actions/fix", {
      params: {
        path: { work_item_id: workItemId },
        header: headers(idempotencyKey),
      },
      body: {},
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
