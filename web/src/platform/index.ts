import { tauriPlatform } from "./tauri";
import { webPlatform } from "./web";
import { isTauri } from "@tauri-apps/api/core";

export type EventStream = {
  addEventListener: (
    type: string,
    listener: (event: MessageEvent<string>) => void,
  ) => void;
  close: () => void;
};

export type Preferences = {
  get: (key: string) => string | null;
  set: (key: string, value: string) => void;
};

export type CredentialResult =
  | { status: "ok"; credential: Record<string, unknown> }
  | { status: "cancelled" | "unsupported" | "duplicate" | "failed" };

export type Credentials = {
  supported: () => boolean;
  create: (options: unknown) => Promise<CredentialResult>;
  get: (options: unknown) => Promise<CredentialResult>;
};

export type Connection = {
  online: () => boolean;
  subscribe: (listener: () => void) => () => void;
};

export type PlatformInfo = {
  kind: "web" | "desktop";
  os: string;
};

export type Notice = {
  title: string;
  body: string;
  route: string;
};

export type SavedConnection = {
  id: string;
  name: string;
  url: string;
  active: boolean;
};

export type NewConnection = {
  name: string;
  url: string;
  pairingCode: string;
};

export type Connections = {
  hasActive: () => boolean;
  list: () => Promise<SavedConnection[]>;
  add: (input: NewConnection) => Promise<SavedConnection>;
  remove: (id: string) => Promise<void>;
  select: (id: string) => Promise<void>;
};

export type Platform = {
  connections: () => Connections | null;
  ready: () => Promise<void>;
  info: () => PlatformInfo;
  apiBaseUrl: () => string;
  apiToken: () => string | null;
  notify: (notice: Notice) => void;
  preferredLocales: () => readonly string[];
  eventSource: (url: string) => EventStream;
  rootElement: () => HTMLElement | null;
  preferences: Preferences;
  setDocumentLanguage: (locale: string) => void;
  copyText: (text: string) => Promise<boolean>;
  observeVisible: (element: Element, onVisible: () => void) => () => void;
  credentials: Credentials;
  connection: Connection;
  registerServiceWorker: (url: string) => void;
  navigateTo: (path: string) => void;
};

export const platform: Platform = isTauri() ? tauriPlatform : webPlatform;
