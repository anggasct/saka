// @vitest-environment node

import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

import { describe, expect, it } from "vitest";

const root = fileURLToPath(new URL("..", import.meta.url));
const publicFile = (name: string) => path.join(root, "public", name);

type Icon = { src: string; sizes: string; type: string; purpose?: string };
type Manifest = {
  name: string;
  short_name: string;
  start_url: string;
  scope: string;
  display: string;
  background_color: string;
  theme_color: string;
  icons: Icon[];
};

const manifest = JSON.parse(
  readFileSync(publicFile("manifest.webmanifest"), "utf8"),
) as Manifest;

function pngSize(file: string): [number, number] {
  const bytes = readFileSync(file);
  expect(bytes.subarray(0, 8)).toEqual(
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  );
  return [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
}

describe("manifest", () => {
  it("names the app and starts it standalone inside its scope", () => {
    expect(manifest.name).toBe("Saka");
    expect(manifest.short_name).toBe("Saka");
    expect(manifest.display).toBe("standalone");
    expect(manifest.start_url.startsWith(manifest.scope)).toBe(true);
    for (const colour of [manifest.background_color, manifest.theme_color]) {
      expect(colour).toMatch(/^#[0-9a-f]{6}$/);
    }
  });

  it("lists real PNG icons at 192 and 512 px, one of them maskable", () => {
    const sizes = manifest.icons.map((icon) => icon.sizes);
    expect(sizes).toContain("192x192");
    expect(sizes).toContain("512x512");
    expect(manifest.icons.some((i) => i.purpose === "maskable")).toBe(true);
    for (const icon of manifest.icons) {
      expect(icon.type).toBe("image/png");
      const [width, height] = pngSize(publicFile(icon.src));
      expect(`${width}x${height}`).toBe(icon.sizes);
    }
  });

  it("is linked from the page with the theme colour", () => {
    const html = readFileSync(path.join(root, "index.html"), "utf8");
    expect(html).toContain('rel="manifest" href="/manifest.webmanifest"');
    expect(html).toContain(`content="${manifest.theme_color}"`);
  });
});

type Handler = (event: {
  request: { method: string; url: string; mode: string };
  waitUntil: (work: Promise<unknown>) => void;
  respondWith: (answer: Promise<Response>) => void;
}) => void;

function worker(options: { online: boolean; listed?: boolean }) {
  const handlers = new Map<string, Handler>();
  const store = new Map<string, Response>();
  const network: string[] = [];
  const key = (request: string | { url: string }) =>
    new URL(typeof request === "string" ? request : request.url, ORIGIN)
      .pathname;
  const cache = {
    put: (request: string | { url: string }, response: Response) => {
      store.set(key(request), response);
      return Promise.resolve();
    },
    addAll: async (paths: string[]) => {
      for (const p of paths) {
        store.set(key(p), await fetchMock(p));
      }
    },
  };
  const fetchMock = (request: string | { url: string }) => {
    network.push(key(request));
    if (!options.online) {
      return Promise.reject(new TypeError("offline"));
    }
    const path = key(request);
    if (path === "/precache.json" && options.listed === false) {
      return Promise.resolve(new Response("", { status: 404 }));
    }
    if (path === "/precache.json") {
      return Promise.resolve(
        new Response(
          JSON.stringify([
            "/assets/index-1.js",
            "/assets/chunk-1.js",
            "/assets/inbox-1.js",
          ]),
        ),
      );
    }
    const body =
      path === "/"
        ? '<link rel="modulepreload" href="/assets/chunk-1.js"><script type="module" src="/assets/index-1.js"></script>'
        : "body";
    return Promise.resolve(new Response(body));
  };
  const caches = {
    open: () => Promise.resolve(cache),
    match: (
      request: string | { url: string },
      options?: { ignoreVary?: boolean },
    ) => {
      matchOptions.push(options?.ignoreVary === true);
      return Promise.resolve(store.get(key(request))?.clone());
    },
    keys: () => Promise.resolve(["saka-shell-v1", "saka-shell-v0"]),
    delete: (name: string) => {
      deleted.push(name);
      return Promise.resolve(true);
    },
  };
  const deleted: string[] = [];
  const matchOptions: boolean[] = [];
  const self = {
    location: { origin: ORIGIN },
    addEventListener: (type: string, handler: Handler) => {
      handlers.set(type, handler);
    },
    skipWaiting: () => Promise.resolve(),
    clients: { claim: () => Promise.resolve() },
  };
  const source = readFileSync(publicFile("sw.js"), "utf8");
  vm.runInNewContext(source, {
    self,
    caches,
    fetch: fetchMock,
    Response,
    URL,
    Promise,
    TypeError,
    Error,
  });
  return { handlers, store, network, deleted, matchOptions };
}

const ORIGIN = "https://saka.example.com";

function fetchEvent(
  url: string,
  init: { method?: string; mode?: string } = {},
) {
  const answers: Promise<Response>[] = [];
  return {
    event: {
      request: {
        method: init.method ?? "GET",
        url: `${ORIGIN}${url}`,
        mode: init.mode ?? "cors",
      },
      waitUntil: () => undefined,
      respondWith: (answer: Promise<Response>) => answers.push(answer),
    },
    answers,
  };
}

describe("service worker", () => {
  it("never handles the API, the ops endpoints, other methods or other origins", () => {
    const { handlers } = worker({ online: true });
    const fetchHandler = handlers.get("fetch");
    for (const [url, init] of [
      ["/api/v1/projects", {}],
      ["/api/v1/events", {}],
      ["/api/v1/auth/status", { mode: "navigate" }],
      ["/metrics", { mode: "navigate" }],
      ["/healthz", {}],
      ["/hooks/github", {}],
      ["/assets/index-1.js", { method: "POST" }],
      ["/delivery", { method: "PUT", mode: "navigate" }],
    ] as const) {
      const { event, answers } = fetchEvent(url, init);
      fetchHandler?.(event);
      expect(answers, url).toHaveLength(0);
    }
    const cross = fetchEvent("/assets/x.js");
    cross.event.request.url = "https://elsewhere.example/assets/x.js";
    fetchHandler?.(cross.event);
    expect(cross.answers).toHaveLength(0);
  });

  it("keeps the shell and the built files at install and drops old caches at activation", async () => {
    const { handlers, store, deleted } = worker({ online: true });
    let installed: Promise<unknown> = Promise.resolve();
    handlers.get("install")?.({
      ...fetchEvent("/").event,
      waitUntil: (work) => {
        installed = work;
      },
    });
    await installed;
    expect([...store.keys()].sort()).toEqual([
      "/",
      "/assets/chunk-1.js",
      "/assets/inbox-1.js",
      "/assets/index-1.js",
      "/manifest.webmanifest",
    ]);
    let activated: Promise<unknown> = Promise.resolve();
    handlers.get("activate")?.({
      ...fetchEvent("/").event,
      waitUntil: (work) => {
        activated = work;
      },
    });
    await activated;
    expect(deleted).toEqual(["saka-shell-v0"]);
  });

  it("falls back to the files the page names when the list is missing", async () => {
    const { handlers, store } = worker({ online: true, listed: false });
    let installed: Promise<unknown> = Promise.resolve();
    handlers.get("install")?.({
      ...fetchEvent("/").event,
      waitUntil: (work) => {
        installed = work;
      },
    });
    await installed;
    expect([...store.keys()].sort()).toEqual([
      "/",
      "/assets/chunk-1.js",
      "/assets/index-1.js",
      "/manifest.webmanifest",
    ]);
  });

  it("answers built files from the cache and opens the shell without a network", async () => {
    const online = worker({ online: true });
    let installed: Promise<unknown> = Promise.resolve();
    online.handlers.get("install")?.({
      ...fetchEvent("/").event,
      waitUntil: (work) => {
        installed = work;
      },
    });
    await installed;

    const offline = worker({ online: false });
    for (const [path, response] of online.store) {
      offline.store.set(path, response);
    }
    const asset = fetchEvent("/assets/index-1.js");
    offline.handlers.get("fetch")?.(asset.event);
    expect(await (await asset.answers[0])?.text()).toBe("body");
    expect(offline.network).not.toContain("/assets/index-1.js");
    expect(offline.matchOptions.every(Boolean)).toBe(true);

    const page = fetchEvent("/delivery/tasks/1", { mode: "navigate" });
    offline.handlers.get("fetch")?.(page.event);
    expect(await (await page.answers[0])?.text()).toContain(
      "/assets/index-1.js",
    );
  });
});
