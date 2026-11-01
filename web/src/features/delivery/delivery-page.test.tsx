import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fakeApi, page, pending } from "@/test/fake-api";
import {
  blockedTask,
  project,
  PROJECT_ID,
  queuedTask,
  spec,
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
    [PERMISSIONS]: { body: page([{ id: "p" }, { id: "q" }]) },
  };
}

describe("DeliveryPage", () => {
  it("shows a busy skeleton while loading", () => {
    fakeApi({ [PROJECTS]: pending, [PERMISSIONS]: pending });
    renderWithProviders(<DeliveryPage />);
    const busy = screen.getByRole("status", { name: "Loading delivery" });
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
    expect(
      screen.getByText("2 permission requests waiting"),
    ).toBeInTheDocument();
  });

  it("renders the same blocker through the Indonesian catalog", async () => {
    fakeApi(readyRoutes());
    renderWithProviders(<DeliveryPage />, "id");
    expect(
      await screen.findByText("CI gagal: test, lint."),
    ).toBeInTheDocument();
    expect(screen.getByText("Terhenti")).toBeInTheDocument();
    expect(screen.getByText("2 permintaan izin menunggu")).toBeInTheDocument();
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
});
