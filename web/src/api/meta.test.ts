import { afterEach, describe, expect, it, vi } from "vitest";

import { fakeApi } from "@/test/fake-api";

import { checkServer, compatibility } from "./meta";

const meta = (api: string, min: string) => ({
  server_version: "0.9.0",
  api_version: api,
  min_client_api_version: min,
});

describe("compatibility", () => {
  it("accepts the same version and the one before it", () => {
    expect(compatibility(meta("1", "1")).status).toBe("compatible");
    expect(compatibility(meta("0", "0")).status).toBe("compatible");
  });

  it("refuses a server that needs a newer client", () => {
    expect(compatibility(meta("3", "2"))).toEqual({
      status: "incompatible",
      reason: "client_too_old",
      serverVersion: "0.9.0",
      apiVersion: "3",
    });
  });

  it("refuses a server more than one version behind", () => {
    expect(compatibility(meta("-1", "-1"))).toMatchObject({
      reason: "server_too_old",
    });
  });

  it("refuses versions it cannot read", () => {
    expect(compatibility(meta("one", "1"))).toMatchObject({
      reason: "unreadable",
    });
  });
});

describe("checkServer", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("reads /meta and judges it", async () => {
    fakeApi({
      "/api/v1/meta": { body: { ...meta("5", "5"), capabilities: [] } },
    });
    expect((await checkServer()).status).toBe("incompatible");
  });

  it("lets the app open when /meta cannot be read", async () => {
    vi.stubGlobal("fetch", () => Promise.reject(new TypeError("offline")));
    expect((await checkServer()).status).toBe("compatible");
    fakeApi({});
    expect((await checkServer()).status).toBe("compatible");
  });
});
