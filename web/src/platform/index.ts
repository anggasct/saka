// What the app needs from where it runs (browser now, desktop shell later). Feature code
// imports `platform` from here and never touches browser globals directly.

import { webPlatform } from "./web";

/** The subset of `EventSource` the app uses. */
export type EventStream = {
  addEventListener: (
    type: string,
    listener: (event: MessageEvent<string>) => void,
  ) => void;
  close: () => void;
};

export type Platform = {
  /** Origin of the API (the page's own origin when served by Saka). */
  apiBaseUrl: () => string;
  /** The user's languages, most preferred first. */
  preferredLocales: () => readonly string[];
  /** Opens a server-sent event stream. */
  eventSource: (url: string) => EventStream;
  /** The element the app renders into. */
  rootElement: () => HTMLElement | null;
};

export const platform: Platform = webPlatform;
