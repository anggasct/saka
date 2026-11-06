import { webCredentials } from "./credentials";
import type { EventStream, Platform } from "./index";

export const webPlatform: Platform = {
  apiBaseUrl: () => location.origin,
  preferredLocales: () =>
    navigator.languages.length > 0 ? navigator.languages : [navigator.language],
  eventSource: (url: string): EventStream => new EventSource(url),
  rootElement: () => document.getElementById("root"),
  preferences: {
    get: (key) => {
      try {
        return localStorage.getItem(key);
      } catch {
        return null;
      }
    },
    set: (key, value) => {
      try {
        localStorage.setItem(key, value);
      } catch {
        return;
      }
    },
  },
  setDocumentLanguage: (locale) => {
    document.documentElement.lang = locale;
  },
  copyText: async (text) => {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      return false;
    }
  },
  credentials: webCredentials,
  navigateTo: (path) => {
    location.assign(path);
  },
  observeVisible: (element, onVisible) => {
    if (typeof IntersectionObserver === "undefined") {
      return () => undefined;
    }
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        onVisible();
      }
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
    };
  },
};
