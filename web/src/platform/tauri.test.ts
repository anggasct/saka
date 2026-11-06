import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({
  invoke,
  isTauri: () => false,
}));

import { ConnectionError } from "./connection-error";
import { tauriPlatform } from "./tauri";

const ACTIVE = {
  connections: [
    { id: "a", name: "Office", url: "https://saka.example.com", active: true },
  ],
  active: { id: "a", url: "https://saka.example.com", token: "sdt_abc" },
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
});
