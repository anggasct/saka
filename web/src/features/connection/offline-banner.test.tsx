import { act, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { fakeApi } from "@/test/fake-api";
import { renderApp } from "@/test/render";
import { platform } from "@/platform";

function connection(initial: boolean) {
  let online = initial;
  const listeners = new Set<() => void>();
  vi.spyOn(platform.connection, "online").mockImplementation(() => online);
  vi.spyOn(platform.connection, "subscribe").mockImplementation((listener) => {
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
    };
  });
  return (next: boolean) => {
    online = next;
    for (const listener of listeners) {
      listener();
    }
  };
}

describe("Connection banner", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("appears when the browser goes offline and disappears when it is back", async () => {
    const set = connection(true);
    fakeApi({});
    renderApp("/settings");
    await screen.findByRole("link", { name: "Settings" });
    expect(screen.queryByText(/You are offline/)).toBeNull();
    act(() => {
      set(false);
    });
    expect(await screen.findByText(/You are offline/)).toBeVisible();
    act(() => {
      set(true);
    });
    expect(screen.queryByText(/You are offline/)).toBeNull();
  });

  it("is there from the first paint when the page opens offline", async () => {
    connection(false);
    fakeApi({});
    renderApp("/settings");
    expect(await screen.findByText(/You are offline/)).toBeVisible();
  });
});
