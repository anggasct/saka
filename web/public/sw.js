const SHELL_CACHE = "saka-shell-v1";
const MATCH = { ignoreVary: true };
const STATIC_PREFIXES = ["/assets/", "/icons/"];
const SHELL_FILES = ["/", "/manifest.webmanifest"];
const NEVER_HANDLED = ["/api/", "/hooks/", "/metrics", "/healthz", "/readyz"];

function never(url) {
  return NEVER_HANDLED.some((prefix) => url.pathname.startsWith(prefix));
}

function staticAsset(url) {
  return STATIC_PREFIXES.some((prefix) => url.pathname.startsWith(prefix));
}

async function builtFiles(shell) {
  try {
    const listed = await fetch("/precache.json", { cache: "reload" });
    if (listed.ok) {
      return await listed.json();
    }
  } catch {
    return [];
  }
  const html = await shell.clone().text();
  return [...html.matchAll(/(?:src|href)="(\/assets\/[^"]+)"/g)].map(
    (match) => match[1],
  );
}

async function precache() {
  const cache = await caches.open(SHELL_CACHE);
  const shell = await fetch("/", { cache: "reload" });
  if (!shell.ok) {
    throw new Error("shell unavailable");
  }
  const files = await builtFiles(shell);
  await cache.put("/", shell);
  await cache.addAll([...SHELL_FILES.slice(1), ...files]);
}

async function pruneOldCaches() {
  const names = await caches.keys();
  await Promise.all(
    names
      .filter((name) => name !== SHELL_CACHE)
      .map((name) => caches.delete(name)),
  );
}

async function remember(request, response) {
  if (response.ok && response.type === "basic") {
    const cache = await caches.open(SHELL_CACHE);
    await cache.put(request, response.clone());
  }
  return response;
}

async function cacheFirst(request) {
  const hit = await caches.match(request, MATCH);
  return hit ?? remember(request, await fetch(request));
}

async function openShell(request) {
  try {
    return await fetch(request);
  } catch {
    return (await caches.match("/", MATCH)) ?? Response.error();
  }
}

self.addEventListener("install", (event) => {
  event.waitUntil(precache().then(() => self.skipWaiting()));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(pruneOldCaches().then(() => self.clients.claim()));
});

self.addEventListener("fetch", (event) => {
  const { request } = event;
  const url = new URL(request.url);
  if (
    request.method !== "GET" ||
    url.origin !== self.location.origin ||
    never(url)
  ) {
    return;
  }
  if (staticAsset(url)) {
    event.respondWith(cacheFirst(request));
  } else if (request.mode === "navigate") {
    event.respondWith(openShell(request));
  }
});
