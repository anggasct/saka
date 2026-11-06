import { screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { platform } from "@/platform";
import { fakeApi } from "@/test/fake-api";
import { mountRoot } from "@/test/root";

import { start } from "./app";

describe("start", () => {
  beforeEach(() => {
    vi.spyOn(platform, "rootElement").mockReturnValue(mountRoot());
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("shows the warning and makes no other call when /meta is out of range", async () => {
    const api = fakeApi({
      "/api/v1/meta": {
        body: {
          server_version: "9.0.0",
          api_version: "7",
          min_client_api_version: "7",
          capabilities: [],
        },
      },
    });
    await start();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "needs a newer app",
    );
    expect(
      screen.getByRole("heading", { name: /does not match/ }),
    ).toBeVisible();
    expect(api.calls).toEqual(["/api/v1/meta"]);
  });
});
