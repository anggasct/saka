import { afterEach, describe, expect, it, vi } from "vitest";

import { webPlatform } from "./web";

describe("copyText", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("writes to the clipboard and reports success", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    await expect(webPlatform.copyText("WDJB-MJHT")).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith("WDJB-MJHT");
  });

  it("reports failure when the clipboard refuses or is missing", async () => {
    vi.stubGlobal("navigator", {
      clipboard: { writeText: vi.fn().mockRejectedValue(new Error("no")) },
    });
    await expect(webPlatform.copyText("x")).resolves.toBe(false);
    vi.stubGlobal("navigator", {});
    await expect(webPlatform.copyText("x")).resolves.toBe(false);
  });
});

describe("web platform", () => {
  it("stays on the page origin and carries no token", () => {
    expect(webPlatform.apiBaseUrl()).toBe(location.origin);
    expect(webPlatform.apiToken()).toBeNull();
    expect(webPlatform.info().kind).toBe("web");
  });

  it("is ready at once", async () => {
    await expect(webPlatform.ready()).resolves.toBeUndefined();
  });
});

describe("onVisible", () => {
  it("calls back only when the page becomes visible, until it is stopped", () => {
    const listener = vi.fn();
    const stop = webPlatform.onVisible(listener);
    const state = vi.spyOn(document, "visibilityState", "get");
    state.mockReturnValue("hidden");
    document.dispatchEvent(new Event("visibilitychange"));
    expect(listener).not.toHaveBeenCalled();
    state.mockReturnValue("visible");
    document.dispatchEvent(new Event("visibilitychange"));
    expect(listener).toHaveBeenCalledTimes(1);
    stop();
    document.dispatchEvent(new Event("visibilitychange"));
    expect(listener).toHaveBeenCalledTimes(1);
    state.mockRestore();
  });
});
