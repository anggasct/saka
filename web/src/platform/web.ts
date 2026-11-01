import type { EventStream, Platform } from "./index";

export const webPlatform: Platform = {
  apiBaseUrl: () => location.origin,
  preferredLocales: () =>
    navigator.languages.length > 0 ? navigator.languages : [navigator.language],
  eventSource: (url: string): EventStream => new EventSource(url),
  rootElement: () => document.getElementById("root"),
};
