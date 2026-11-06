import { invoke } from "@tauri-apps/api/core";

import { toConnectionError } from "./connection-error";
import { fetchEventStream } from "./sse";
import { webPlatform } from "./web";
import type {
  Connections,
  EventStream,
  NewConnection,
  Platform,
  SavedConnection,
} from "./index";

type Active = { id: string; url: string; token: string | null };
type Listing = { connections: SavedConnection[]; active: Active | null };

const EMPTY: Listing = { connections: [], active: null };

let listing: Listing = EMPTY;

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

const connections: Connections = {
  hasActive: () => listing.active !== null,
  list: async () => (await call<Listing>("connections_list")).connections,
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
  registerServiceWorker: () => undefined,
};
