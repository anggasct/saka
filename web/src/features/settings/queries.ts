import { useQuery } from "@tanstack/react-query";
import type { QueryClient } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { components } from "@/api/schema";

export type Project = components["schemas"]["Project"];
export type ProjectRepo = components["schemas"]["ProjectRepo"];
export type ConfigError = components["schemas"]["ProjectConfigError"];
export type SecretName = components["schemas"]["SecretName"];
export type ReloadResult = components["schemas"]["ProjectReload"];

export const SECRET_NAME = /^[A-Z_][A-Z0-9_]{0,63}$/;

export const settingsKeys = {
  secrets: (projectId: string) => [QUERY_ROOTS.secrets, projectId] as const,
};

function headers(idempotencyKey: string) {
  return { "Idempotency-Key": idempotencyKey };
}

export function useSecrets(projectId: string) {
  return useQuery({
    queryKey: settingsKeys.secrets(projectId),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}/secrets", {
          params: { path: { project_id: projectId } },
          signal,
        }),
      ).items,
  });
}

export async function reloadProject(
  projectId: string,
  idempotencyKey: string,
): Promise<ReloadResult> {
  return unwrap(
    await api.POST("/api/v1/projects/{project_id}/actions/reload", {
      params: {
        path: { project_id: projectId },
        header: headers(idempotencyKey),
      },
      body: {},
    }),
  );
}

export async function putSecret(
  projectId: string,
  name: string,
  value: string,
  idempotencyKey: string,
): Promise<void> {
  const result = await api.PUT("/api/v1/projects/{project_id}/secrets/{name}", {
    params: {
      path: { project_id: projectId, name },
      header: headers(idempotencyKey),
    },
    body: { value },
  });
  if (result.error !== undefined) {
    unwrap(result);
  }
}

export async function deleteSecret(
  projectId: string,
  name: string,
  idempotencyKey: string,
): Promise<void> {
  const result = await api.DELETE(
    "/api/v1/projects/{project_id}/secrets/{name}",
    {
      params: {
        path: { project_id: projectId, name },
        header: headers(idempotencyKey),
      },
    },
  );
  if (result.error !== undefined) {
    unwrap(result);
  }
}

export function invalidate(client: QueryClient, root: string): Promise<void> {
  return client.invalidateQueries({ queryKey: [root] });
}
