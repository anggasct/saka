import { afterEach, describe, expect, it, vi } from "vitest";

import { backoff, fetchEventStream, sseParser } from "./sse";
import type { SseMessage } from "./sse";

function parse(chunks: string[]): SseMessage[] {
  const out: SseMessage[] = [];
  const parser = sseParser((m) => out.push(m));
  for (const chunk of chunks) {
    parser.feed(chunk);
  }
  return out;
}

describe("sseParser", () => {
  it("reads id, event and data fields and ignores comments", () => {
    expect(
      parse([': keep-alive\n\nid: 7\nevent: task.v1\ndata: {"a":1}\n\n']),
    ).toEqual([{ event: "task.v1", data: '{"a":1}', id: "7" }]);
  });

  it("joins data lines and defaults the type to message", () => {
    expect(parse(["data: one\ndata: two\n\n"])).toEqual([
      { event: "message", data: "one\ntwo", id: "" },
    ]);
  });

  it("copes with chunks that split a line, a field and a CRLF", () => {
    expect(
      parse([
        "id: 1\r",
        "\nevent: a.v1\r\nda",
        "ta: x\r\n",
        "\r\nid: 2\ndata: y\n\n",
      ]),
    ).toEqual([
      { event: "a.v1", data: "x", id: "1" },
      { event: "message", data: "y", id: "2" },
    ]);
  });

  it("drops an event without data", () => {
    expect(parse(["event: x\n\n"])).toEqual([]);
  });

  it("keeps the last id across events", () => {
    const parser = sseParser(() => undefined);
    parser.feed("id: 9\ndata: x\n\ndata: y\n\n");
    expect(parser.lastId()).toBe("9");
  });
});

function body(parts: string[]): Response {
  const encoder = new TextEncoder();
  return new Response(
    new ReadableStream({
      start(controller) {
        for (const part of parts) {
          controller.enqueue(encoder.encode(part));
        }
        controller.close();
      },
    }),
    { status: 200, headers: { "content-type": "text/event-stream" } },
  );
}

function listen(stream: ReturnType<typeof fetchEventStream>, type: string) {
  const seen: { data: string; id: string }[] = [];
  stream.addEventListener(type, (e) => {
    seen.push({ data: e.data, id: e.lastEventId });
  });
  return seen;
}

describe("fetchEventStream", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("sends the bearer token and delivers events to their listeners", async () => {
    const fetchMock = vi.fn(() =>
      Promise.resolve(body(["id: 3\nevent: a.v1\ndata: hello\n\n"])),
    );
    vi.stubGlobal("fetch", fetchMock);
    const stream = fetchEventStream("/api/v1/events", {
      baseUrl: "https://saka.example",
      token: () => "sdt_abc",
      delayMs: () => 60_000,
    });
    const seen = listen(stream, "a.v1");
    await vi.waitFor(() => {
      expect(seen).toEqual([{ data: "hello", id: "3" }]);
    });
    const [url, init] = fetchMock.mock.calls[0] as unknown as [
      URL,
      RequestInit,
    ];
    expect(url.href).toBe("https://saka.example/api/v1/events");
    expect(new Headers(init.headers).get("Authorization")).toBe(
      "Bearer sdt_abc",
    );
    expect(new Headers(init.headers).get("Last-Event-ID")).toBeNull();
    stream.close();
  });

  it("sends no Authorization header without a token", async () => {
    const fetchMock = vi.fn(() => Promise.resolve(body([])));
    vi.stubGlobal("fetch", fetchMock);
    const stream = fetchEventStream("/api/v1/events", {
      baseUrl: "http://127.0.0.1:7420",
      token: () => null,
      delayMs: () => 60_000,
    });
    await vi.waitFor(() => {
      expect(fetchMock).toHaveBeenCalled();
    });
    const init = (fetchMock.mock.calls[0] as unknown as [URL, RequestInit])[1];
    expect(new Headers(init.headers).has("Authorization")).toBe(false);
    stream.close();
  });

  it("reconnects with Last-Event-ID and announces each connection", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(body(["id: 5\ndata: a\n\n"]))
      .mockResolvedValueOnce(body(["id: 6\ndata: b\n\n"]))
      .mockReturnValue(new Promise(() => undefined));
    vi.stubGlobal("fetch", fetchMock);
    const stream = fetchEventStream("/api/v1/events", {
      baseUrl: "http://127.0.0.1:7420",
      token: () => "t",
      delayMs: () => 0,
    });
    const opens = listen(stream, "open");
    const messages = listen(stream, "message");
    await vi.waitFor(() => {
      expect(messages.map((m) => m.data)).toEqual(["a", "b"]);
    });
    expect(opens).toHaveLength(2);
    const second = fetchMock.mock.calls[1] as unknown as [URL, RequestInit];
    expect(new Headers(second[1].headers).get("Last-Event-ID")).toBe("5");
    stream.close();
  });

  it("retries after a server error but stops on a refusal", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(new Response(null, { status: 503 }))
      .mockResolvedValueOnce(new Response(null, { status: 401 }));
    vi.stubGlobal("fetch", fetchMock);
    const stream = fetchEventStream("/api/v1/events", {
      baseUrl: "http://127.0.0.1:7420",
      token: () => "t",
      delayMs: () => 0,
    });
    await vi.waitFor(() => {
      expect(fetchMock).toHaveBeenCalledTimes(2);
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(fetchMock).toHaveBeenCalledTimes(2);
    stream.close();
  });

  it("stops for good when closed", async () => {
    const fetchMock = vi.fn(() => Promise.resolve(body([])));
    vi.stubGlobal("fetch", fetchMock);
    const stream = fetchEventStream("/api/v1/events", {
      baseUrl: "http://127.0.0.1:7420",
      token: () => null,
      delayMs: () => 5,
    });
    await vi.waitFor(() => {
      expect(fetchMock).toHaveBeenCalled();
    });
    stream.close();
    const calls = fetchMock.mock.calls.length;
    await new Promise((resolve) => setTimeout(resolve, 40));
    expect(fetchMock.mock.calls.length).toBeLessThanOrEqual(calls + 1);
  });
});

describe("backoff", () => {
  it("doubles from one second up to thirty", () => {
    expect([0, 1, 2, 5, 9].map(backoff)).toEqual([
      1000, 2000, 4000, 30000, 30000,
    ]);
  });
});
