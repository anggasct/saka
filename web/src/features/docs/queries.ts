import { useQuery } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { components } from "@/api/schema";
import type { Locale } from "@/i18n/messages";

export type FileTree = components["schemas"]["FileTree"];
export type FileContent = components["schemas"]["FileContent"];
export type FileWrite = components["schemas"]["FileWrite"];
export type Spec = components["schemas"]["Spec"];
export type WorkItem = components["schemas"]["WorkItem"];

const LIMIT = 200;

export const docsKeys = {
  tree: (projectId: string) => [QUERY_ROOTS.docs, projectId, "tree"] as const,
  file: (projectId: string, path: string) =>
    [QUERY_ROOTS.docs, projectId, "file", path] as const,
  specByPath: (projectId: string, path: string) =>
    [QUERY_ROOTS.specs, "by-path", projectId, path] as const,
  specTasks: (specId: string) =>
    [QUERY_ROOTS.workItems, "spec", specId] as const,
  template: (locale: Locale) =>
    [QUERY_ROOTS.specs, "template", locale] as const,
};

export function useDocTree(projectId: string) {
  return useQuery({
    queryKey: docsKeys.tree(projectId),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}/files", {
          params: { path: { project_id: projectId } },
          signal,
        }),
      ),
  });
}

export async function fetchDocFile(
  projectId: string,
  path: string,
  signal?: AbortSignal,
): Promise<FileContent> {
  return unwrap(
    await api.GET("/api/v1/projects/{project_id}/file", {
      params: { path: { project_id: projectId }, query: { path } },
      ...(signal === undefined ? {} : { signal }),
    }),
  );
}

export function useDocFile(projectId: string, path: string, enabled: boolean) {
  return useQuery({
    queryKey: docsKeys.file(projectId, path),
    enabled,
    retry: false,
    queryFn: ({ signal }) => fetchDocFile(projectId, path, signal),
  });
}

export function useSpecByPath(
  projectId: string,
  path: string,
  enabled: boolean,
) {
  return useQuery({
    queryKey: docsKeys.specByPath(projectId, path),
    enabled,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}/specs", {
          params: {
            path: { project_id: projectId },
            query: { path, limit: 1 },
          },
          signal,
        }),
      ).items[0] ?? null,
  });
}

export function useSpecTasks(specId: string | undefined) {
  return useQuery({
    queryKey: docsKeys.specTasks(specId ?? ""),
    enabled: specId !== undefined,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/work-items", {
          params: { query: { spec_id: specId ?? "", limit: LIMIT } },
          signal,
        }),
      ).items,
  });
}

export async function fetchTemplate(locale: Locale): Promise<string> {
  return unwrap(
    await api.GET("/api/v1/spec-template", {
      params: { query: { locale } },
    }),
  ).content;
}

export type SaveRequest = {
  projectId: string;
  path: string;
  content: string;
  blob: string | null;
  idempotencyKey: string;
};

export async function saveFile(request: SaveRequest): Promise<FileWrite> {
  const precondition =
    request.blob === null
      ? { "If-None-Match": "*" }
      : { "If-Match": `"${request.blob}"` };
  return unwrap(
    await api.PUT("/api/v1/projects/{project_id}/file", {
      params: {
        path: { project_id: request.projectId },
        query: { path: request.path },
        header: { "Idempotency-Key": request.idempotencyKey, ...precondition },
      },
      body: { content: request.content },
    }),
  );
}
