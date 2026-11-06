import type { EventStream } from "./index";

type Listener = (event: MessageEvent<string>) => void;

export type SseMessage = { event: string; data: string; id: string };

export type SseOptions = {
  baseUrl: string;
  token: () => string | null;
  delayMs?: (attempt: number) => number;
};

const MAX_DELAY_MS = 30_000;

export function backoff(attempt: number): number {
  return Math.min(MAX_DELAY_MS, 1_000 * 2 ** attempt);
}

export function sseParser(
  onMessage: (message: SseMessage) => void,
  initialId = "",
) {
  let buffer = "";
  let skipLf = false;
  let event = "";
  let data: string[] = [];
  let id = initialId;

  const line = (text: string) => {
    if (text === "") {
      if (data.length > 0) {
        onMessage({
          event: event === "" ? "message" : event,
          data: data.join("\n"),
          id,
        });
      }
      event = "";
      data = [];
      return;
    }
    if (text.startsWith(":")) {
      return;
    }
    const colon = text.indexOf(":");
    const field = colon < 0 ? text : text.slice(0, colon);
    const raw = colon < 0 ? "" : text.slice(colon + 1);
    const value = raw.startsWith(" ") ? raw.slice(1) : raw;
    if (field === "event") {
      event = value;
    } else if (field === "data") {
      data.push(value);
    } else if (field === "id" && !value.includes("\0")) {
      id = value;
    }
  };

  return {
    feed: (chunk: string) => {
      buffer += skipLf && chunk.startsWith("\n") ? chunk.slice(1) : chunk;
      skipLf = false;
      const lines = buffer.split(/\r\n|\r|\n/);
      buffer = lines.pop() ?? "";
      skipLf = buffer === "" && chunk.endsWith("\r");
      for (const text of lines) {
        line(text);
      }
    },
    lastId: () => id,
  };
}

function retryable(status: number): boolean {
  return status >= 500 || status === 408 || status === 429;
}

function wait(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        resolve();
      },
      { once: true },
    );
  });
}

export function fetchEventStream(
  path: string,
  options: SseOptions,
): EventStream {
  const listeners = new Map<string, Listener[]>();
  const abort = new AbortController();
  const delay = options.delayMs ?? backoff;
  let lastId = "";

  const emit = (type: string, data: string, lastEventId: string) => {
    for (const listener of listeners.get(type) ?? []) {
      listener(new MessageEvent(type, { data, lastEventId }));
    }
  };

  const connect = async (): Promise<"ended" | "refused"> => {
    const headers = new Headers({ Accept: "text/event-stream" });
    const token = options.token();
    if (token !== null) {
      headers.set("Authorization", `Bearer ${token}`);
    }
    if (lastId !== "") {
      headers.set("Last-Event-ID", lastId);
    }
    const response = await globalThis.fetch(new URL(path, options.baseUrl), {
      headers,
      cache: "no-store",
      signal: abort.signal,
    });
    if (!response.ok || response.body === null) {
      if (retryable(response.status)) {
        throw new Error(`event stream answered ${String(response.status)}`);
      }
      return "refused";
    }
    emit("open", "", lastId);
    const parser = sseParser((message) => {
      emit(message.event, message.data, message.id);
    }, lastId);
    const decoder = new TextDecoder();
    const reader = response.body.getReader();
    for (;;) {
      const { done, value } = await reader.read();
      if (done) {
        return "ended";
      }
      parser.feed(decoder.decode(value, { stream: true }));
      lastId = parser.lastId();
    }
  };

  const run = async () => {
    let failures = 0;
    while (!abort.signal.aborted) {
      try {
        if ((await connect()) === "refused") {
          return;
        }
        failures = 0;
      } catch {
        failures += 1;
      }
      await wait(delay(failures), abort.signal);
    }
  };
  void run();

  return {
    addEventListener: (type, listener) => {
      listeners.set(type, [...(listeners.get(type) ?? []), listener]);
    },
    close: () => {
      abort.abort();
    },
  };
}
