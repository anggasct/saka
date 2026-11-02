import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fakeApi, page } from "@/test/fake-api";
import type { Recorded, Route, Routes } from "@/test/fake-api";
import { detail, project, PROJECT_ID, spec, task } from "@/test/fixtures";
import { renderWithProviders } from "@/test/render";

import type { WorkItemDetail } from "./queries";
import { TaskPage } from "./task-page";

const HEAD = "a1b2c3d4e5f6".padEnd(40, "0");

const change = {
  id: "0193f0c4-1b2a-7c3d-9e8f-001122334499",
  number: 12,
  url: "https://github.com/acme/tasks/pull/12",
  state: "open",
  head_sha: HEAD,
  base_branch: "main",
  merge_commit_sha: null,
  merged_at: null,
};

const gate = {
  source: "scm",
  required: true,
  head_sha: HEAD,
  outcome: "passed",
  checks: [],
  observed_at: "2026-10-31T10:04:00.000Z",
};

const ready = detail(
  task(1, {
    state: "in_stage",
    stage: "merge",
    stage_phase: "awaiting_merge",
    change_request: change,
    gates: [gate],
    available_actions: ["merge", "cancel"],
    version: 7,
  }),
);

const blocked = detail(
  task(1, {
    state: "blocked",
    stage: "dev",
    change_request: change,
    blocker: {
      id: "0193f0c4-1b2a-7c3d-9e8f-001122334488",
      code: "session_failed",
      params: {},
      opened_at: "2026-10-31T10:05:00.000Z",
      stage: "dev",
    },
    available_actions: ["retry", "cancel"],
  }),
);

const ID = ready.id;
const DETAIL = `/api/v1/work-items/${ID}`;
const ACCEPTED: Route = { status: 202, body: ready };

function routes(item: () => WorkItemDetail, extra: Routes = {}): Routes {
  return {
    [DETAIL]: () => ({ body: item() }),
    [`/api/v1/specs/${spec.id}`]: { body: spec },
    [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
    "/api/v1/work-items": { body: page([item()]) },
    "/api/v1/sessions": { body: page([]) },
    "/api/v1/permission-requests": { body: page([]) },
    ...extra,
  };
}

function problem(status: number, code: string, params: unknown = {}): Route {
  return { status, body: { code, params } };
}

function detailReads(requests: Recorded[]): number {
  return requests.filter((r) => r.method === "GET" && r.path === DETAIL).length;
}

function posts(requests: Recorded[], action: string): Recorded[] {
  return requests.filter(
    (r) => r.method === "POST" && r.path === `${DETAIL}/actions/${action}`,
  );
}

async function open(name: string) {
  const header = await screen.findByRole("banner");
  await userEvent.click(within(header).getByRole("button", { name }));
  return screen.getByRole("dialog");
}

describe("task actions", () => {
  for (const actions of [
    [],
    ["cancel"],
    ["merge", "cancel"],
    ["retry", "cancel"],
  ]) {
    it(`renders only ${actions.join(", ") || "no"} actions`, async () => {
      fakeApi(routes(() => detail({ ...ready, available_actions: actions })));
      renderWithProviders(<TaskPage workItemId={ID} />);
      const header = await screen.findByRole("banner");
      const has = (name: string) =>
        within(header).queryByRole("button", { name }) !== null;
      expect(has("Merge")).toBe(actions.includes("merge"));
      expect(has("Retry")).toBe(actions.includes("retry"));
      expect(has("Cancel task")).toBe(actions.includes("cancel"));
    });
  }

  it("merges with the version it shows and closes on acceptance", async () => {
    const { requests } = fakeApi(
      routes(() => ready, { [`${DETAIL}/actions/merge`]: ACCEPTED }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const dialog = await open("Merge");
    expect(within(dialog).getByText("PR #12")).toBeInTheDocument();
    expect(within(dialog).getByText("a1b2c3d4e5f6")).toBeInTheDocument();
    expect(within(dialog).getByText("CI on the code host")).toBeInTheDocument();
    expect(within(dialog).getByText("Passed")).toBeInTheDocument();
    const reads = detailReads(requests);
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Merge the pull request" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [sent] = posts(requests, "merge");
    expect(sent?.headers.get("If-Match")).toBe('W/"7"');
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
    expect(sent?.body).toEqual({ reason: null });
    await waitFor(() => {
      expect(detailReads(requests)).toBeGreaterThan(reads);
    });
  });

  it("reloads a stale task and keeps the merge dialog open", async () => {
    let version = 7;
    const { requests } = fakeApi(
      routes(() => detail({ ...ready, version }), {
        [`${DETAIL}/actions/merge`]: (r) => {
          if (r.headers.get("If-Match") === 'W/"7"') {
            version = 8;
            return problem(412, "version_stale");
          }
          return ACCEPTED;
        },
      }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const dialog = await open("Merge");
    const reads = detailReads(requests);
    const submit = within(dialog).getByRole("button", {
      name: "Merge the pull request",
    });
    await userEvent.click(submit);
    expect(
      await within(dialog).findByText(
        "The task changed since it was loaded. Review it and try again.",
      ),
    ).toBeInTheDocument();
    expect(detailReads(requests)).toBeGreaterThan(reads);
    expect(screen.getByRole("dialog")).toBe(dialog);
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [first, second] = posts(requests, "merge");
    expect(second?.headers.get("If-Match")).toBe('W/"8"');
    expect(second?.headers.get("Idempotency-Key")).not.toBe(
      first?.headers.get("Idempotency-Key"),
    );
  });

  it("explains a merge the gate does not allow yet", async () => {
    fakeApi(
      routes(() => ready, {
        [`${DETAIL}/actions/merge`]: problem(409, "merge_not_ready", {
          gate_outcome: "pending",
          head_sha: HEAD,
        }),
      }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const dialog = await open("Merge");
    await userEvent.type(
      within(dialog).getByLabelText("Reason (optional)"),
      "ship",
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Merge the pull request" }),
    );
    expect(
      await within(dialog).findByText(
        "The gate at a1b2c3d4e5f6 is Pending, so the pull request cannot be merged yet.",
      ),
    ).toBeInTheDocument();
  });

  it("retries from the blocker with a reason", async () => {
    const { requests } = fakeApi(
      routes(() => blocked, { [`${DETAIL}/actions/retry`]: ACCEPTED }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const card = await screen.findByRole("region", { name: "Blocker" });
    await userEvent.click(within(card).getByRole("button", { name: "Retry" }));
    const dialog = screen.getByRole("dialog", {
      name: "Retry the blocked stage",
    });
    expect(within(dialog).getByText("Development")).toBeInTheDocument();
    const submit = within(dialog).getByRole("button", {
      name: "Retry the stage",
    });
    expect(submit).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText("Reason"), "   ");
    expect(submit).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText("Reason"), "flaky");
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(posts(requests, "retry").map((r) => r.body)).toEqual([
      { reason: "flaky" },
    ]);
  });

  it("explains the manual retry limit", async () => {
    fakeApi(
      routes(() => blocked, {
        [`${DETAIL}/actions/retry`]: problem(409, "retry_limit_reached", {
          stage: "dev",
          limit: 3,
        }),
      }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const dialog = await open("Retry");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "again");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Retry the stage" }),
    );
    expect(
      await within(dialog).findByText(
        "Stage Development already used its 3 manual retries.",
      ),
    ).toBeInTheDocument();
  });

  it("cancels only with a reason", async () => {
    const { requests } = fakeApi(
      routes(() => ready, { [`${DETAIL}/actions/cancel`]: ACCEPTED }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const dialog = await open("Cancel task");
    expect(
      within(dialog).getByText(/The pull request stays open/),
    ).toBeInTheDocument();
    const submit = within(dialog).getByRole("button", {
      name: "Cancel the task",
    });
    expect(submit).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText("Reason"), "wrong spec");
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(posts(requests, "cancel").map((r) => r.body)).toEqual([
      { reason: "wrong spec" },
    ]);
  });
});
