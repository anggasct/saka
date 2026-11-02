import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fakeApi } from "@/test/fake-api";
import type { Recorded, Routes } from "@/test/fake-api";
import { PROJECT_ID, task } from "@/test/fixtures";
import { renderWithProviders } from "@/test/render";

import type { Session } from "./queries";
import { SessionSection } from "./session-section";

const item = task(1, { state: "in_stage", stage: "dev" });
const FIRST = "0193f0c4-1b2a-7c3d-9e8f-0011223344f1";
const SECOND = "0193f0c4-1b2a-7c3d-9e8f-0011223344f2";

function session(id: string, extra: Partial<Session> = {}): Session {
  return {
    id,
    project_id: PROJECT_ID,
    owner: {
      kind: "stage_run",
      work_item_id: item.id,
      stage_run_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344c1",
    },
    cli: "claude",
    cli_version: "2.1.0",
    model: "opus",
    state: "running",
    turns: 2,
    running_ms: 125_000,
    cost_usd: "0.4100",
    input_tokens: 1200,
    output_tokens: 300,
    resumed_from_session_id: null,
    resume_cause: null,
    end_reason: null,
    available_actions: ["steer", "interrupt", "stop"],
    started_at: "2026-10-31T10:00:00.000Z",
    ended_at: null,
    ...extra,
  };
}

const KINDS = [
  { kind: "system", summary: "session_init", data: { code: "session_init" } },
  { kind: "thought", summary: "Reading the spec", data: {} },
  { kind: "message", summary: "I will add the command", data: {} },
  { kind: "action", summary: "Bash: go test ./...", data: { tool: "Bash" } },
  { kind: "output", summary: "FAIL x_test.go", data: { is_error: true } },
  {
    kind: "permission",
    summary: "npm install",
    data: { permission_request_id: "p", state: "allowed" },
  },
  {
    kind: "result",
    summary: "Done",
    data: { turn: 1, status: "success" },
  },
  { kind: "steer", summary: "use the store", data: { turn: 2 } },
];

function rows(from: number, to: number) {
  const out = [];
  for (let o = from; o < to; o++) {
    const base = KINDS[o % KINDS.length] ?? KINDS[0];
    out.push({
      offset: o,
      at: "2026-10-31T10:00:00.000Z",
      kind: base?.kind ?? "message",
      summary: o < KINDS.length ? (base?.summary ?? "") : `row ${o}`,
      data: base?.data ?? {},
    });
  }
  return out;
}

function timelineRoute(total: number, tailSize: number) {
  return (request: Recorded) => {
    const before = request.search.get("before");
    if (request.search.get("tail") === "true") {
      const start = Math.max(0, total - tailSize);
      return {
        body: {
          items: rows(start, total),
          prev_before: start > 0 ? start : null,
          next_after: null,
        },
      };
    }
    if (before !== null) {
      const end = Number(before);
      const start = Math.max(0, end - tailSize);
      return {
        body: {
          items: rows(start, end),
          prev_before: start > 0 ? start : null,
          next_after: end - 1,
        },
      };
    }
    return { body: { items: [], prev_before: null, next_after: null } };
  };
}

function routes(sessions: Session[], extra: Routes = {}): Routes {
  const out: Routes = {
    "/api/v1/sessions": { body: { items: sessions } },
    ...extra,
  };
  for (const s of sessions) {
    const path = `/api/v1/sessions/${s.id}/timeline`;
    out[path] ??= timelineRoute(KINDS.length, 200);
  }
  return out;
}

describe("SessionSection", () => {
  it("opens the timeline at its end and loads earlier activity on request", async () => {
    const { requests } = fakeApi(
      routes([session(FIRST)], {
        [`/api/v1/sessions/${FIRST}/timeline`]: timelineRoute(260, 200),
      }),
    );
    renderWithProviders(
      <SessionSection task={item} project="tasks" permissions={[]} />,
    );
    const log = await screen.findByRole("log", { name: "Session timeline" });
    expect(await within(log).findByText("row 259")).toBeInTheDocument();
    expect(within(log).queryByText("row 59")).not.toBeInTheDocument();
    const first = requests.find((r) => r.path.endsWith("/timeline"));
    expect(first?.search.get("tail")).toBe("true");
    await userEvent.click(
      within(log).getByRole("button", { name: "Load earlier" }),
    );
    expect(await within(log).findByText("row 59")).toBeInTheDocument();
    const older = requests.filter((r) => r.search.has("before"));
    expect(older.map((r) => r.search.get("before"))).toEqual(["60"]);
    const texts = within(log)
      .getAllByText(/^row \d+$/)
      .map((e) => Number(e.textContent.slice(4)));
    expect(texts).toEqual([...texts].sort((a, b) => a - b));
    expect(
      within(log).queryByRole("button", { name: "Load earlier" }),
    ).not.toBeInTheDocument();
  });

  it("renders every kind of activity", async () => {
    fakeApi(routes([session(FIRST)]));
    renderWithProviders(
      <SessionSection task={item} project="tasks" permissions={[]} />,
    );
    const log = await screen.findByRole("log");
    expect(
      await within(log).findByText("The agent session is ready."),
    ).toBeInTheDocument();
    expect(within(log).getByText("Reading the spec")).toHaveClass("italic");
    expect(within(log).getByText("I will add the command")).toBeVisible();
    expect(within(log).getByText("Bash")).toBeInTheDocument();
    expect(within(log).getByText("Bash: go test ./...")).toBeInTheDocument();
    expect(within(log).getByText("FAIL x_test.go")).toHaveClass("bg-danger");
    expect(within(log).getByText("npm install")).toBeInTheDocument();
    expect(within(log).getByText("Allowed")).toBeInTheDocument();
    expect(within(log).getByText("Turn 1 ended: success")).toBeInTheDocument();
    expect(
      within(log).getByText("Operator: use the store"),
    ).toBeInTheDocument();
  });

  it("summarises the session", async () => {
    fakeApi(routes([session(FIRST)]));
    renderWithProviders(
      <SessionSection task={item} project="tasks" permissions={[]} />,
    );
    expect(await screen.findByText("claude 2.1.0")).toBeInTheDocument();
    expect(screen.getByText("opus")).toBeInTheDocument();
    expect(screen.getByText("2 minutes")).toBeInTheDocument();
    expect(screen.getByText("0.4100 USD")).toBeInTheDocument();
    expect(screen.getByText("1,200 in · 300 out")).toBeInTheDocument();
  });

  it("picks the latest session and switches to another", async () => {
    const { requests } = fakeApi(
      routes([
        session(FIRST, {
          state: "failed",
          available_actions: [],
          end_reason: {
            code: "operator_stop",
            params: { reason: "wrong file" },
          },
        }),
        session(SECOND, { resumed_from_session_id: FIRST }),
      ]),
    );
    renderWithProviders(
      <SessionSection task={item} project="tasks" permissions={[]} />,
    );
    const picker = await screen.findByRole("combobox", { name: "Session" });
    expect(picker).toHaveValue(SECOND);
    await waitFor(() => {
      expect(requests.some((r) => r.path.includes(SECOND))).toBe(true);
    });
    expect(requests.some((r) => r.path.includes(FIRST))).toBe(false);
    await userEvent.selectOptions(picker, FIRST);
    await waitFor(() => {
      expect(
        requests.some((r) => r.path === `/api/v1/sessions/${FIRST}/timeline`),
      ).toBe(true);
    });
    expect(
      screen.getByText("Stopped by the operator: wrong file"),
    ).toBeInTheDocument();
  });

  for (const [state, actions] of [
    ["running", ["steer", "interrupt", "stop"]],
    ["idle", ["steer", "stop"]],
    ["starting", ["stop"]],
    ["waiting", ["stop"]],
    ["interrupted", ["stop"]],
    ["ended", []],
    ["failed", []],
    ["lost", []],
  ] as const) {
    it(`offers only the actions the server allows when ${state}`, async () => {
      fakeApi(
        routes([session(FIRST, { state, available_actions: [...actions] })]),
      );
      renderWithProviders(
        <SessionSection task={item} project="tasks" permissions={[]} />,
      );
      await screen.findByRole("log");
      const has = (name: string) =>
        screen.queryByRole("button", { name }) !== null;
      expect(has("Send")).toBe(actions.includes("steer" as never));
      expect(has("Interrupt")).toBe(actions.includes("interrupt" as never));
      expect(has("Stop")).toBe(actions.includes("stop" as never));
    });
  }

  it("steers the agent with an idempotency key", async () => {
    const { requests } = fakeApi(
      routes([session(FIRST)], {
        [`/api/v1/sessions/${FIRST}/steer`]: {
          status: 202,
          body: session(FIRST),
        },
      }),
    );
    renderWithProviders(
      <SessionSection task={item} project="tasks" permissions={[]} />,
    );
    const field = await screen.findByRole("textbox", {
      name: "Steer the agent",
    });
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();
    await userEvent.type(field, "use the store");
    expect(screen.getByText("13/4,000")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => {
      expect(field).toHaveValue("");
    });
    const sent = requests.find((r) => r.path.endsWith("/steer"));
    expect(sent?.body).toEqual({ text: "use the store" });
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("interrupts without a dialog", async () => {
    const { requests } = fakeApi(
      routes([session(FIRST)], {
        [`/api/v1/sessions/${FIRST}/interrupt`]: {
          status: 202,
          body: session(FIRST, { state: "interrupted" }),
        },
      }),
    );
    renderWithProviders(
      <SessionSection task={item} project="tasks" permissions={[]} />,
    );
    await userEvent.click(
      await screen.findByRole("button", { name: "Interrupt" }),
    );
    await waitFor(() => {
      expect(requests.some((r) => r.path.endsWith("/interrupt"))).toBe(true);
    });
    expect(requests.find((r) => r.path.endsWith("/interrupt"))?.body).toEqual(
      {},
    );
  });

  it("stops only with a reason and resends with the same key after a network error", async () => {
    let fail = true;
    const { requests } = fakeApi(
      routes([session(FIRST)], {
        [`/api/v1/sessions/${FIRST}/stop`]: () =>
          fail
            ? Promise.reject(new TypeError("network down"))
            : { status: 202, body: session(FIRST, { state: "failed" }) },
      }),
    );
    renderWithProviders(
      <SessionSection task={item} project="tasks" permissions={[]} />,
    );
    const trigger = await screen.findByRole("button", { name: "Stop" });
    await userEvent.click(trigger);
    const dialog = await screen.findByRole("dialog", {
      name: "Stop the agent session",
    });
    const submit = within(dialog).getByRole("button", {
      name: "Stop the session",
    });
    expect(submit).toBeDisabled();
    await userEvent.type(
      within(dialog).getByRole("textbox", { name: "Reason" }),
      "wrong approach",
    );
    await userEvent.click(submit);
    expect(
      await within(dialog).findByText("The server could not be reached."),
    ).toBeInTheDocument();
    fail = false;
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const stops = requests.filter((r) => r.path.endsWith("/stop"));
    expect(stops).toHaveLength(2);
    expect(stops[1]?.body).toEqual({ reason: "wrong approach" });
    const key = stops[0]?.headers.get("Idempotency-Key");
    expect(key).toBeTruthy();
    expect(stops[1]?.headers.get("Idempotency-Key")).toBe(key);
  });

  it("explains a session that changed and reloads it", async () => {
    const { requests } = fakeApi(
      routes(
        [session(FIRST, { state: "idle", available_actions: ["steer"] })],
        {
          [`/api/v1/sessions/${FIRST}/steer`]: {
            status: 409,
            body: { code: "invalid_state", params: {} },
          },
        },
      ),
    );
    renderWithProviders(
      <SessionSection task={item} project="tasks" permissions={[]} />,
    );
    await userEvent.type(
      await screen.findByRole("textbox", { name: "Steer the agent" }),
      "go on",
    );
    const before = requests.filter((r) => r.path === "/api/v1/sessions").length;
    await userEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(
      await screen.findByText(
        "The task or session is no longer in a state that allows this.",
      ),
    ).toBeInTheDocument();
    await waitFor(() => {
      expect(
        requests.filter((r) => r.path === "/api/v1/sessions").length,
      ).toBeGreaterThan(before);
    });
  });

  it("keeps focus inside the dialog and returns it on Escape", async () => {
    fakeApi(routes([session(FIRST)]));
    renderWithProviders(
      <SessionSection task={item} project="tasks" permissions={[]} />,
    );
    const trigger = await screen.findByRole("button", { name: "Stop" });
    await userEvent.click(trigger);
    const dialog = await screen.findByRole("dialog");
    expect(dialog.contains(dialog.ownerDocument.activeElement)).toBe(true);
    for (let i = 0; i < 5; i++) {
      await userEvent.tab();
      expect(dialog.contains(dialog.ownerDocument.activeElement)).toBe(true);
    }
    await userEvent.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(trigger).toHaveFocus();
  });

  it("says when there is no session yet", async () => {
    fakeApi({ "/api/v1/sessions": { body: { items: [] } } });
    renderWithProviders(
      <SessionSection task={item} project="tasks" permissions={[]} />,
    );
    expect(
      await screen.findByText("No agent session yet."),
    ).toBeInTheDocument();
  });
});
