import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fakeApi, page } from "@/test/fake-api";
import type { Recorded, Routes } from "@/test/fake-api";
import {
  detail,
  project,
  PROJECT_ID,
  REVERT_ID,
  revertTask,
  spec,
  SPEC_ID,
  task,
} from "@/test/fixtures";
import { renderWithProviders } from "@/test/render";

import { DeliveryPage } from "./delivery-page";
import { TaskPage } from "./task-page";

const MERGED = {
  id: "0193f0c4-1b2a-7c3d-9e8f-001122334499",
  number: 12,
  url: "https://github.com/acme/tasks/pull/12",
  state: "merged",
  head_sha: "a1b2c3d4e5f6".padEnd(40, "0"),
  base_branch: "main",
  merge_commit_sha: "feedface".padEnd(40, "0"),
  merged_at: "2026-10-31T10:09:00.000Z",
};

const done = task(1, {
  state: "done",
  change_request: MERGED,
  available_actions: ["revert"],
  version: 9,
});
const DONE_URL = `/api/v1/work-items/${done.id}`;
const REVERT_URL = `${DONE_URL}/actions/revert`;

function routes(item: ReturnType<typeof detail>, extra: Routes = {}): Routes {
  return {
    [`/api/v1/work-items/${item.id}`]: { body: item },
    [`/api/v1/specs/${spec.id}`]: { body: spec },
    [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
    "/api/v1/work-items": { body: page([item]) },
    "/api/v1/sessions": { body: page([]) },
    "/api/v1/permission-requests": { body: page([]) },
    ...extra,
  };
}

function posts(requests: Recorded[]): Recorded[] {
  return requests.filter((r) => r.method === "POST" && r.path === REVERT_URL);
}

async function openRevert() {
  const header = await screen.findByRole("banner");
  await userEvent.click(within(header).getByRole("button", { name: "Revert" }));
  return screen.getByRole("dialog");
}

describe("revert action", () => {
  it("asks for a reason and sends it with the version the page shows", async () => {
    const { requests } = fakeApi(
      routes(detail(done), {
        [REVERT_URL]: { status: 202, body: revertTask },
      }),
    );
    renderWithProviders(<TaskPage workItemId={done.id} />);
    const dialog = await openRevert();
    expect(within(dialog).getByText("PR #12")).toBeInTheDocument();
    expect(within(dialog).getByText("feedface0000")).toBeInTheDocument();
    const submit = within(dialog).getByRole("button", {
      name: "Revert the task",
    });
    await userEvent.type(within(dialog).getByLabelText("Reason"), "broke CI");
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [sent] = posts(requests);
    expect(sent?.headers.get("If-Match")).toBe('W/"9"');
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
    expect(sent?.body).toEqual({ reason: "broke CI" });
  });

  it("explains that a revert is already running", async () => {
    fakeApi(
      routes(detail(done), {
        [REVERT_URL]: {
          status: 409,
          body: { code: "revert_in_progress", params: {} },
        },
      }),
    );
    renderWithProviders(<TaskPage workItemId={done.id} />);
    const dialog = await openRevert();
    await userEvent.type(within(dialog).getByLabelText("Reason"), "again");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Revert the task" }),
    );
    expect(
      await within(dialog).findByText(
        "This task already has a revert in progress.",
      ),
    ).toBeInTheDocument();
  });

  it("offers no Revert button when the API does not", async () => {
    fakeApi(routes(detail({ ...done, available_actions: [] })));
    renderWithProviders(<TaskPage workItemId={done.id} />);
    const header = await screen.findByRole("banner");
    expect(
      within(header).queryByRole("button", { name: "Revert" }),
    ).not.toBeInTheDocument();
  });
});

describe("revert links", () => {
  it("links the original task to the revert that undoes it", async () => {
    const reverted = {
      ...done,
      available_actions: [],
      reverted_by: { id: REVERT_ID, key: "tasks-done/s1-R1", state: "queued" },
    };
    fakeApi(routes(detail(reverted)));
    renderWithProviders(<TaskPage workItemId={done.id} />);
    const header = await screen.findByRole("banner");
    expect(within(header).getByText("Reverted by")).toBeInTheDocument();
    expect(
      within(header).getByRole("link", { name: "tasks-done/s1-R1" }),
    ).toHaveAttribute("href", `/delivery/tasks/${REVERT_ID}`);
    expect(within(header).getByText("Queued")).toBeInTheDocument();
  });

  it("links the revert task to the task it reverts and explains its blocker", async () => {
    fakeApi(routes(detail(revertTask)));
    renderWithProviders(<TaskPage workItemId={REVERT_ID} />);
    const header = await screen.findByRole("banner");
    expect(
      within(header).getByRole("heading", { name: "tasks-done/s1-R1" }),
    ).toBeInTheDocument();
    expect(within(header).getByText("Revert")).toBeInTheDocument();
    expect(
      within(header).getByRole("link", { name: "tasks-done/s1" }),
    ).toHaveAttribute("href", `/delivery/tasks/${revertTask.revert_of?.id}`);
    expect(
      await screen.findByText(
        /The revert conflicts with later changes in 1 file: src\/a\.rs/,
      ),
    ).toBeInTheDocument();
  });
});

describe("revert in Delivery", () => {
  it("lists revert tasks in their own group and marks the reverted step", async () => {
    const reverted = {
      ...done,
      spec_id: SPEC_ID,
      available_actions: [],
      reverted_by: { id: REVERT_ID, key: "tasks-done/s1-R1", state: "done" },
    };
    fakeApi({
      "/api/v1/projects": { body: page([project]) },
      [`/api/v1/projects/${PROJECT_ID}/specs`]: { body: page([spec]) },
      "/api/v1/work-items": { body: page([reverted, revertTask]) },
      "/api/v1/permission-requests": { body: page([]) },
    });
    renderWithProviders(<DeliveryPage />);
    const group = await screen.findByRole("region", { name: "Reverts" });
    expect(
      within(group).getByRole("link", { name: "tasks-done/s1-R1" }),
    ).toBeInTheDocument();
    expect(
      within(group).getByText("Reverts tasks-done/s1"),
    ).toBeInTheDocument();
    const marks = screen.getAllByText("Reverted");
    expect(marks.length).toBeGreaterThan(0);
  });
});
