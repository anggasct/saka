import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fakeApi, page } from "@/test/fake-api";
import type { Recorded, Routes } from "@/test/fake-api";
import {
  detail,
  project,
  PROJECT_ID,
  spec,
  SPEC_ID,
  task,
} from "@/test/fixtures";
import { renderWithProviders } from "@/test/render";

import { DeliveryPage } from "./delivery-page";
import { TaskPage } from "./task-page";

const cancelled = task(1, {
  state: "cancelled",
  available_actions: ["redo"],
  version: 5,
});
const REDO_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344b1";
const redone = task(1, {
  id: REDO_ID,
  task_key: "tasks-done/s1.2",
  branch: "saka/tasks-done/s1-2",
  created_at: "2026-10-31T11:00:00.000Z",
  redo_of: { id: cancelled.id, key: cancelled.task_key },
});
const CANCELLED_URL = `/api/v1/work-items/${cancelled.id}`;
const REDO_URL = `${CANCELLED_URL}/actions/redo`;
const WITHDRAW_URL = `/api/v1/specs/${SPEC_ID}/withdraw`;

function routes(extra: Routes = {}): Routes {
  return {
    "/api/v1/projects": { body: page([project]) },
    [`/api/v1/projects/${PROJECT_ID}/specs`]: { body: page([spec]) },
    [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
    [`/api/v1/specs/${spec.id}`]: { body: spec },
    "/api/v1/work-items": { body: page([cancelled, task(2, {})]) },
    [CANCELLED_URL]: { body: detail(cancelled) },
    "/api/v1/sessions": { body: page([]) },
    "/api/v1/permission-requests": { body: page([]) },
    ...extra,
  };
}

function posts(requests: Recorded[], path: string): Recorded[] {
  return requests.filter((r) => r.method === "POST" && r.path === path);
}

describe("redo a cancelled step", () => {
  it("asks for a reason and a note and sends them with the version the page shows", async () => {
    const { requests } = fakeApi(
      routes({ [REDO_URL]: { status: 202, body: redone } }),
    );
    renderWithProviders(<TaskPage workItemId={cancelled.id} />);
    const header = await screen.findByRole("banner");
    await userEvent.click(
      within(header).getByRole("button", { name: "Redo step" }),
    );
    const dialog = screen.getByRole("dialog");
    expect(
      within(dialog).getByRole("heading", { name: "Redo step S1" }),
    ).toBeInTheDocument();
    const submit = within(dialog).getByRole("button", {
      name: "Redo the step",
    });
    expect(submit).toBeDisabled();
    await userEvent.type(
      within(dialog).getByLabelText("Reason"),
      "the fix was wrong",
    );
    await userEvent.type(
      within(dialog).getByLabelText("Note for the agent (optional)"),
      "keep the old route",
    );
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [sent] = posts(requests, REDO_URL);
    expect(sent?.headers.get("If-Match")).toBe('W/"5"');
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
    expect(sent?.body).toEqual({
      reason: "the fix was wrong",
      note: "keep the old route",
    });
  });

  it("sends no note when it is left empty and explains a step that was already redone", async () => {
    const { requests } = fakeApi(
      routes({
        [REDO_URL]: {
          status: 409,
          body: { code: "already_redone", params: {} },
        },
      }),
    );
    renderWithProviders(<TaskPage workItemId={cancelled.id} />);
    const header = await screen.findByRole("banner");
    await userEvent.click(
      within(header).getByRole("button", { name: "Redo step" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "again");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Redo the step" }),
    );
    expect(
      await within(dialog).findByText("This task was already redone."),
    ).toBeInTheDocument();
    expect(posts(requests, REDO_URL)[0]?.body).toEqual({
      reason: "again",
      note: null,
    });
  });

  it("offers no Redo button when the API does not", async () => {
    fakeApi(
      routes({
        [CANCELLED_URL]: {
          body: detail({ ...cancelled, available_actions: [] }),
        },
      }),
    );
    renderWithProviders(<TaskPage workItemId={cancelled.id} />);
    const header = await screen.findByRole("banner");
    expect(
      within(header).queryByRole("button", { name: "Redo step" }),
    ).not.toBeInTheDocument();
  });

  it("links the two attempts to each other", async () => {
    const superseded = {
      ...cancelled,
      state: "superseded",
      available_actions: [],
      redone_by: { id: REDO_ID, key: redone.task_key, state: "queued" },
    };
    fakeApi(routes({ [CANCELLED_URL]: { body: detail(superseded) } }));
    renderWithProviders(<TaskPage workItemId={cancelled.id} />);
    const header = await screen.findByRole("banner");
    expect(within(header).getByText("Redone by")).toBeInTheDocument();
    expect(
      within(header).getByRole("link", { name: "tasks-done/s1.2" }),
    ).toHaveAttribute("href", `/delivery/tasks/${REDO_ID}`);
  });
});

describe("redo in Delivery", () => {
  it("pauses the spec with a banner that offers Redo and Withdraw", async () => {
    const { requests } = fakeApi(
      routes({ [WITHDRAW_URL]: { status: 200, body: spec } }),
    );
    renderWithProviders(<DeliveryPage />);
    expect(
      await screen.findByText(
        "Spec paused: S1 cancelled. Redo the step to continue, or withdraw the spec.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Redo step" }),
    ).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("button", { name: "Withdraw spec" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "dropped");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Withdraw the spec" }),
    );
    await waitFor(() => {
      expect(posts(requests, WITHDRAW_URL)).toHaveLength(1);
    });
    expect(posts(requests, WITHDRAW_URL)[0]?.body).toEqual({
      reason: "dropped",
    });
  });

  it("shows the history of a step as the original and its redo, and no banner once it was redone", async () => {
    const superseded = {
      ...cancelled,
      state: "superseded",
      available_actions: [],
      redone_by: { id: REDO_ID, key: redone.task_key, state: "queued" },
    };
    fakeApi(
      routes({
        "/api/v1/work-items": { body: page([redone, superseded]) },
      }),
    );
    renderWithProviders(<DeliveryPage />);
    expect(await screen.findByText("Redone as")).toBeInTheDocument();
    expect(screen.getByText("Redoes")).toBeInTheDocument();
    expect(
      screen.getAllByRole("link", { name: "tasks-done/s1.2" }).length,
    ).toBeGreaterThan(0);
    expect(screen.queryByText(/Spec paused/)).not.toBeInTheDocument();
    const rows = screen.getAllByRole("listitem");
    expect(rows[0]).toHaveTextContent("Redone as");
  });
});
