import { act, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { fakeApi } from "@/test/fake-api";
import { renderApp } from "@/test/render";
import { platform } from "@/platform";

const SUMMARY = {
  open: 3,
  by_kind: {},
  running: 0,
  merged_today: 0,
};

function api() {
  fakeApi({
    "/api/v1/inbox": { body: { items: [], next_cursor: null } },
    "/api/v1/inbox/summary": { body: SUMMARY },
  });
}

function width(narrow: boolean) {
  let listener: (() => void) | undefined;
  let now = narrow;
  vi.spyOn(platform.viewport, "narrow").mockImplementation(() => now);
  vi.spyOn(platform.viewport, "subscribe").mockImplementation((next) => {
    listener = next;
    return () => {
      listener = undefined;
    };
  });
  return (to: boolean) => {
    now = to;
    act(() => {
      listener?.();
    });
  };
}

function names(nav: HTMLElement): (string | null | undefined)[] {
  return within(nav)
    .getAllByRole("link")
    .map((link) => link.childNodes[0]?.textContent);
}

describe("main navigation", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("is one bar in the header on a wide screen, Sessions after Delivery", async () => {
    api();
    width(false);
    renderApp("/");
    const nav = await screen.findByRole("navigation");
    expect(nav.closest("header")).not.toBeNull();
    await within(nav).findByText("3");
    await waitFor(() => {
      expect(names(nav)).toEqual([
        "Inbox",
        "Docs",
        "Delivery",
        "Sessions",
        "Settings",
      ]);
    });
  });

  it("is a bottom bar of the four destinations on a narrow screen, with the Inbox count", async () => {
    api();
    width(true);
    renderApp("/");
    const nav = await screen.findByRole("navigation", { name: "Main" });
    expect(nav.closest("header")).toBeNull();
    expect(names(nav)).toEqual(["Inbox", "Docs", "Delivery", "Sessions"]);
    await within(nav).findByText("3 items waiting for you");
    expect(
      within(nav).getByText("3 items waiting for you"),
    ).toBeInTheDocument();
    expect(screen.getAllByRole("navigation")).toHaveLength(1);
    const header = screen.getByRole("banner");
    expect(
      within(header).getByRole("link", { name: "Settings" }),
    ).toBeVisible();
    expect(within(header).getByRole("combobox")).toBeVisible();
  });

  it("marks the open destination", async () => {
    api();
    width(true);
    renderApp("/sessions");
    const nav = await screen.findByRole("navigation", { name: "Main" });
    expect(within(nav).getByRole("link", { name: "Sessions" })).toHaveClass(
      "active",
    );
  });

  it("swaps when the window crosses the breakpoint", async () => {
    api();
    const resize = width(false);
    renderApp("/");
    await screen.findByRole("link", { name: "Settings" });
    expect(screen.queryByRole("navigation", { name: "Main" })).toBeNull();
    resize(true);
    expect(
      await screen.findByRole("navigation", { name: "Main" }),
    ).toBeVisible();
    resize(false);
    await screen.findByRole("link", { name: "Settings" });
    expect(screen.queryByRole("navigation", { name: "Main" })).toBeNull();
  });

  it("hides the bottom bar on the sign-in page", async () => {
    api();
    width(true);
    renderApp("/login");
    await screen.findByRole("banner");
    expect(screen.queryByRole("navigation")).toBeNull();
  });
});
