import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { startEventStream } from "@/api/events";
import { fakeApi, page } from "@/test/fake-api";
import type { Recorded, Routes } from "@/test/fake-api";
import { decision } from "@/test/decision-fixtures";
import { fakeStream } from "@/test/fake-stream";
import { permission } from "@/test/fixtures";
import {
  ATTEMPT,
  DRAFT,
  fullSession,
  sessionId,
  WORK_ITEM,
} from "@/test/session-fixtures";
import { renderApp } from "@/test/render";

import type { Session } from "./queries";

const ID = sessionId(1);
const SESSION = `/api/v1/sessions/${ID}`;
const TIMELINE = `${SESSION}/timeline`;
const PERMISSIONS = "/api/v1/permission-requests";
const DECISIONS = "/api/v1/decision-requests";

const NO_INBOX: Routes = {
  "/api/v1/inbox/summary": {
    body: { open: 0, running: 0, merged_today: 0 },
  },
};

const ACTIVITY = [
  {
    offset: 0,
    at: "2026-11-06T10:00:00.000Z",
    kind: "message",
    summary: "Reading the spec",
    data: {},
  },
  {
    offset: 1,
    at: "2026-11-06T10:01:00.000Z",
    kind: "action",
    summary: "Bash: make test",
    data: { tool: "Bash" },
  },
];

function routes(session: () => Session, extra: Routes = {}): Routes {
  return {
    ...NO_INBOX,
    [SESSION]: () => ({ body: session() }),
    [TIMELINE]: {
      body: { items: ACTIVITY, prev_before: null, next_after: null },
    },
    [PERMISSIONS]: { body: page([]) },
    [DECISIONS]: { body: page([]) },
    ...extra,
  };
}

function posts(requests: Recorded[]): Recorded[] {
  return requests.filter((r) => r.method === "POST");
}

describe("session page", () => {
  it("shows the header, the environment, the live timeline and the controls of a task session", async () => {
    fakeApi(routes(() => fullSession(1)));
    renderApp(`/sessions/${ID}`);
    expect(
      await screen.findByRole("heading", { name: "feat-x/s1", level: 1 }),
    ).toBeVisible();
    expect(screen.getByText(/tasks · Developer/)).toBeVisible();
    expect(screen.getAllByText("Running").length).toBeGreaterThan(0);
    expect(screen.getByRole("link", { name: "Open the task" })).toHaveAttribute(
      "href",
      `/delivery/tasks/${WORK_ITEM}`,
    );
    expect(screen.getByRole("link", { name: "All sessions" })).toHaveAttribute(
      "href",
      "/sessions",
    );
    const env = screen
      .getByRole("heading", { name: "Environment" })
      .closest("section");
    expect(env).not.toBeNull();
    if (env !== null) {
      expect(within(env).getByText("saka/feat-x-s1")).toBeVisible();
      expect(within(env).getByText("41000–41003")).toBeVisible();
      expect(within(env).getByText("node 26.13.0")).toBeVisible();
    }
    const log = await screen.findByRole("log", { name: "Session timeline" });
    expect(await within(log).findByText("Reading the spec")).toBeVisible();
    expect(screen.getByLabelText("Steer the agent")).toBeVisible();
    expect(screen.getByRole("button", { name: "Interrupt" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Stop" })).toBeVisible();
  });

  it("answers a waiting permission request and a question in place", async () => {
    const waiting = permission(1);
    const asked = decision(1, {
      owner: {
        kind: "stage_run",
        work_item_id: WORK_ITEM,
        stage_run_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344c1",
      },
    });
    const { requests } = fakeApi(
      routes(
        () => fullSession(1, { state: "waiting", available_actions: ["stop"] }),
        {
          [PERMISSIONS]: { body: page([waiting]) },
          [DECISIONS]: { body: page([asked]) },
          [`${PERMISSIONS}/${waiting.id}/answer`]: {
            body: { ...waiting, state: "allowed" },
          },
          [`${DECISIONS}/${asked.id}/answer`]: {
            body: { ...asked, state: "answered" },
          },
        },
      ),
    );
    renderApp(`/sessions/${ID}`);
    const section = (
      await screen.findByRole("heading", { name: "Waiting for you" })
    ).closest("section");
    expect(section).not.toBeNull();
    if (section !== null) {
      await userEvent.click(
        within(section).getByRole("button", { name: "Allow" }),
      );
    }
    await waitFor(() => {
      expect(posts(requests).map((r) => r.path)).toContain(
        `${PERMISSIONS}/${waiting.id}/answer`,
      );
    });
    expect(screen.queryByLabelText("Steer the agent")).not.toBeInTheDocument();

    const group = await screen.findByRole("group", { name: "Your choice" });
    await userEvent.click(
      within(group).getByRole("radio", { name: /Local disk/ }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Send answer" }));
    await waitFor(() => {
      expect(posts(requests).map((r) => r.path)).toContain(
        `${DECISIONS}/${asked.id}/answer`,
      );
    });
    const listing = requests.find((r) => r.path === PERMISSIONS);
    expect(listing?.search.get("work_item_id")).toBe(WORK_ITEM);
  });

  it("works for an architect session of a draft, without asking for task permissions", async () => {
    const draftSession = fullSession(1, {
      role: "architect",
      title: "export",
      owner: {
        kind: "draft_attempt",
        spec_draft_id: DRAFT,
        attempt_id: ATTEMPT,
      },
    });
    const { requests } = fakeApi(routes(() => draftSession));
    renderApp(`/sessions/${ID}`);
    expect(
      await screen.findByRole("heading", { name: "export", level: 1 }),
    ).toBeVisible();
    expect(screen.getByText(/tasks · Architect/)).toBeVisible();
    const open = screen.getByRole("link", { name: "Open the draft" });
    expect(open.getAttribute("href")).toMatch(new RegExp(`/drafts/${DRAFT}$`));
    await screen.findByRole("log");
    expect(requests.some((r) => r.path === PERMISSIONS)).toBe(false);
    const decisions = requests.find((r) => r.path === DECISIONS);
    expect(decisions?.search.get("spec_draft_id")).toBe(DRAFT);
    expect(screen.getByLabelText("Steer the agent")).toBeVisible();
  });

  it("asks for a reason, names the task and stops the session", async () => {
    const { requests } = fakeApi(
      routes(() => fullSession(1), {
        [`${SESSION}/stop`]: { body: fullSession(1) },
      }),
    );
    renderApp(`/sessions/${ID}`);
    await userEvent.click(await screen.findByRole("button", { name: "Stop" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(/feat-x\/s1 stops now/)).toBeVisible();
    const submit = within(dialog).getByRole("button", {
      name: "Stop the session",
    });
    expect(submit).toBeDisabled();
    await userEvent.type(
      within(dialog).getByLabelText(/Reason/),
      "wrong branch",
    );
    await userEvent.click(submit);
    await waitFor(() => {
      expect(posts(requests).map((r) => r.body)).toEqual([
        { reason: "wrong branch" },
      ]);
    });
  });

  it("drops the controls when the session ends and keeps the timeline", async () => {
    let session = fullSession(1);
    fakeApi(routes(() => session));
    const fake = fakeStream();
    const { queryClient } = renderApp(`/sessions/${ID}`);
    startEventStream(
      queryClient,
      () => fake.stream,
      () => () => undefined,
    );
    await screen.findByRole("button", { name: "Stop" });

    session = fullSession(1, {
      state: "ended",
      available_actions: [],
      ended_at: "2026-11-06T10:30:00.000Z",
      end_reason: { code: "completed", params: {} },
    });
    fake.emit("runtime.session.ended.v1", { seq: 9, stream_type: "session" });
    await waitFor(() => {
      expect(
        screen.queryByRole("button", { name: "Stop" }),
      ).not.toBeInTheDocument();
    });
    expect(screen.queryByLabelText("Steer the agent")).not.toBeInTheDocument();
    expect(screen.getAllByText("Ended").length).toBeGreaterThan(0);
    const log = screen.getByRole("log", { name: "Session timeline" });
    expect(within(log).getByText("Reading the spec")).toBeVisible();
  });

  it("explains an unknown session and links back to the list", async () => {
    fakeApi({
      ...NO_INBOX,
      [SESSION]: { status: 404, body: { code: "not_found" } },
    });
    renderApp(`/sessions/${ID}`);
    expect(
      await screen.findByRole("heading", {
        name: "This session does not exist",
      }),
    ).toBeVisible();
    expect(screen.getByRole("link", { name: "All sessions" })).toHaveAttribute(
      "href",
      "/sessions",
    );
  });

  it("shows another failure with a retry", async () => {
    fakeApi({ ...NO_INBOX, [SESSION]: { status: 500, body: { code: "x" } } });
    renderApp(`/sessions/${ID}`);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Could not load the session",
    );
  });
});
