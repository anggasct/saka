import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  ceremony,
  LAPTOP,
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
    [STATUS]: { body: status({ setup_required: true }) },
    [REGISTER_START]: { body: ceremony("reg-1") },
    [REGISTER_FINISH]: { status: 201, body: LAPTOP },
    ...extra,
  };
}

async function fill(code: string, name: string) {
  await userEvent.type(await screen.findByLabelText("Code"), code);
  await userEvent.type(screen.getByLabelText("Name of this device"), name);
}

describe("Register a passkey", () => {
  beforeEach(() => {
    vi.spyOn(platform.credentials, "supported").mockReturnValue(true);
    vi.spyOn(platform, "navigateTo").mockImplementation(() => undefined);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("explains the host command on a new instance and registers with the code", async () => {
    const create = vi
      .spyOn(platform.credentials, "create")
      .mockResolvedValue({ status: "ok", credential: { id: "new" } });
    const api = fakeApi(routes());
    renderApp("/setup");
    expect(
      await screen.findByText(/run saka auth setup and enter the code/),
    ).toBeVisible();
    const submit = screen.getByRole("button", { name: "Create passkey" });
    expect(submit).toBeDisabled();
    await fill("wdjb-mjht1", "Phone");
    expect(submit).toBeEnabled();
    await userEvent.click(submit);
    await waitFor(() => {
      expect(platform.navigateTo).toHaveBeenCalledWith("/");
    });
    expect(api.requests.find((r) => r.path === REGISTER_START)?.body).toEqual({
      code: "wdjb-mjht1",
      name: "Phone",
    });
    expect(create).toHaveBeenCalledWith({ publicKey: { challenge: "AQID" } });
    expect(api.requests.find((r) => r.path === REGISTER_FINISH)?.body).toEqual({
      ceremony_id: "reg-1",
      credential: { id: "new" },
    });
  });

  it("shows one sentence for a code that does not work and keeps what was typed", async () => {
    fakeApi(
      routes({
        [REGISTER_START]: {
          status: 400,
          body: { code: "pairing_code_invalid", params: {} },
        },
      }),
    );
    renderApp("/setup");
    await fill("ZZZZZZZZZZ", "Phone");
    await userEvent.click(
      screen.getByRole("button", { name: "Create passkey" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "This code is not valid.",
    );
    expect(screen.getByLabelText("Code")).toHaveValue("ZZZZZZZZZZ");
    expect(platform.navigateTo).not.toHaveBeenCalled();
  });

  it("says when the prompt was cancelled, the device is known, or the browser cannot", async () => {
    const create = vi
      .spyOn(platform.credentials, "create")
      .mockResolvedValue({ status: "cancelled" });
    fakeApi(routes());
    renderApp("/setup");
    await fill("ZZZZZZZZZZ", "Phone");
    const submit = screen.getByRole("button", { name: "Create passkey" });
    await userEvent.click(submit);
    expect(await screen.findByRole("alert")).toHaveTextContent("cancelled");
    create.mockResolvedValue({ status: "duplicate" });
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.getByRole("alert")).toHaveTextContent(
        "already has a passkey",
      );
    });
    vi.spyOn(platform.credentials, "supported").mockReturnValue(false);
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.getByRole("alert")).toHaveTextContent(
        "cannot create passkeys",
      );
    });
  });

  it("offers sign-in when a passkey already exists", async () => {
    fakeApi(routes({ [STATUS]: { body: status() } }));
    renderApp("/setup");
    expect(
      await screen.findByText(/Enter a code from a signed-in browser/),
    ).toBeVisible();
    await userEvent.click(
      screen.getByRole("link", { name: "Already have a passkey? Sign in" }),
    );
    expect(
      await screen.findByRole("button", { name: "Sign in with a passkey" }),
    ).toBeVisible();
  });

  it("sends a signed-in browser on to where it was going", async () => {
    fakeApi(routes({ [STATUS]: { body: SIGNED_IN } }));
    renderApp("/setup?next=%2Fdocs");
    await waitFor(() => {
      expect(platform.navigateTo).toHaveBeenCalledWith("/docs");
    });
  });
});
