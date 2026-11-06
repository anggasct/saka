import { afterEach, describe, expect, it, vi } from "vitest";

import { webCredentials } from "./credentials";

const bytes = (...values: number[]) => new Uint8Array(values).buffer;

function stubCredentials(impl: {
  create?: (options: CredentialCreationOptions) => Promise<unknown>;
  get?: (options: CredentialRequestOptions) => Promise<unknown>;
}) {
  const create = vi.fn(impl.create ?? (() => Promise.resolve(null)));
  const get = vi.fn(impl.get ?? (() => Promise.resolve(null)));
  vi.stubGlobal("navigator", { credentials: { create, get } });
  vi.stubGlobal("PublicKeyCredential", function PublicKeyCredential() {
    return undefined;
  });
  return { create, get };
}

const CREATE_OPTIONS = {
  publicKey: {
    challenge: "AQID",
    rp: { id: "example.com", name: "Saka" },
    user: { id: "BAUG", name: "operator", displayName: "Laptop" },
    pubKeyCredParams: [{ type: "public-key", alg: -7 }],
    excludeCredentials: [{ type: "public-key", id: "_-8" }],
  },
};

describe("credentials", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("reports whether the browser has passkeys", () => {
    vi.stubGlobal("navigator", {});
    expect(webCredentials.supported()).toBe(false);
    stubCredentials({});
    expect(webCredentials.supported()).toBe(true);
  });

  it("turns base64url options into buffers and the new credential into JSON", async () => {
    const { create } = stubCredentials({
      create: () =>
        Promise.resolve({
          id: "abc",
          type: "public-key",
          rawId: bytes(251, 255),
          response: {
            attestationObject: bytes(1, 2),
            clientDataJSON: bytes(3, 4),
          },
        }),
    });
    const result = await webCredentials.create(CREATE_OPTIONS);
    const sent = create.mock.calls[0]?.[0]?.publicKey;
    expect(new Uint8Array(sent?.challenge as ArrayBuffer)).toEqual(
      new Uint8Array([1, 2, 3]),
    );
    expect(new Uint8Array(sent?.user.id as ArrayBuffer)).toEqual(
      new Uint8Array([4, 5, 6]),
    );
    expect(
      new Uint8Array(sent?.excludeCredentials?.[0]?.id as ArrayBuffer),
    ).toEqual(new Uint8Array([255, 239]));
    expect(sent?.rp.id).toBe("example.com");
    expect(result).toEqual({
      status: "ok",
      credential: {
        id: "abc",
        rawId: "-_8",
        type: "public-key",
        response: { attestationObject: "AQI", clientDataJSON: "AwQ" },
        extensions: {},
      },
    });
  });

  it("signs a challenge and returns the assertion", async () => {
    const { get } = stubCredentials({
      get: () =>
        Promise.resolve({
          id: "abc",
          type: "public-key",
          rawId: bytes(1),
          response: {
            authenticatorData: bytes(9),
            clientDataJSON: bytes(8),
            signature: bytes(7),
            userHandle: null,
          },
        }),
    });
    const result = await webCredentials.get({
      publicKey: {
        challenge: "AQID",
        allowCredentials: [{ type: "public-key", id: "AQ" }],
      },
    });
    expect(get.mock.calls[0]?.[0]?.publicKey?.allowCredentials).toHaveLength(1);
    expect(result).toMatchObject({
      status: "ok",
      credential: {
        response: {
          authenticatorData: "CQ",
          signature: "Bw",
          userHandle: null,
        },
      },
    });
  });

  it("names why nothing came back", async () => {
    stubCredentials({ create: () => Promise.resolve(null) });
    await expect(webCredentials.create(CREATE_OPTIONS)).resolves.toEqual({
      status: "cancelled",
    });
    for (const [name, status] of [
      ["NotAllowedError", "cancelled"],
      ["InvalidStateError", "duplicate"],
      ["SecurityError", "failed"],
    ] as const) {
      stubCredentials({
        create: () => Promise.reject(new DOMException("x", name)),
      });
      await expect(webCredentials.create(CREATE_OPTIONS)).resolves.toEqual({
        status,
      });
    }
    vi.stubGlobal("navigator", {});
    await expect(webCredentials.create(CREATE_OPTIONS)).resolves.toEqual({
      status: "unsupported",
    });
  });

  it("refuses options that are not WebAuthn options", async () => {
    stubCredentials({});
    await expect(webCredentials.create({})).resolves.toEqual({
      status: "failed",
    });
    await expect(webCredentials.get(null)).resolves.toEqual({
      status: "failed",
    });
  });
});
