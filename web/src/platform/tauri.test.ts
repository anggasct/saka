import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
const listen = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  isTauri: () => false,
}));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

import { ConnectionError } from "./connection-error";
import { tauriPlatform } from "./tauri";

const ACTIVE = {
  connections: [
    {
      id: "a",
      name: "Office",
      url: "https://saka.example.com",
      active: true,
      kind: "remote",
      running: false,
    },
  ],
  active: {
    id: "a",
    url: "https://saka.example.com",
    token: "sdt_abc",
    kind: "remote",
  },
};

const LOCAL_ENTRY = {
  id: "local",
  name: "This computer",
  url: "http://127.0.0.1:41000",
  active: true,
  kind: "local",
  running: false,
};
const LOCAL_DOWN = {
  connections: [LOCAL_ENTRY],
  active: { id: "local", url: LOCAL_ENTRY.url, token: null, kind: "local" },
};
const LOCAL_UP = {
  ...LOCAL_DOWN,
  connections: [{ ...LOCAL_ENTRY, running: true }],
};

describe("tauri platform", () => {
  beforeEach(() => {
    invoke.mockReset();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("takes the base URL and the token from the active connection once ready", async () => {
    invoke.mockResolvedValue(ACTIVE);
    await tauriPlatform.ready();
    expect(invoke).toHaveBeenCalledWith("connections_list", undefined);
    expect(tauriPlatform.apiBaseUrl()).toBe("https://saka.example.com");
    expect(tauriPlatform.apiToken()).toBe("sdt_abc");
    expect(tauriPlatform.connections()?.hasActive()).toBe(true);
    expect(tauriPlatform.info().kind).toBe("desktop");
  });

  it("has no token and no active connection before one is added", async () => {
    invoke.mockResolvedValue({ connections: [], active: null });
    await tauriPlatform.ready();
    expect(tauriPlatform.apiToken()).toBeNull();
    expect(tauriPlatform.connections()?.hasActive()).toBe(false);
  });

  it("reads events with fetch against the active server and its token", async () => {
    invoke.mockResolvedValue(ACTIVE);
    await tauriPlatform.ready();
    const fetchMock = vi.fn(() => new Promise<Response>(() => undefined));
    vi.stubGlobal("fetch", fetchMock);
    const stream = tauriPlatform.eventSource("/api/v1/events");
    await vi.waitFor(() => {
      expect(fetchMock).toHaveBeenCalled();
    });
    const [url, init] = fetchMock.mock.calls[0] as unknown as [
      URL,
      RequestInit,
    ];
    expect(url.href).toBe("https://saka.example.com/api/v1/events");
    expect(new Headers(init.headers).get("Authorization")).toBe(
      "Bearer sdt_abc",
    );
    stream.close();
  });

  it("sends the shell commands with the names the shell expects", async () => {
    invoke.mockResolvedValue(undefined);
    const connections = tauriPlatform.connections();
    await connections?.add({
      name: "Office",
      url: "https://saka.example.com",
      pairingCode: "WDJB-MJHT",
    });
    expect(invoke).toHaveBeenCalledWith("connection_add", {
      name: "Office",
      url: "https://saka.example.com",
      pairing_code: "WDJB-MJHT",
    });
    await connections?.remove("a");
    expect(invoke).toHaveBeenCalledWith("connection_remove", { id: "a" });
    await connections?.select("a");
    expect(invoke).toHaveBeenCalledWith("connection_select", { id: "a" });
  });

  it("turns a shell error into a ConnectionError with its code", async () => {
    invoke.mockRejectedValue({ code: "pairing_code_invalid" });
    const error = await tauriPlatform
      .connections()
      ?.add({ name: "x", url: "https://x.test", pairingCode: "y" })
      .catch((e: unknown) => e);
    expect(error).toBeInstanceOf(ConnectionError);
    expect((error as ConnectionError).code).toBe("pairing_code_invalid");
    invoke.mockRejectedValue("boom");
    const unknown = await tauriPlatform
      .connections()
      ?.remove("a")
      .catch((e: unknown) => e);
    expect((unknown as ConnectionError).code).toBe("unknown");
  });

  it("starts the local Saka before the app opens when it is the active connection", async () => {
    let started = false;
    invoke.mockImplementation((command: string): Promise<unknown> => {
      if (command === "local_start") {
        started = true;
        return Promise.resolve(undefined);
      }
      return Promise.resolve(started ? LOCAL_UP : LOCAL_DOWN);
    });
    await tauriPlatform.ready();
    expect(invoke.mock.calls.map((c: unknown[]) => c[0])).toEqual([
      "connections_list",
      "local_start",
      "connections_list",
    ]);
    expect(tauriPlatform.apiBaseUrl()).toBe("http://127.0.0.1:41000");
    expect(tauriPlatform.apiToken()).toBeNull();
    expect(tauriPlatform.connections()?.hasActive()).toBe(true);
  });

  it("keeps the app on the servers page and remembers why when the local Saka cannot start", async () => {
    invoke.mockImplementation((command: string) =>
      command === "local_start"
        ? Promise.reject(
            Object.assign(new Error("x"), { code: "local_start_failed" }),
          )
        : Promise.resolve(LOCAL_DOWN),
    );
    await tauriPlatform.ready();
    expect(tauriPlatform.connections()?.hasActive()).toBe(false);
    expect(tauriPlatform.connections()?.startError()).toBe(
      "local_start_failed",
    );
  });

  it("sends a notification to the shell with its route", () => {
    invoke.mockResolvedValue(undefined);
    tauriPlatform.notify({ title: "Saka", body: "1 item", route: "/inbox/x" });
    expect(invoke).toHaveBeenCalledWith("notify", {
      title: "Saka",
      body: "1 item",
      route: "/inbox/x",
    });
  });

  it("hands the shell's navigation requests to the app until it stops listening", async () => {
    const unlisten = vi.fn();
    let deliver: (event: { payload: string }) => void = () => undefined;
    listen.mockImplementation(
      (_name: string, handler: (event: { payload: string }) => void) => {
        deliver = handler;
        return Promise.resolve(unlisten);
      },
    );
    const seen: string[] = [];
    const stop = tauriPlatform.onNavigate((path) => seen.push(path));
    await vi.waitFor(() => {
      expect(listen).toHaveBeenCalledWith(
        "saka://navigate",
        expect.any(Function),
      );
    });
    deliver({ payload: "/inbox/x" });
    expect(seen).toEqual(["/inbox/x"]);
    stop();
    expect(unlisten).toHaveBeenCalled();
  });
});
