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
