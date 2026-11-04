import { QueryClient } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";

import { fakeApi } from "@/test/fake-api";
import type { Recorded } from "@/test/fake-api";
import { fakeStream } from "@/test/fake-stream";

import { EVENT_KINDS, startEventStream, timelineKey } from "./events";
import type { Activity, TimelineData } from "./events";

describe("startEventStream", () => {
  it("listens to every event kind on one stream", () => {
    const fake = fakeStream();
    const open = vi.fn(() => fake.stream);
    startEventStream(new QueryClient(), open);
    expect(open).toHaveBeenCalledWith("/api/v1/events");
    expect([...fake.listeners.keys()]).toEqual([
      ...EVENT_KINDS.map((k) => `${k}.v1`),
      "open",
    ]);
    for (const kind of [
      "spec.spec.withdrawn",
      "delivery.work_item.repinned",
      "delivery.work_item.superseded",
      "runtime.session.reattached",
      "runtime.session.steered",
      "runtime.session.interrupt_requested",
      "runtime.session.stop_requested",
      "runtime.session.activity",
      "runtime.permission_rule.added",
      "runtime.permission_rule.revoked",
      "runtime.budget.warned",
      "runtime.quota.observed",
      "spec.draft.requested",
      "spec.draft.linted",
      "spec.draft.succeeded",
      "spec.draft.failed",
    ]) {
      expect(fake.listeners.has(`${kind}.v1`)).toBe(true);
    }
  });

  it("invalidates the queries of the event's stream", async () => {
    const fake = fakeStream();
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    const stop = startEventStream(client, () => fake.stream);
    fake.emit("delivery.work_item.blocked.v1", {
      seq: 7,
      stream_type: "work_item",
    });
    expect(invalidate.mock.calls.map(([filters]) => filters?.queryKey)).toEqual(
      [["work-items"], ["specs"]],
    );
    invalidate.mockClear();
    fake.emit("runtime.session.state_changed.v1", {
      seq: 7,
      stream_type: "session",
    });
    expect(invalidate.mock.calls.map(([filters]) => filters?.queryKey)).toEqual(
      [["work-items"], ["sessions"], ["spec-drafts"]],
    );
    invalidate.mockClear();
    fake.emit("spec.draft.succeeded.v1", {
      seq: 7,
      stream_type: "spec_draft",
    });
    expect(invalidate.mock.calls.map(([filters]) => filters?.queryKey)).toEqual(
      [["spec-drafts"], ["specs"]],
    );
    invalidate.mockClear();
    fake.emit("runtime.permission.requested.v1", {
      seq: 8,
      stream_type: "permission_request",
    });
    expect(invalidate.mock.calls.map(([filters]) => filters?.queryKey)).toEqual(
      [["permission-requests"], ["work-items"]],
    );
    invalidate.mockClear();
    fake.emit("engine.job.exhausted.v1", { seq: 9, stream_type: "job" });
    expect(invalidate).not.toHaveBeenCalled();

    const queryFn = vi.fn(() => Promise.resolve([]));
    await client.query({ queryKey: ["work-items", "p"], queryFn });
    const observer = client
      .getQueryCache()
      .find({ queryKey: ["work-items", "p"] });
    expect(observer?.state.isInvalidated).toBe(false);
    fake.emit("delivery.work_item.done.v1", {
      seq: 10,
      stream_type: "work_item",
    });
    expect(
      client.getQueryCache().find({ queryKey: ["work-items", "p"] })?.state
        .isInvalidated,
    ).toBe(true);
    stop();
    expect(fake.closed()).toBe(true);
  });
});

const SESSION = "0193f0c4-1b2a-7c3d-9e8f-0011223344e1";
const OWNER = { kind: "stage_run", work_item_id: "w", stage_run_id: "s" };
const TIMELINE = `/api/v1/sessions/${SESSION}/timeline`;

function row(offset: number): Activity {
  return {
    offset,
    at: "2026-10-31T10:00:00.000Z",
    kind: "message",
    summary: `row ${offset}`,
    data: {},
  };
}

function liveServer(total: () => number) {
  return fakeApi({
    [TIMELINE]: async (request: Recorded) => {
      await new Promise((resolve) => setTimeout(resolve, 5));
      const after = Number(request.search.get("after") ?? "-1");
      const end = total();
      const items = [];
      for (let o = after + 1; o < end; o++) {
        items.push(row(o));
      }
      return {
        body: {
          items,
          next_after: null,
          prev_before: after >= 0 ? after + 1 : null,
        },
      };
    },
  });
}

function seeded(): QueryClient {
  const client = new QueryClient();
  client.setQueryData<TimelineData>(timelineKey(SESSION), {
    items: [row(0), row(1), row(2)],
    prevBefore: null,
  });
  return client;
}

function offsets(client: QueryClient): number[] {
  return (
    client.getQueryData<TimelineData>(timelineKey(SESSION))?.items ?? []
  ).map((a) => a.offset);
}

describe("live timeline", () => {
  it("reads new activities at most once at a time and keeps each once", async () => {
    let total = 3;
    const { requests } = liveServer(() => total);
    const client = seeded();
    const fake = fakeStream();
    startEventStream(client, () => fake.stream);
    for (let o = 3; o < 8; o++) {
      total = o + 1;
      fake.emit("runtime.session.activity.v1", {
        session_id: SESSION,
        owner: OWNER,
        offset: o,
        kind: "message",
      });
    }
    await vi.waitFor(() => {
      expect(offsets(client)).toEqual([0, 1, 2, 3, 4, 5, 6, 7]);
    });
    const forward = requests.filter((r) => r.search.has("after"));
    expect(forward.length).toBeLessThanOrEqual(2);
    expect(forward[0]?.search.get("after")).toBe("2");
  });

  it("ignores activities of a session that is not open", async () => {
    const { requests } = liveServer(() => 9);
    const client = seeded();
    const fake = fakeStream();
    startEventStream(client, () => fake.stream);
    fake.emit("runtime.session.activity.v1", {
      session_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344e2",
      owner: OWNER,
      offset: 9,
      kind: "message",
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(requests).toHaveLength(0);
  });

  it("catches up once after the stream reconnects", async () => {
    const { requests } = liveServer(() => 5);
    const client = seeded();
    const fake = fakeStream();
    startEventStream(client, () => fake.stream);
    fake.emit("open", {});
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(requests).toHaveLength(0);
    fake.emit("open", {});
    await vi.waitFor(() => {
      expect(offsets(client)).toEqual([0, 1, 2, 3, 4]);
    });
    expect(requests.map((r) => r.search.get("after"))).toEqual(["2"]);
  });
});

describe("settings events", () => {
  it("refreshes secrets and authorship for project events, and account links for link events", () => {
    const fake = fakeStream();
    const client = new QueryClient();
    const invalidate = vi.spyOn(client, "invalidateQueries");
    startEventStream(client, () => fake.stream);
    const roots = () =>
      invalidate.mock.calls.map(([filters]) => filters?.queryKey);
    fake.emit("workspace.secret.set.v1", { seq: 1, stream_type: "project" });
    expect(roots()).toEqual([
      ["projects"],
      ["docs"],
      ["secrets"],
      ["authorship"],
    ]);
    invalidate.mockClear();
    fake.emit("scm.user_link.linked.v1", { seq: 2, stream_type: "user_link" });
    expect(roots()).toEqual([["user-links"], ["authorship"]]);
    invalidate.mockClear();
    fake.emit("scm.user_link.authorization_ended.v1", {
      seq: 3,
      stream_type: "user_link_authorization",
    });
    expect(roots()).toEqual([["user-links"]]);
  });
});
