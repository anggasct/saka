import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import type { QueryClient } from "@tanstack/react-query";

import { api, ApiError, unwrap } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { components } from "@/api/schema";

export type Project = components["schemas"]["Project"];
export type ProjectRepo = components["schemas"]["ProjectRepo"];
export type ConfigError = components["schemas"]["ProjectConfigError"];
export type SecretName = components["schemas"]["SecretName"];
export type ReloadResult = components["schemas"]["ProjectReload"];

export type Authorship = components["schemas"]["ProjectAuthorship"];
export type UserLink = components["schemas"]["UserLink"];
export type Hook = components["schemas"]["Hook"];
export type HookRun = components["schemas"]["HookRun"];

export type PermissionRule = components["schemas"]["PermissionRule"];
export type OperatorRule = components["schemas"]["OperatorPermissionRule"];
export type NewRule = components["schemas"]["NewPermissionRule"];
export type RulePreview = components["schemas"]["PermissionPreview"];
export type PermissionHistoryItem = components["schemas"]["PermissionRequest"];
export type AnsweredBy = "policy" | "human";

export const HISTORY_PAGE = 10;
export const HOOK_RUNS_SHOWN = 20;
export type Authorization = components["schemas"]["UserLinkAuthorization"];

export const SECRET_NAME = /^[A-Z_][A-Z0-9_]{0,63}$/;

export const settingsKeys = {
  permissionRules: (projectId: string) =>
    [QUERY_ROOTS.permissionRules, projectId] as const,
  permissionHistory: (projectId: string, source: AnsweredBy | "all") =>
    [QUERY_ROOTS.permissions, "history", projectId, source] as const,
  hooks: (projectId: string) => [QUERY_ROOTS.hooks, projectId] as const,
  hookRuns: (projectId: string) => [QUERY_ROOTS.hookRuns, projectId] as const,
  secrets: (projectId: string) => [QUERY_ROOTS.secrets, projectId] as const,
  authorship: (projectId: string) =>
    [QUERY_ROOTS.authorship, projectId] as const,
  userLink: (integrationId: string) =>
    [QUERY_ROOTS.userLinks, integrationId] as const,
  authorization: (authorizationId: string) =>
    [QUERY_ROOTS.userLinks, "authorization", authorizationId] as const,
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

export function useHooks(projectId: string) {
  return useQuery({
    queryKey: settingsKeys.hooks(projectId),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}/hooks", {
          params: { path: { project_id: projectId } },
          signal,
        }),
      ).items,
  });
}

export function useHookRuns(projectId: string) {
  return useQuery({
    queryKey: settingsKeys.hookRuns(projectId),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/hook-runs", {
          params: { query: { project_id: projectId, limit: HOOK_RUNS_SHOWN } },
          signal,
        }),
      ).items,
  });
}

export async function rerunHook(
  hookRunId: string,
  reason: string,
  idempotencyKey: string,
): Promise<HookRun> {
  return unwrap(
    await api.POST("/api/v1/hook-runs/{hook_run_id}/actions/rerun", {
      params: {
        path: { hook_run_id: hookRunId },
        header: headers(idempotencyKey),
      },
      body: { reason },
    }),
  );
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

export function useAuthorship(projectId: string) {
  return useQuery({
    queryKey: settingsKeys.authorship(projectId),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}/authorship", {
          params: { path: { project_id: projectId } },
          signal,
        }),
      ),
  });
}

export function useUserLink(integrationId: string) {
  return useQuery({
    queryKey: settingsKeys.userLink(integrationId),
    queryFn: async ({ signal }): Promise<UserLink | null> => {
      try {
        return unwrap(
          await api.GET("/api/v1/scm/{integration_id}/user-link", {
            params: { path: { integration_id: integrationId } },
            signal,
          }),
        );
      } catch (e) {
        if (e instanceof ApiError && e.code === "user_link_not_found") {
          return null;
        }
        throw e;
      }
    },
  });
}

export function useAuthorization(
  integrationId: string,
  initial: Authorization,
  interval: (current: Authorization) => number | false,
) {
  return useQuery({
    queryKey: settingsKeys.authorization(initial.id),
    initialData: initial,
    staleTime: Number.POSITIVE_INFINITY,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET(
          "/api/v1/scm/{integration_id}/user-link/authorizations/{authorization_id}",
          {
            params: {
              path: {
                integration_id: integrationId,
                authorization_id: initial.id,
              },
            },
            signal,
          },
        ),
      ),
    refetchInterval: (query) =>
      query.state.data === undefined ? false : interval(query.state.data),
    refetchIntervalInBackground: true,
  });
}

export async function startAuthorization(
  integrationId: string,
  idempotencyKey: string,
): Promise<Authorization> {
  return unwrap(
    await api.POST("/api/v1/scm/{integration_id}/user-link/authorizations", {
      params: {
        path: { integration_id: integrationId },
        header: headers(idempotencyKey),
      },
      body: {},
    }),
  );
}

export async function removeUserLink(
  integrationId: string,
  idempotencyKey: string,
): Promise<void> {
  const result = await api.DELETE("/api/v1/scm/{integration_id}/user-link", {
    params: {
      path: { integration_id: integrationId },
      header: headers(idempotencyKey),
    },
  });
  if (result.error !== undefined) {
    unwrap(result);
  }
}

export async function putAuthorship(
  projectId: string,
  mode: string,
  idempotencyKey: string,
): Promise<Authorship> {
  return unwrap(
    await api.PUT("/api/v1/projects/{project_id}/authorship", {
      params: {
        path: { project_id: projectId },
        header: headers(idempotencyKey),
      },
      body: { mode },
    }),
  );
}

export async function putUserToken(
  projectId: string,
  value: string,
  idempotencyKey: string,
): Promise<Authorship> {
  return unwrap(
    await api.PUT("/api/v1/projects/{project_id}/authorship/user-token", {
      params: {
        path: { project_id: projectId },
        header: headers(idempotencyKey),
      },
      body: { value },
    }),
  );
}

export async function deleteUserToken(
  projectId: string,
  idempotencyKey: string,
): Promise<void> {
  const result = await api.DELETE(
    "/api/v1/projects/{project_id}/authorship/user-token",
    {
      params: {
        path: { project_id: projectId },
        header: headers(idempotencyKey),
      },
    },
  );
  if (result.error !== undefined) {
    unwrap(result);
  }
}

export type DiagnosticReport = components["schemas"]["DiagnosticReport"];
export type DiagnosticCheck = components["schemas"]["DiagnosticCheck"];

export function useDiagnostics(projectId: string) {
  return useQuery({
    queryKey: [QUERY_ROOTS.diagnostics, projectId] as const,
    staleTime: 0,
    refetchOnWindowFocus: false,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/diagnostics", {
          params: { query: { project_id: projectId } },
          signal,
        }),
      ),
  });
}

export function usePermissionRules(projectId: string) {
  return useQuery({
    queryKey: settingsKeys.permissionRules(projectId),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}/permission-rules", {
          params: { path: { project_id: projectId } },
          signal,
        }),
      ),
  });
}

export function usePermissionHistory(
  projectId: string,
  source: AnsweredBy | "all",
) {
  return useInfiniteQuery({
    queryKey: settingsKeys.permissionHistory(projectId, source),
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam, signal }) =>
      unwrap(
        await api.GET("/api/v1/permission-requests", {
          params: {
            query: {
              project_id: projectId,
              limit: HISTORY_PAGE,
              ...(source === "all" ? {} : { source }),
              ...(pageParam === undefined ? {} : { cursor: pageParam }),
            },
          },
          signal,
        }),
      ),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
}

export async function previewRule(
  projectId: string,
  rule: NewRule,
): Promise<RulePreview> {
  return unwrap(
    await api.POST("/api/v1/projects/{project_id}/permission-rules/preview", {
      params: { path: { project_id: projectId } },
      body: rule,
    }),
  );
}

export async function addRule(
  projectId: string,
  rule: NewRule,
  idempotencyKey: string,
): Promise<PermissionRule> {
  return unwrap(
    await api.POST("/api/v1/projects/{project_id}/permission-rules", {
      params: {
        path: { project_id: projectId },
        header: headers(idempotencyKey),
      },
      body: rule,
    }),
  );
}

export async function revokeRule(
  ruleId: string,
  reason: string,
  idempotencyKey: string,
): Promise<PermissionRule> {
  return unwrap(
    await api.POST("/api/v1/permission-rules/{rule_id}/revoke", {
      params: {
        path: { rule_id: ruleId },
        header: headers(idempotencyKey),
      },
      body: { reason },
    }),
  );
}
