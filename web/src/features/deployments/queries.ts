import {
  keepPreviousData,
  useInfiniteQuery,
  useQuery,
} from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { components } from "@/api/schema";

export type Deployment = components["schemas"]["Deployment"];
export type DeploymentCheck = components["schemas"]["DeploymentCheck"];
export type DeployEnvironment = components["schemas"]["DeployEnvironment"];

export const DEPLOYMENT_STATES = [
  "queued",
  "deploying",
  "awaiting_ready",
  "verifying",
  "succeeded",
  "failed",
  "cancelled",
] as const;

const PAGE = 30;

const deploymentKeys = {
  environments: (projectId: string) =>
    [QUERY_ROOTS.deployments, "environments", projectId] as const,
  list: (projectId: string, environment: string, state: string) =>
    [QUERY_ROOTS.deployments, "list", projectId, environment, state] as const,
  detail: (id: string) => [QUERY_ROOTS.deployments, "detail", id] as const,
};

function headers(idempotencyKey: string) {
  return { "Idempotency-Key": idempotencyKey };
}

export function useEnvironments(projectId: string) {
  return useQuery({
    queryKey: deploymentKeys.environments(projectId),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}/environments", {
          params: { path: { project_id: projectId } },
          signal,
        }),
      ).items,
  });
}

export function useDeployments(
  projectId: string,
  environment: string | undefined,
  state: string | undefined,
) {
  return useInfiniteQuery({
    queryKey: deploymentKeys.list(projectId, environment ?? "", state ?? ""),
    initialPageParam: undefined as string | undefined,
    placeholderData: keepPreviousData,
    queryFn: async ({ pageParam, signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}/deployments", {
          params: {
            path: { project_id: projectId },
            query: {
              limit: PAGE,
              ...(environment === undefined ? {} : { environment }),
              ...(state === undefined ? {} : { state }),
              ...(pageParam === undefined ? {} : { cursor: pageParam }),
            },
          },
          signal,
        }),
      ),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
}

export function useDeployment(id: string | undefined) {
  return useQuery({
    queryKey: deploymentKeys.detail(id ?? ""),
    enabled: id !== undefined,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/deployments/{deployment_id}", {
          params: { path: { deployment_id: id ?? "" } },
          signal,
        }),
      ),
  });
}

export async function redeployEnvironment(
  projectId: string,
  environment: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST(
      "/api/v1/projects/{project_id}/environments/{environment}/actions/deploy",
      {
        params: {
          path: { project_id: projectId, environment },
          header: headers(idempotencyKey),
        },
        body: { reason },
      },
    ),
  );
}

export async function cancelDeployment(
  deploymentId: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/deployments/{deployment_id}/actions/cancel", {
      params: {
        path: { deployment_id: deploymentId },
        header: headers(idempotencyKey),
      },
      body: { reason },
    }),
  );
}

export async function draftFix(
  deploymentId: string,
  note: string | null,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/deployments/{deployment_id}/actions/draft-fix", {
      params: {
        path: { deployment_id: deploymentId },
        header: headers(idempotencyKey),
      },
      body: { note },
    }),
  );
}

export async function acceptUnverified(
  workItemId: string,
  blockerId: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST(
      "/api/v1/work-items/{work_item_id}/actions/accept-unverified",
      {
        params: {
          path: { work_item_id: workItemId },
          header: headers(idempotencyKey),
        },
        body: { blocker_id: blockerId, reason },
      },
    ),
  );
}
