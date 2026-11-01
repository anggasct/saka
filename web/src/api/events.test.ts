import { QueryClient } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";

import type { EventStream } from "@/platform";

import { EVENT_KINDS, startEventStream } from "./events";

function fakeStream() {
  const listeners = new Map<string, (event: MessageEvent<string>) => void>();
  let closed = false;
  const stream: EventStream = {
    addEventListener: (type, listener) => {
      listeners.set(type, listener);
    },
    close: () => {
      closed = true;
    },
  };
  const emit = (type: string, data: unknown) => {
    listeners.get(type)?.(
      new MessageEvent(type, { data: JSON.stringify(data) }),
    );
  };
  return { stream, emit, listeners, closed: () => closed };
}

describe("startEventStream", () => {
  it("listens to every event kind on one stream", () => {
    const fake = fakeStream();
    const open = vi.fn(() => fake.stream);
    startEventStream(new QueryClient(), open);
    expect(open).toHaveBeenCalledWith("/api/v1/events");
    expect([...fake.listeners.keys()]).toEqual(
      EVENT_KINDS.map((k) => `${k}.v1`),
    );
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

    // A refetch follows for an active query.
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
