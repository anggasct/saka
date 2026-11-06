import { useInfiniteQuery, useQuery } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import { fetchTail, QUERY_ROOTS, timelineKey } from "@/api/events";
import type { components } from "@/api/schema";

export type Session = components["schemas"]["Session"];
export type ActiveSession = components["schemas"]["ActiveSession"];
export type SessionsOf =
  { kind: "work_item"; id: string } | { kind: "spec_draft"; id: string };

const ACTIVE_PAGE = 50;

export const sessionKeys = {
  of: (owner: SessionsOf) =>
    [QUERY_ROOTS.sessions, owner.kind, owner.id] as const,
  one: (id: string) => [QUERY_ROOTS.sessions, "one", id] as const,
  active: () => [QUERY_ROOTS.sessions, "active"] as const,
};

export function useSessions(owner: SessionsOf) {
  return useQuery({
    queryKey: sessionKeys.of(owner),
    queryFn: async ({ signal }) =>
      (
        unwrap(
          await api.GET("/api/v1/sessions", {
            params: {
              query:
                owner.kind === "work_item"
                  ? { work_item_id: owner.id }
                  : { spec_draft_id: owner.id },
            },
            signal,
          }),
        ) as components["schemas"]["SessionList"]
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

export function useSession(id: string) {
  return useQuery({
    queryKey: sessionKeys.one(id),
    queryFn: async ({ signal }) =>
      unwrap(
        await api.GET("/api/v1/sessions/{session_id}", {
          params: { path: { session_id: id } },
          signal,
        }),
      ),
  });
}

export function useActiveSessions() {
  return useInfiniteQuery({
    queryKey: sessionKeys.active(),
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam, signal }) =>
      unwrap(
        await api.GET("/api/v1/sessions", {
          params: {
            query: {
              active: "true",
              limit: ACTIVE_PAGE,
              ...(pageParam === undefined ? {} : { cursor: pageParam }),
            },
          },
          signal,
        }),
      ) as components["schemas"]["ActiveSessionList"],
    getNextPageParam: (last) => last.next_cursor ?? undefined,
  });
}
