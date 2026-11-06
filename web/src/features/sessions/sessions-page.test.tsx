import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { startEventStream } from "@/api/events";
import { fakeApi } from "@/test/fake-api";
import type { Routes } from "@/test/fake-api";
import { fakeStream } from "@/test/fake-stream";
import { activeSession, sessionId } from "@/test/session-fixtures";
import { renderApp } from "@/test/render";

import type { ActiveSession } from "./queries";

const SESSIONS = "/api/v1/sessions";
const NO_INBOX: Routes = {
  "/api/v1/inbox/summary": {
    body: { open: 0, running: 0, merged_today: 0 },
  },
};

function routes(items: () => ActiveSession[], next: () => string | null) {
  return {
    ...NO_INBOX,
    [SESSIONS]: () => ({
      body: { items: items(), next_cursor: next() },
    }),
  };
}

describe("sessions page", () => {
  it("lists the active sessions with their project, role, state and open requests", async () => {
    const { requests } = fakeApi(
      routes(
        () => [
          activeSession(1, {
            state: "waiting",
            pending: { permissions: 2, decisions: 1 },
          }),
          activeSession(2, {
            role: "architect",
            title: "export",
            owner: {
              kind: "draft_attempt",
              spec_draft_id: "d",
              attempt_id: "a",
            },
          }),
        ],
        () => null,
      ),
    );
    renderApp("/sessions");
    expect(
      await screen.findByRole("heading", { name: "Sessions", level: 1 }),
    ).toBeInTheDocument();
    expect(await screen.findByText("2 sessions are active")).toBeVisible();
    const first = screen.getByRole("link", { name: /feat-x\/s1/ });
    expect(first).toHaveAttribute("href", `/sessions/${sessionId(1)}`);
    expect(within(first).getByText("tasks")).toBeVisible();
    expect(within(first).getByText("Developer")).toBeVisible();
    expect(within(first).getByText("Waiting for permission")).toBeVisible();
    expect(
      within(first).getByText("2 permission requests waiting"),
    ).toBeVisible();
    expect(within(first).getByText("1 question waiting")).toBeVisible();
    const second = screen.getByRole("link", { name: /export/ });
    expect(within(second).getByText("Architect")).toBeVisible();
    expect(within(second).queryByText(/waiting/)).not.toBeInTheDocument();
    const listing = requests.find((r) => r.path === SESSIONS);
    expect(listing?.search.get("active")).toBe("true");
  });

  it("says so when nothing runs", async () => {
    fakeApi(
      routes(
        () => [],
        () => null,
      ),
    );
    renderApp("/sessions");
    expect(
      await screen.findByRole("heading", {
        name: "No agent session is running",
      }),
    ).toBeVisible();
    expect(screen.queryByRole("list")).not.toBeInTheDocument();
  });

  it("follows the event stream without a reload", async () => {
    let items = [activeSession(1)];
    fakeApi(
      routes(
        () => items,
        () => null,
      ),
    );
    const fake = fakeStream();
    const { queryClient } = renderApp("/sessions");
    startEventStream(
      queryClient,
      () => fake.stream,
      () => () => undefined,
    );
    await screen.findByRole("link", { name: /feat-x\/s1/ });

    items = [
      activeSession(2, { pending: { permissions: 1, decisions: 0 } }),
      activeSession(1),
    ];
    fake.emit("runtime.session.started.v1", { seq: 4, stream_type: "session" });
    await screen.findByRole("link", { name: /feat-x\/s2/ });

    items = [activeSession(1)];
    fake.emit("runtime.session.ended.v1", { seq: 5, stream_type: "session" });
    await waitFor(() => {
      expect(
        screen.queryByRole("link", { name: /feat-x\/s2/ }),
      ).not.toBeInTheDocument();
    });
  });

  it("loads the next page on request", async () => {
    const { requests } = fakeApi({
      ...NO_INBOX,
      [SESSIONS]: (request) =>
        request.search.get("cursor") === "next"
          ? { body: { items: [activeSession(2)], next_cursor: null } }
          : { body: { items: [activeSession(1)], next_cursor: "next" } },
    });
    renderApp("/sessions");
    await screen.findByRole("link", { name: /feat-x\/s1/ });
    await userEvent.click(
      screen.getByRole("button", { name: "Show more sessions" }),
    );
    await screen.findByRole("link", { name: /feat-x\/s2/ });
    expect(
      screen.queryByRole("button", { name: "Show more sessions" }),
    ).not.toBeInTheDocument();
    expect(
      requests
        .filter((r) => r.path === SESSIONS)
        .map((r) => r.search.get("cursor")),
    ).toEqual([null, "next"]);
  });

  it("shows the error with a retry", async () => {
    fakeApi({ ...NO_INBOX, [SESSIONS]: { status: 500, body: { code: "x" } } });
    renderApp("/sessions");
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not load the sessions",
    );
    expect(screen.getByRole("button", { name: "Try again" })).toBeVisible();
  });

  it("is in the top navigation", async () => {
    fakeApi(
      routes(
        () => [],
        () => null,
      ),
    );
    renderApp("/sessions");
    const nav = await screen.findByRole("navigation");
    expect(within(nav).getByRole("link", { name: "Sessions" })).toHaveAttribute(
      "href",
      "/sessions",
    );
  });
});
