import {
  keepPreviousData,
  useInfiniteQuery,
  useQuery,
} from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { components } from "@/api/schema";

export type Incident = components["schemas"]["Incident"];
export type IncidentDetail = components["schemas"]["IncidentDetail"];
export type IncidentSuspect = components["schemas"]["IncidentSuspect"];
export type IncidentSource = components["schemas"]["IncidentSource"];

export const INCIDENT_STATES = [
  "open",
  "handling",
  "resolved",
  "dismissed",
] as const;

export const SEVERITIES = [
  "critical",
  "high",
  "medium",
  "low",
  "info",
] as const;

export type IncidentFilters = {
  state?: string | undefined;
  severity?: string | undefined;
  environment?: string | undefined;
};

const PAGE = 30;

const incidentKeys = {
  sources: (projectId: string) =>
    [QUERY_ROOTS.incidents, "sources", projectId] as const,
  list: (projectId: string, filters: IncidentFilters) =>
    [
      QUERY_ROOTS.incidents,
      "list",
      projectId,
      filters.state ?? "",
      filters.severity ?? "",
      filters.environment ?? "",
    ] as const,
  detail: (id: string) => [QUERY_ROOTS.incidents, "detail", id] as const,
};

function headers(idempotencyKey: string) {
  return { "Idempotency-Key": idempotencyKey };
}

export function useIncidentSources(projectId: string) {
  return useQuery({
    queryKey: incidentKeys.sources(projectId),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}/incident-sources", {
          params: { path: { project_id: projectId } },
          signal,
        }),
      ).items,
  });
}

export function useIncidents(projectId: string, filters: IncidentFilters) {
  return useInfiniteQuery({
    queryKey: incidentKeys.list(projectId, filters),
    initialPageParam: undefined as string | undefined,
    placeholderData: keepPreviousData,
    queryFn: async ({ pageParam, signal }) =>
      unwrap(
        await api.GET("/api/v1/projects/{project_id}/incidents", {
          params: {
            path: { project_id: projectId },
            query: {
              limit: PAGE,
              ...(filters.state === undefined ? {} : { state: filters.state }),
              ...(filters.severity === undefined
                ? {}
                : { severity: filters.severity }),
              ...(filters.environment === undefined
                ? {}
                : { environment: filters.environment }),
              ...(pageParam === undefined ? {} : { cursor: pageParam }),
            },
          },
          signal,
        }),
      ),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
}

export function useIncident(id: string | undefined) {
  return useQuery({
    queryKey: incidentKeys.detail(id ?? ""),
    enabled: id !== undefined,
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/incidents/{incident_id}", {
          params: { path: { incident_id: id ?? "" } },
          signal,
        }),
      ),
  });
}

export async function draftIncidentFix(
  incidentId: string,
  note: string | null,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/incidents/{incident_id}/actions/draft-fix", {
      params: {
        path: { incident_id: incidentId },
        header: headers(idempotencyKey),
      },
      body: { note },
    }),
  );
}

export async function revertIncident(
  incidentId: string,
  workItemId: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/incidents/{incident_id}/actions/revert", {
      params: {
        path: { incident_id: incidentId },
        header: headers(idempotencyKey),
      },
      body: { work_item_id: workItemId, reason },
    }),
  );
}

export async function dismissIncident(
  incidentId: string,
  reason: string,
  idempotencyKey: string,
): Promise<void> {
  unwrap(
    await api.POST("/api/v1/incidents/{incident_id}/actions/dismiss", {
      params: {
        path: { incident_id: incidentId },
        header: headers(idempotencyKey),
      },
      body: { reason },
    }),
  );
}
