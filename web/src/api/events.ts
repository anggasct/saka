import type { QueryClient, QueryKey } from "@tanstack/react-query";

import { platform } from "@/platform";
import type { EventStream } from "@/platform";

export const EVENT_KINDS = [
  "workspace.repo.registered",
  "workspace.project.registered",
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
} as const;

const AFFECTS: Record<string, readonly string[]> = {
  repo: [QUERY_ROOTS.projects],
  project: [QUERY_ROOTS.projects],
  spec: [QUERY_ROOTS.specs],
  work_item: [QUERY_ROOTS.workItems, QUERY_ROOTS.specs],
  session: [QUERY_ROOTS.workItems],
  permission_request: [QUERY_ROOTS.permissions, QUERY_ROOTS.workItems],
};

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
  return () => {
    stream.close();
  };
}
