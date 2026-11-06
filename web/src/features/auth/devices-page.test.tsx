import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  ceremony,
  DEVICE_LIST,
  DEVICES,
  LAPTOP,
  LOGOUT,
  PAIRINGS,
  PHONE,
  PIXEL,
  REGISTER_FINISH,
  REGISTER_START,
  SIGNED_IN,
  STATUS,
  status,
} from "@/test/auth-fixtures";
import { fakeApi } from "@/test/fake-api";
import type { Routes } from "@/test/fake-api";
import { renderApp } from "@/test/render";
import { platform } from "@/platform";

function routes(extra: Routes = {}): Routes {
  return {
    [STATUS]: { body: SIGNED_IN },
    [DEVICES]: { body: DEVICE_LIST },
    ...extra,
  };
}

describe("Devices and sign-in", () => {
  beforeEach(() => {
    vi.spyOn(platform, "navigateTo").mockImplementation(() => undefined);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("lists passkeys and devices, and cannot revoke the local CLI", async () => {
    fakeApi(routes());
    renderApp("/settings/devices");
    const passkeys = within(
      await screen.findByRole("region", { name: "Passkeys" }),
    );
    expect(passkeys.getByText("Laptop")).toBeVisible();
    expect(passkeys.getByText("Never used")).toBeVisible();
    expect(
      passkeys.getByRole("button", { name: "Revoke Phone" }),
    ).toBeVisible();
    const devices = within(
      screen.getByRole("region", { name: "CLI and apps" }),
    );
    expect(devices.getByText("Pixel app")).toBeVisible();
    expect(
      devices.getByText(/replaces its token at every start/),
    ).toBeVisible();
    expect(
      devices.getByRole("button", { name: "Revoke Pixel app" }),
    ).toBeVisible();
    expect(
      devices.queryByRole("button", { name: "Revoke Local CLI" }),
    ).toBeNull();
  });

  it("shows a new pairing code once, with its own Idempotency-Key", async () => {
    vi.spyOn(platform, "copyText").mockResolvedValue(true);
    const api = fakeApi(
      routes({
        [PAIRINGS]: {
          status: 201,
          body: {
            id: "p1",
            purpose: "token",
            code: "WDJBMJHT12",
            expires_at: new Date(Date.now() + 300_000).toISOString(),
          },
        },
      }),
    );
    renderApp("/settings/devices");
    await userEvent.click(
      await screen.findByRole("button", { name: "Pair a CLI or app" }),
    );
    const dialog = within(await screen.findByRole("dialog"));
    expect(dialog.getByText("WDJBMJHT12")).toBeVisible();
    expect(
      dialog.getByText("The code is shown once and works once."),
    ).toBeVisible();
    await userEvent.click(dialog.getByRole("button", { name: "Copy code" }));
    expect(await dialog.findByText("Copied")).toBeVisible();
    await userEvent.click(dialog.getByRole("button", { name: "Done" }));
    await waitFor(() => {
      expect(screen.queryByText("WDJBMJHT12")).toBeNull();
    });
    const sent = api.requests.find((r) => r.path === PAIRINGS);
    expect(sent?.body).toEqual({ purpose: "token" });
    expect(sent?.headers.get("Idempotency-Key")).toBeTruthy();
  });

  it("asks for a passkey code when pairing another browser", async () => {
    const api = fakeApi(
      routes({
        [PAIRINGS]: {
          status: 201,
          body: {
            id: "p1",
            purpose: "passkey",
            code: "ABCDEFGHJK",
            expires_at: new Date(Date.now() + 300_000).toISOString(),
          },
        },
      }),
    );
    renderApp("/settings/devices");
    await userEvent.click(
      await screen.findByRole("button", { name: "Pair another browser" }),
    );
    expect(await screen.findByText("ABCDEFGHJK")).toBeVisible();
    expect(api.requests.find((r) => r.path === PAIRINGS)?.body).toEqual({
      purpose: "passkey",
    });
  });

  it("revokes a device after confirmation and loads the list again", async () => {
    let revoked = false;
    const api = fakeApi(
      routes({
        [DEVICES]: () =>
          Promise.resolve({
            body: revoked
              ? { ...DEVICE_LIST, devices: [DEVICE_LIST.devices[0]] }
              : DEVICE_LIST,
          }),
        [`/api/v1/auth/devices/${PIXEL.id}/revoke`]: () => {
          revoked = true;
          return Promise.resolve({
            body: { ...PIXEL, revoked_at: "2026-11-06T09:00:00.000Z" },
          });
        },
      }),
    );
    renderApp("/settings/devices");
    await userEvent.click(
      await screen.findByRole("button", { name: "Revoke Pixel app" }),
    );
    const dialog = within(await screen.findByRole("dialog"));
    expect(
      dialog.getByText("Its token stops working at the next request."),
    ).toBeVisible();
    await userEvent.click(dialog.getByRole("button", { name: "Revoke" }));
    await waitFor(() => {
      expect(screen.queryByText("Pixel app")).toBeNull();
    });
    const sent = api.requests.find((r) => r.path.endsWith("/revoke"));
    expect(sent?.headers.get("Idempotency-Key")).toBeTruthy();
  });

  it("explains why the last passkey stays", async () => {
    fakeApi(
      routes({
        [DEVICES]: { body: { devices: [], credentials: [LAPTOP] } },
        [`/api/v1/auth/credentials/${LAPTOP.id}/revoke`]: {
          status: 409,
          body: { code: "last_credential", params: {} },
        },
      }),
    );
    renderApp("/settings/devices");
    await userEvent.click(
      await screen.findByRole("button", { name: "Revoke Laptop" }),
    );
    const dialog = within(await screen.findByRole("dialog"));
    await userEvent.click(dialog.getByRole("button", { name: "Revoke" }));
    expect(await dialog.findByRole("alert")).toHaveTextContent(
      "This is the last passkey.",
    );
  });

  it("adds a passkey to the signed-in browser without a code", async () => {
    vi.spyOn(platform.credentials, "supported").mockReturnValue(true);
    vi.spyOn(platform.credentials, "create").mockResolvedValue({
      status: "ok",
      credential: { id: "tablet" },
    });
    const api = fakeApi(
      routes({
        [REGISTER_START]: { body: ceremony("reg-2") },
        [REGISTER_FINISH]: { status: 201, body: PHONE },
      }),
    );
    renderApp("/settings/devices");
    await userEvent.type(
      await screen.findByLabelText("Name of the passkey"),
      "Tablet",
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Create passkey" }),
    );
    expect(await screen.findByText("Passkey added.")).toBeVisible();
    expect(api.requests.find((r) => r.path === REGISTER_START)?.body).toEqual({
      code: null,
      name: "Tablet",
    });
  });

  it("says sign-in is off when the server has no auth section", async () => {
    fakeApi({});
    renderApp("/settings/devices");
    expect(
      await screen.findByText(/Sign-in is off on this server/),
    ).toBeVisible();
  });

  it("shows the sign-out button for a passkey session only", async () => {
    fakeApi(routes({ [LOGOUT]: { status: 204, body: null } }));
    renderApp("/settings/devices");
    await userEvent.click(
      await screen.findByRole("button", { name: "Sign out" }),
    );
    await waitFor(() => {
      expect(platform.navigateTo).toHaveBeenCalledWith("/login");
    });
  });

  it("hides sign-out for a token session", async () => {
    fakeApi(
      routes({
        [STATUS]: {
          body: status({
            authenticated: true,
            actor: "operator:device:0193f0c4-0000-7000-8000-0000000000d1",
          }),
        },
      }),
    );
    renderApp("/settings/devices");
    expect(await screen.findByRole("link", { name: "Devices" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "Sign out" })).toBeNull();
  });
});
