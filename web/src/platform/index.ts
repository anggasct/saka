import { webPlatform } from "./web";

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

export type Platform = {
  apiBaseUrl: () => string;
  preferredLocales: () => readonly string[];
  eventSource: (url: string) => EventStream;
  rootElement: () => HTMLElement | null;
  preferences: Preferences;
  setDocumentLanguage: (locale: string) => void;
  observeVisible: (element: Element, onVisible: () => void) => () => void;
};

export const platform: Platform = webPlatform;
