import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fakeApi, page, pending } from "@/test/fake-api";
import {
  blockedTask,
  outsideTask,
  project,
  PROJECT_ID,
  queuedTask,
  spec,
  SPEC_ID,
  supersededTask,
  task,
} from "@/test/fixtures";
import { renderWithProviders } from "@/test/render";

import { DeliveryPage } from "./delivery-page";

const PROJECTS = "/api/v1/projects";
const SPECS = `/api/v1/projects/${PROJECT_ID}/specs`;
const WORK_ITEMS = "/api/v1/work-items";
const PERMISSIONS = "/api/v1/permission-requests";

function readyRoutes() {
  return {
    [PROJECTS]: { body: page([project]) },
    [SPECS]: { body: page([spec]) },
    [WORK_ITEMS]: { body: page([queuedTask, blockedTask]) },
    [PERMISSIONS]: { body: page([]) },
  };
}

describe("DeliveryPage", () => {
  it("shows a busy skeleton while loading", async () => {
    fakeApi({ [PROJECTS]: pending, [PERMISSIONS]: pending });
    renderWithProviders(<DeliveryPage />);
    const busy = await screen.findByRole("status", {
      name: "Loading delivery",
    });
    expect(busy).toHaveAttribute("aria-busy", "true");
  });

  it("shows how to start when nothing is approved", async () => {
    fakeApi({
      [PROJECTS]: { body: page([]) },
      [PERMISSIONS]: { body: page([]) },
    });
    renderWithProviders(<DeliveryPage />);
    expect(
      await screen.findByRole("heading", { name: "Nothing to deliver yet" }),
    ).toBeInTheDocument();
    expect(screen.getByText(/saka project add/)).toBeInTheDocument();
    expect(screen.getByText(/saka spec approve/)).toBeInTheDocument();
  });

  it("shows the error from its code and retries", async () => {
    let fail = true;
    fakeApi({
      ...readyRoutes(),
      [PROJECTS]: () =>
        fail
          ? { status: 500, body: { code: "internal", params: {} } }
          : { body: page([project]) },
    });
    renderWithProviders(<DeliveryPage />);
    const alert = await screen.findByRole("alert");
    expect(
      within(alert).getByText("Something went wrong on the server."),
    ).toBeInTheDocument();
    fail = false;
    await userEvent.click(
      within(alert).getByRole("button", { name: "Try again" }),
    );
    expect(
      await screen.findByRole("heading", { name: "tasks" }),
    ).toBeInTheDocument();
  });

  it("lists projects, specs, and tasks with their blockers in English", async () => {
    fakeApi(readyRoutes());
    renderWithProviders(<DeliveryPage />);
    expect(
      await screen.findByRole("heading", { name: "Mark tasks as done" }),
    ).toBeInTheDocument();
    const items = await screen.findAllByRole("listitem");
    expect(items).toHaveLength(2);
    const [first, second] = items;
    expect(first).toBeDefined();
    expect(second).toBeDefined();
    if (first === undefined || second === undefined) return;
    expect(within(first).getByText("Step S1")).toBeInTheDocument();
    expect(within(first).getByText("Blocked")).toBeInTheDocument();
    expect(
      within(first).getByText("CI failed: test, lint."),
    ).toBeInTheDocument();
    expect(within(first).getByText("ci_red")).toBeInTheDocument();
    expect(within(first).getByRole("link", { name: "PR #12" })).toHaveAttribute(
      "href",
      "https://github.com/acme/tasks/pull/12",
    );
    expect(within(second).getByText("Queued")).toBeInTheDocument();
  });

  it("renders the same blocker through the Indonesian catalog", async () => {
    fakeApi(readyRoutes());
    renderWithProviders(<DeliveryPage />, "id");
    expect(
      await screen.findByText("CI gagal: test, lint."),
    ).toBeInTheDocument();
    expect(screen.getByText("Terhenti")).toBeInTheDocument();
  });

  it("labels a superseded task from the catalog", async () => {
    fakeApi({
      ...readyRoutes(),
      [WORK_ITEMS]: { body: page([supersededTask]) },
    });
    const { unmount } = renderWithProviders(<DeliveryPage />);
    expect(await screen.findByText("Superseded")).toBeInTheDocument();
    expect(screen.queryByText("superseded")).not.toBeInTheDocument();
    unmount();
    renderWithProviders(<DeliveryPage />, "id");
    expect(await screen.findByText("Digantikan")).toBeInTheDocument();
  });

  it("renders a blocker code it does not know with the code", async () => {
    const routes = readyRoutes();
    fakeApi({
      ...routes,
      [WORK_ITEMS]: {
        body: page([
          {
            ...blockedTask,
            blocker: { ...blockedTask.blocker, code: "future_code" },
          },
        ]),
      },
    });
    renderWithProviders(<DeliveryPage />);
    await waitFor(() => {
      expect(screen.getByText("Stopped (future_code).")).toBeInTheDocument();
    });
  });

  it("links each task to its page and says what it waits for", async () => {
    const waiting = task(2, {
      waiting_on: [
        { kind: "step", step_id: "S1", work_item_id: blockedTask.id },
        { kind: "spec", slug: "tasks-store", spec_id: SPEC_ID },
        { kind: "port_block" },
      ],
      pending_permission_count: 1,
    });
    const quota = task(3, {
      waiting_on: [
        {
          kind: "quota",
          account: "claude:default",
          window: "five_hour",
          utilization: 0.97,
          resets_at: null,
        },
      ],
    });
    const ports = task(4, { waiting_on: [{ kind: "port_block" }] });
    const other = {
      ...task(5, { spec_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344dd" }),
      waiting_on: [
        { kind: "step" as const, step_id: "S1", work_item_id: blockedTask.id },
      ],
    };
    fakeApi({
      ...readyRoutes(),
      [WORK_ITEMS]: {
        body: page([blockedTask, waiting, quota, ports, other]),
      },
    });
    renderWithProviders(<DeliveryPage />);
    const link = await screen.findByRole("link", { name: "Step S1" });
    expect(link).toHaveAttribute("href", `/delivery/tasks/${blockedTask.id}`);
    const rows = screen.getAllByRole("listitem");
    const row = (name: string) => {
      const found = rows.find(
        (r) => within(r).queryByRole("link", { name }) !== null,
      );
      if (found === undefined) {
        throw new Error(`no row for ${name}`);
      }
      return found;
    };
    expect(
      within(row("Step S1")).getByText("Holds 2 waiting steps"),
    ).toBeInTheDocument();
    expect(
      within(row("Step S2")).getByText(/Waiting for step S1 to be done\./),
    ).toHaveTextContent("Waiting for step S1 to be done. +2 more");
    expect(
      within(row("Step S2")).getByText("1 permission request waiting"),
    ).toBeInTheDocument();
    expect(
      within(row("Step S3")).getByText(
        "Waiting for the agent account claude:default: its five_hour window is at 97%.",
      ),
    ).toBeInTheDocument();
    expect(
      within(row("Step S4")).getByText("Waiting for a free port block."),
    ).toBeInTheDocument();
  });

  it("dims tasks that will not run", async () => {
    fakeApi({
      ...readyRoutes(),
      [WORK_ITEMS]: {
        body: page([supersededTask, task(4, { state: "cancelled" })]),
      },
    });
    renderWithProviders(<DeliveryPage />);
    const rows = await screen.findAllByRole("listitem");
    expect(rows).toHaveLength(2);
    for (const r of rows) {
      expect(r).toHaveClass("text-muted-foreground");
    }
  });
});

describe("Changes from outside", () => {
  it("lists them under their own heading, marks the ones with no spec, and works without any spec", async () => {
    const reviewOnly = {
      ...project,
      workflow: {
        preset: "review_only",
        spec: false,
        change_request: {
          trigger: "auto",
          labels: [],
          authors: [],
          branches: [],
          paths: [],
          include_forks: false,
          include_drafts: false,
          trigger_label: "saka:review",
        },
        stages: [{ stage: "review", trigger: "auto" }],
      },
    };
    fakeApi({
      [PROJECTS]: { body: page([reviewOnly]) },
      [SPECS]: { body: page([]) },
      [WORK_ITEMS]: { body: page([outsideTask]) },
      [PERMISSIONS]: { body: page([]) },
    });
    renderWithProviders(<DeliveryPage />);
    const group = await screen.findByRole("region", {
      name: "Changes from outside",
    });
    const link = within(group).getByRole("link", { name: "app#7" });
    expect(link).toHaveAttribute("href", `/delivery/tasks/${outsideTask.id}`);
    expect(
      within(group).getByText("Add retries to the poller"),
    ).toBeInTheDocument();
    expect(within(group).getByText("by alice")).toBeInTheDocument();
    expect(within(group).getByText("No spec")).toBeInTheDocument();
    expect(within(group).getByText("Fork")).toBeInTheDocument();
    expect(
      within(group).getByText(/waiting for a review command/),
    ).toBeInTheDocument();
    expect(screen.queryByText(/No specs approved/)).not.toBeInTheDocument();
  });

  it("names the spec a change is linked to instead of marking it", async () => {
    const linked = {
      ...outsideTask,
      external: {
        ...outsideTask.external,
        author: "alice",
        title: outsideTask.work,
        from_fork: false,
        draft: false,
        cause: "auto",
        actor: null,
        spec: { spec_id: SPEC_ID, slug: "tasks-done", step_id: "S2" },
      },
    };
    fakeApi({
      [PROJECTS]: { body: page([project]) },
      [SPECS]: { body: page([spec]) },
      [WORK_ITEMS]: { body: page([linked]) },
      [PERMISSIONS]: { body: page([]) },
    });
    renderWithProviders(<DeliveryPage />);
    const group = await screen.findByRole("region", {
      name: "Changes from outside",
    });
    expect(within(group).getByText("Spec tasks-done")).toBeInTheDocument();
    expect(within(group).queryByText("No spec")).not.toBeInTheDocument();
  });
});
