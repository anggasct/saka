import { webPlatform } from "./web";

export type EventStream = {
  addEventListener: (
    type: string,
    listener: (event: MessageEvent<string>) => void,
  ) => void;
  close: () => void;
};

export type Platform = {
  apiBaseUrl: () => string;
  preferredLocales: () => readonly string[];
  eventSource: (url: string) => EventStream;
  rootElement: () => HTMLElement | null;
};

export const platform: Platform = webPlatform;
