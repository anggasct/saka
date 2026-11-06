import { webCredentials } from "./credentials";
import type { EventStream, Platform } from "./index";

export const webPlatform: Platform = {
  connections: () => null,
  onNavigate: () => () => undefined,
  ready: () => Promise.resolve(),
  info: () => ({ kind: "web", os: navigator.platform }),
  apiBaseUrl: () => location.origin,
  apiToken: () => null,
  notify: ({ title, body, route }) => {
    if (
      typeof Notification === "undefined" ||
      Notification.permission !== "granted"
    ) {
      return;
    }
    const notification = new Notification(title, { body });
    notification.onclick = () => {
      window.focus();
      location.assign(route);
    };
  },
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
  connection: {
    online: () => navigator.onLine,
    subscribe: (listener) => {
      window.addEventListener("online", listener);
      window.addEventListener("offline", listener);
      return () => {
        window.removeEventListener("online", listener);
        window.removeEventListener("offline", listener);
      };
    },
  },
  registerServiceWorker: (url) => {
    if ("serviceWorker" in navigator) {
      navigator.serviceWorker.register(url).catch(() => undefined);
    }
  },
  navigateTo: (path) => {
    location.assign(path);
  },
  onVisible: (listener) => {
    const handle = () => {
      if (document.visibilityState === "visible") {
        listener();
      }
    };
    document.addEventListener("visibilitychange", handle);
    return () => {
      document.removeEventListener("visibilitychange", handle);
    };
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
