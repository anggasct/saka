import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  ceremony,
  LOGIN_FINISH,
  LOGIN_START,
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
    [STATUS]: { body: status() },
    [LOGIN_START]: { body: ceremony() },
    [LOGIN_FINISH]: { status: 204, body: null },
    ...extra,
  };
}

describe("Sign in", () => {
  beforeEach(() => {
    vi.spyOn(platform.credentials, "supported").mockReturnValue(true);
    vi.spyOn(platform, "navigateTo").mockImplementation(() => undefined);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("signs in with a passkey and returns to the page that asked", async () => {
    const get = vi
      .spyOn(platform.credentials, "get")
      .mockResolvedValue({ status: "ok", credential: { id: "cred" } });
    const api = fakeApi(routes());
    renderApp("/login?next=%2Fdelivery%3Fstate%3Dblocked");
    await userEvent.click(
      await screen.findByRole("button", { name: "Sign in with a passkey" }),
    );
    await waitFor(() => {
      expect(platform.navigateTo).toHaveBeenCalledWith(
        "/delivery?state=blocked",
      );
    });
    expect(get).toHaveBeenCalledWith({ publicKey: { challenge: "AQID" } });
    const finish = api.requests.find((r) => r.path === LOGIN_FINISH);
    expect(finish?.body).toEqual({
      ceremony_id: "ceremony-1",
      credential: { id: "cred" },
    });
  });

  it("never follows a next outside the app", async () => {
    vi.spyOn(platform.credentials, "get").mockResolvedValue({
      status: "ok",
      credential: {},
    });
    fakeApi(routes());
    renderApp("/login?next=%2F%2Fevil.example");
    await userEvent.click(
      await screen.findByRole("button", { name: "Sign in with a passkey" }),
    );
    await waitFor(() => {
      expect(platform.navigateTo).toHaveBeenCalledWith("/");
    });
  });

  it("says when the prompt was cancelled", async () => {
    vi.spyOn(platform.credentials, "get").mockResolvedValue({
      status: "cancelled",
    });
    fakeApi(routes());
    renderApp("/login");
    await userEvent.click(
      await screen.findByRole("button", { name: "Sign in with a passkey" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Sign-in was cancelled.",
    );
    expect(platform.navigateTo).not.toHaveBeenCalled();
  });

  it("shows the server's sentence for a refused sign-in", async () => {
    vi.spyOn(platform.credentials, "get").mockResolvedValue({
      status: "ok",
      credential: {},
    });
    fakeApi(
      routes({
        [LOGIN_FINISH]: {
          status: 401,
          body: { code: "auth_invalid", params: {} },
        },
      }),
    );
    renderApp("/login");
    await userEvent.click(
      await screen.findByRole("button", { name: "Sign in with a passkey" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Your sign-in is no longer valid.",
    );
    expect(platform.navigateTo).not.toHaveBeenCalled();
  });

  it("tells how long to wait after too many failures", async () => {
    fakeApi(
      routes({
        [LOGIN_START]: {
          status: 429,
          body: { code: "auth_rate_limited", params: { retry_after_secs: 12 } },
        },
      }),
    );
    renderApp("/login");
    await userEvent.click(
      await screen.findByRole("button", { name: "Sign in with a passkey" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Try again in 12 seconds.",
    );
  });

  it("explains a browser without passkeys", async () => {
    vi.spyOn(platform.credentials, "supported").mockReturnValue(false);
    fakeApi(routes());
    renderApp("/login");
    await userEvent.click(
      await screen.findByRole("button", { name: "Sign in with a passkey" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "cannot use passkeys",
    );
  });

  it("sends a new instance to the setup page", async () => {
    fakeApi(routes({ [STATUS]: { body: status({ setup_required: true }) } }));
    renderApp("/login?next=%2Fdocs");
    expect(
      await screen.findByRole("heading", { name: "Register a passkey" }),
    ).toBeVisible();
    expect(screen.getByText(/saka auth setup/)).toBeVisible();
  });

  it("leaves the page for the inbox when sign-in is off, and for the target when already signed in", async () => {
    fakeApi({});
    renderApp("/login");
    expect(await screen.findByRole("heading", { name: "Inbox" })).toBeVisible();
    fakeApi(routes({ [STATUS]: { body: SIGNED_IN } }));
    renderApp("/login?next=%2Fdocs");
    await waitFor(() => {
      expect(platform.navigateTo).toHaveBeenCalledWith("/docs");
    });
  });
});
