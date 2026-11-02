import type { QueryClient, QueryKey } from "@tanstack/react-query";

import { platform } from "@/platform";
import type { EventStream } from "@/platform";

import { api, unwrap } from "./client";
import type { components } from "./schema";

export const EVENT_KINDS = [
  "workspace.repo.registered",
  "workspace.project.registered",
  "workspace.doc.committed",
  "spec.spec.registered",
  "spec.revision.recorded",
  "spec.spec.approved",
  "spec.spec.withdrawn",
  "spec.spec.shipped",
  "delivery.work_item.created",
  "delivery.work_item.repinned",
  "delivery.work_item.superseded",
  "delivery.work_item.admitted",
  "delivery.stage_run.created",
  "delivery.stage_run.phase_changed",
  "delivery.stage_run.succeeded",
  "delivery.gate.started",
  "delivery.gate.observed",
  "delivery.merge.requested",
  "delivery.work_item.blocked",
  "delivery.blocker.resolved",
  "delivery.work_item.cancelled",
  "delivery.work_item.done",
  "runtime.environment.prepared",
  "runtime.environment.torn_down",
  "runtime.environment.teardown_failed",
  "runtime.session.started",
  "runtime.session.reattached",
  "runtime.session.steered",
  "runtime.session.interrupt_requested",
  "runtime.session.stop_requested",
  "runtime.session.activity",
  "runtime.session.initialized",
  "runtime.session.state_changed",
  "runtime.session.turn_requested",
  "runtime.session.turn_ended",
  "runtime.session.ended",
  "runtime.session.lost",
  "runtime.permission.requested",
  "runtime.permission.answered",
  "runtime.permission.delivered",
  "runtime.permission_rule.added",
  "runtime.permission_rule.revoked",
  "runtime.budget.warned",
  "runtime.quota.observed",
  "scm.branch.pushed",
  "scm.change_request.opened",
  "scm.change_request.updated",
  "scm.change_request.merged",
  "scm.change_request.closed",
  "engine.job.exhausted",
] as const;

export const QUERY_ROOTS = {
  projects: "projects",
  specs: "specs",
  workItems: "work-items",
  permissions: "permission-requests",
  sessions: "sessions",
  timeline: "timeline",
  docs: "docs",
} as const;

const AFFECTS: Record<string, readonly string[]> = {
  repo: [QUERY_ROOTS.projects],
  project: [QUERY_ROOTS.projects, QUERY_ROOTS.docs],
  spec: [QUERY_ROOTS.specs],
  work_item: [QUERY_ROOTS.workItems, QUERY_ROOTS.specs],
  session: [QUERY_ROOTS.workItems, QUERY_ROOTS.sessions],
  permission_request: [QUERY_ROOTS.permissions, QUERY_ROOTS.workItems],
};

export type Activity = components["schemas"]["SessionActivity"];
export type TimelineData = {
  items: readonly Activity[];
  prevBefore: number | null;
};

const TAIL = 200;
const FORWARD = 500;
const ACTIVITY = "runtime.session.activity.v1";

export function timelineKey(sessionId: string) {
  return [QUERY_ROOTS.timeline, sessionId] as const;
}

function merge(
  first: readonly Activity[],
  second: readonly Activity[],
): Activity[] {
  const byOffset = new Map<number, Activity>();
  for (const a of [...first, ...second]) {
    byOffset.set(a.offset, a);
  }
  return [...byOffset.values()].sort((a, b) => a.offset - b.offset);
}

export async function fetchTail(
  sessionId: string,
  signal?: AbortSignal,
): Promise<TimelineData> {
  const page = unwrap(
    await api.GET("/api/v1/sessions/{session_id}/timeline", {
      params: {
        path: { session_id: sessionId },
        query: { tail: "true", limit: TAIL },
      },
      ...(signal === undefined ? {} : { signal }),
    }),
  );
  return { items: page.items, prevBefore: page.prev_before ?? null };
}

export async function fetchOlder(
  queryClient: QueryClient,
  sessionId: string,
): Promise<void> {
  const key = timelineKey(sessionId);
  const before = queryClient.getQueryData<TimelineData>(key)?.prevBefore;
  if (before === null || before === undefined) {
    return;
  }
  const page = unwrap(
    await api.GET("/api/v1/sessions/{session_id}/timeline", {
      params: {
        path: { session_id: sessionId },
        query: { before, limit: TAIL },
      },
    }),
  );
  queryClient.setQueryData<TimelineData>(key, (old) =>
    old === undefined
      ? old
      : {
          items: merge(page.items, old.items),
          prevBefore: page.prev_before ?? null,
        },
  );
}

const following = new Map<string, { again: boolean }>();

async function readForward(
  queryClient: QueryClient,
  sessionId: string,
): Promise<void> {
  const key = timelineKey(sessionId);
  for (;;) {
    const data = queryClient.getQueryData<TimelineData>(key);
    if (data === undefined) {
      return;
    }
    const last = data.items.at(-1)?.offset;
    if (last === undefined) {
      await queryClient.refetchQueries({ queryKey: key, exact: true });
      return;
    }
    const page = unwrap(
      await api.GET("/api/v1/sessions/{session_id}/timeline", {
        params: {
          path: { session_id: sessionId },
          query: { after: last, limit: FORWARD },
        },
      }),
    );
    queryClient.setQueryData<TimelineData>(key, (old) =>
      old === undefined ? old : { ...old, items: merge(old.items, page.items) },
    );
    if (page.next_after === null || page.next_after === undefined) {
      return;
    }
  }
}

export async function catchUp(
  queryClient: QueryClient,
  sessionId: string,
): Promise<void> {
  const running = following.get(sessionId);
  if (running !== undefined) {
    running.again = true;
    return;
  }
  const state = { again: true };
  following.set(sessionId, state);
  try {
    while (state.again) {
      state.again = false;
      await readForward(queryClient, sessionId);
    }
  } catch {
    return;
  } finally {
    following.delete(sessionId);
  }
}

type Notice = { session_id: string; offset: number };

function noticeOf(data: string): Notice | undefined {
  try {
    const parsed: unknown = JSON.parse(data);
    if (
      typeof parsed === "object" &&
      parsed !== null &&
      "session_id" in parsed &&
      "offset" in parsed &&
      typeof parsed.session_id === "string" &&
      typeof parsed.offset === "number"
    ) {
      return { session_id: parsed.session_id, offset: parsed.offset };
    }
  } catch {
    return undefined;
  }
  return undefined;
}

function onActivity(queryClient: QueryClient, data: string): void {
  const notice = noticeOf(data);
  if (notice === undefined) {
    return;
  }
  const cached = queryClient.getQueryData<TimelineData>(
    timelineKey(notice.session_id),
  );
  if (cached === undefined) {
    return;
  }
  const last = cached.items.at(-1)?.offset ?? -1;
  if (notice.offset > last) {
    void catchUp(queryClient, notice.session_id);
  }
}

function catchUpAll(queryClient: QueryClient): void {
  for (const [key] of queryClient.getQueriesData<TimelineData>({
    queryKey: [QUERY_ROOTS.timeline],
  })) {
    const sessionId = key[1];
    if (typeof sessionId === "string") {
      void catchUp(queryClient, sessionId);
    }
  }
}

export function affectedQueries(streamType: string): QueryKey[] {
  return (AFFECTS[streamType] ?? []).map((root) => [root]);
}

function streamTypeOf(data: string): string | undefined {
  try {
    const parsed: unknown = JSON.parse(data);
    if (
      typeof parsed === "object" &&
      parsed !== null &&
      "stream_type" in parsed
    ) {
      const value = parsed.stream_type;
      return typeof value === "string" ? value : undefined;
    }
  } catch {
    return undefined;
  }
  return undefined;
}

export function startEventStream(
  queryClient: QueryClient,
  open: (url: string) => EventStream = platform.eventSource,
): () => void {
  const stream = open("/api/v1/events");
  const handle = (event: MessageEvent<string>) => {
    const streamType = streamTypeOf(event.data);
    if (streamType === undefined) {
      return;
    }
    for (const queryKey of affectedQueries(streamType)) {
      void queryClient.invalidateQueries({ queryKey });
    }
  };
  for (const kind of EVENT_KINDS) {
    stream.addEventListener(`${kind}.v1`, handle);
  }
  stream.addEventListener(ACTIVITY, (event) => {
    onActivity(queryClient, event.data);
  });
  let opened = false;
  stream.addEventListener("open", () => {
    if (opened) {
      catchUpAll(queryClient);
    }
    opened = true;
  });
  return () => {
    stream.close();
  };
}
