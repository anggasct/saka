import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { toConnectionError } from "./connection-error";
import { fetchEventStream } from "./sse";
import { webPlatform } from "./web";
import type {
  Connections,
  EventStream,
  NewConnection,
  Notice,
  Platform,
  SavedConnection,
} from "./index";

type Active = {
  id: string;
  url: string;
  token: string | null;
  kind: "remote" | "local";
};
type Listing = { connections: SavedConnection[]; active: Active | null };

const NAVIGATE_EVENT = "saka://navigate";
const EMPTY: Listing = { connections: [], active: null };

let listing: Listing = EMPTY;
let startFailure: string | null = null;

async function call<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw toConnectionError(error);
  }
}

function localIsDown(): boolean {
  const local = listing.connections.find((c) => c.kind === "local");
  return listing.active?.kind === "local" && local?.running !== true;
}

async function startLocalIfActive(): Promise<void> {
  if (!localIsDown()) {
    return;
  }
  try {
    await call<undefined>("local_start");
    listing = await call<Listing>("connections_list");
  } catch (error) {
    startFailure = error instanceof Error ? error.message : "unknown";
  }
}

const connections: Connections = {
  hasActive: () =>
    listing.active !== null &&
    (listing.active.kind === "remote" || !localIsDown()),
  startError: () => startFailure,
  localStart: async () => {
    startFailure = null;
    await call<undefined>("local_start");
    listing = await call<Listing>("connections_list");
  },
  localStop: async () => {
    await call<undefined>("local_stop");
    listing = await call<Listing>("connections_list");
  },
  list: async () => {
    listing = await call<Listing>("connections_list");
    return listing.connections;
  },
  add: (input: NewConnection) =>
    call<SavedConnection>("connection_add", {
      name: input.name,
      url: input.url,
      pairing_code: input.pairingCode,
    }),
  remove: (id) => call<undefined>("connection_remove", { id }),
  select: (id) => call<undefined>("connection_select", { id }),
};

export const tauriPlatform: Platform = {
  ...webPlatform,
  ready: async () => {
    listing = await call<Listing>("connections_list");
    await startLocalIfActive();
  },
  info: () => ({ kind: "desktop", os: navigator.platform }),
  connections: () => connections,
  apiBaseUrl: () => listing.active?.url ?? webPlatform.apiBaseUrl(),
  apiToken: () => listing.active?.token ?? null,
  eventSource: (url: string): EventStream =>
    fetchEventStream(url, {
      baseUrl: tauriPlatform.apiBaseUrl(),
      token: tauriPlatform.apiToken,
    }),
  notify: ({ title, body, route }: Notice) => {
    void invoke("notify", { title, body, route }).catch(() => undefined);
  },
  onNavigate: (handler) => {
    let stop: (() => void) | null = null;
    let cancelled = false;
    const subscribe = async () => {
      const unlisten = await listen<string>(NAVIGATE_EVENT, (event) => {
        handler(event.payload);
      });
      if (cancelled) {
        unlisten();
      } else {
        stop = unlisten;
      }
    };
    void subscribe();
    return () => {
      cancelled = true;
      stop?.();
    };
  },
  registerServiceWorker: () => undefined,
};
