import type { EventStream } from "@/platform";

export function fakeStream() {
  const listeners = new Map<
    string,
    ((event: MessageEvent<string>) => void)[]
  >();
  let closed = false;
  const stream: EventStream = {
    addEventListener: (type, listener) => {
      listeners.set(type, [...(listeners.get(type) ?? []), listener]);
    },
    close: () => {
      closed = true;
    },
  };
  const emit = (type: string, data: unknown) => {
    for (const listener of listeners.get(type) ?? []) {
      listener(new MessageEvent(type, { data: JSON.stringify(data) }));
    }
  };
  return { stream, emit, listeners, closed: () => closed };
}
