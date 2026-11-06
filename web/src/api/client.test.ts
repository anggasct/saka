import { afterEach, describe, expect, it, vi } from "vitest";

import { platform } from "@/platform";
import { fakeApi } from "@/test/fake-api";

import { api } from "./client";

describe("api client", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("sends no Authorization header from the web platform", async () => {
    const fake = fakeApi({ "/api/v1/meta": { body: {} } });
    await api.GET("/api/v1/meta");
    expect(fake.requests[0]?.headers.has("authorization")).toBe(false);
  });

  it("sends the platform token as a bearer header", async () => {
    vi.spyOn(platform, "apiToken").mockReturnValue("sdt_secret");
    const fake = fakeApi({ "/api/v1/meta": { body: {} } });
    await api.GET("/api/v1/meta");
    expect(fake.requests[0]?.headers.get("authorization")).toBe(
      "Bearer sdt_secret",
    );
  });
});
