import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { TaskPage } from "@/features/delivery/task-page";
import { TaskRow } from "@/features/delivery/task-row";
import { fakeApi, page } from "@/test/fake-api";
import type { Recorded, Routes } from "@/test/fake-api";
import {
  DEPLOYMENT_ID,
  detailUrl,
  OTHER_DEPLOYMENT_ID,
} from "@/test/deploy-fixtures";
import { detail, project, PROJECT_ID, spec, task } from "@/test/fixtures";
import { renderWithProviders } from "@/test/render";

const BLOCKER = "0193f0c4-1b2a-7c3d-9e8f-001122334488";

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

const gates = [
  {
    environment: "staging",
    state: "failed",
    reason: null,
    deployment: {
      id: DEPLOYMENT_ID,
      state: "failed",
      sha: "feedface".padEnd(40, "0"),
      phase: "verify",
      finished_at: "2026-11-06T10:03:00.000Z",
    },
  },
  {
    environment: "production",
    state: "accepted",
    reason: "checked by hand",
    deployment: null,
  },
  {
    environment: "preview",
    state: "skipped",
    reason: "environment_removed",
    deployment: {
      id: OTHER_DEPLOYMENT_ID,
      state: "succeeded",
      sha: null,
      phase: null,
      finished_at: null,
    },
  },
];

function blocker(kind: string, params: Record<string, unknown> = {}) {
  return {
    id: BLOCKER,
    code: "deploy_unverified",
    params: {
      environment: "staging",
      deployment_id: DEPLOYMENT_ID,
      phase: "verify",
      code: "verify_failed",
      on_failure: kind === "draft_fix" ? "fix" : "revert",
    },
    opened_at: "2026-10-31T10:05:00.000Z",
    stage: "sync",
    primary_action: { kind, params },
  };
}

function unverified(kind: string, available: string[]) {
  return detail(
    task(1, {
      state: "blocked",
      stage: "sync",
      change_request: MERGED,
      deploy_gates: gates,
      blocker: blocker(
        kind,
        kind === "draft_fix" ? { deployment_id: DEPLOYMENT_ID } : {},
      ),
      available_actions: available,
      version: 4,
    }),
  );
}

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
  return requests.filter((r) => r.method === "POST");
}

describe("deploy evidence on the task", () => {
  it("lists every deploy gate with its deployment", async () => {
    const item = unverified("revert", ["accept_unverified"]);
    fakeApi(routes(item));
    renderWithProviders(<TaskPage workItemId={item.id} />);
    const section = await screen.findByRole("region", {
      name: "Deployments",
    });
    const staging = within(section).getByRole("heading", { name: "staging" });
    const stagingRow = staging.closest("li") as HTMLElement;
    expect(within(stagingRow).getAllByText("Failed")).toHaveLength(2);
    expect(within(stagingRow).getByText("Verification")).toBeInTheDocument();
    expect(within(stagingRow).getByText("feedface0000")).toBeInTheDocument();
    expect(
      within(stagingRow).getByRole("link", { name: "Open the deployment" }),
    ).toHaveAttribute(
      "href",
      `/deployments/${PROJECT_ID}?environment=staging&deployment=${DEPLOYMENT_ID}`,
    );
    const production = within(section)
      .getByRole("heading", { name: "production" })
      .closest("li") as HTMLElement;
    expect(
      within(production).getByText("Accepted without verification"),
    ).toBeInTheDocument();
    expect(
      within(production).getByText("Not deployed yet"),
    ).toBeInTheDocument();
    expect(
      within(production).getByText("Reason: checked by hand"),
    ).toBeInTheDocument();
    const preview = within(section)
      .getByRole("heading", { name: "preview" })
      .closest("li") as HTMLElement;
    expect(
      within(preview).getByText("Reason: The environment was removed."),
    ).toBeInTheDocument();
  });

  it("shows no section for a task without deploy gates", async () => {
    const item = detail(task(1, { state: "queued" }));
    fakeApi(routes(item));
    renderWithProviders(<TaskPage workItemId={item.id} />);
    await screen.findByRole("banner");
    expect(
      screen.queryByRole("region", { name: "Deployments" }),
    ).not.toBeInTheDocument();
  });

  it("shows a badge per gate on the task row of a spec", async () => {
    const item = task(1, { state: "blocked", deploy_gates: gates });
    renderWithProviders(
      <ul>
        <TaskRow task={item} tasks={[item]} />
      </ul>,
    );
    expect(await screen.findByText("staging: Failed")).toBeInTheDocument();
    expect(
      screen.getByText("production: Accepted without verification"),
    ).toBeInTheDocument();
  });
});

describe("deploy actions on the task", () => {
  it("accepts the change without verification for the open blocker", async () => {
    const item = unverified("revert", ["revert", "accept_unverified"]);
    const { requests } = fakeApi(
      routes(item, {
        [`/api/v1/work-items/${item.id}/actions/accept-unverified`]: {
          status: 202,
          body: item,
        },
      }),
    );
    renderWithProviders(<TaskPage workItemId={item.id} />);
    const header = await screen.findByRole("banner");
    await userEvent.click(
      within(header).getByRole("button", {
        name: "Accept without verification",
      }),
    );
    const dialog = screen.getByRole("dialog");
    const submit = within(dialog).getByRole("button", {
      name: "Accept the change",
    });
    expect(submit).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText("Reason"), "checked");
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [sent] = posts(requests);
    expect(sent?.body).toEqual({ blocker_id: BLOCKER, reason: "checked" });
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("explains a blocker that is no longer open", async () => {
    const item = unverified("revert", ["accept_unverified"]);
    fakeApi(
      routes(item, {
        [`/api/v1/work-items/${item.id}/actions/accept-unverified`]: () => ({
          status: 409,
          body: { code: "blocker_not_open", params: {} },
        }),
      }),
    );
    renderWithProviders(<TaskPage workItemId={item.id} />);
    const header = await screen.findByRole("banner");
    await userEvent.click(
      within(header).getByRole("button", {
        name: "Accept without verification",
      }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "checked");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Accept the change" }),
    );
    expect(
      await within(dialog).findByText(
        "This blocker is no longer open; the task has moved on.",
      ),
    ).toBeInTheDocument();
  });

  it("offers no accept without the action", async () => {
    const item = unverified("revert", ["revert"]);
    fakeApi(routes(item));
    renderWithProviders(<TaskPage workItemId={item.id} />);
    await screen.findByRole("banner");
    expect(
      screen.queryByRole("button", { name: "Accept without verification" }),
    ).not.toBeInTheDocument();
  });

  it("runs the revert primary action against the blocker", async () => {
    const item = unverified("revert", ["revert", "accept_unverified"]);
    const { requests } = fakeApi(
      routes(item, {
        [`/api/v1/work-items/${item.id}/actions/primary`]: {
          status: 202,
          body: item,
        },
      }),
    );
    renderWithProviders(<TaskPage workItemId={item.id} />);
    const alert = await screen.findByRole("region", { name: "Blocker" });
    await userEvent.click(
      within(alert).getByRole("button", { name: "Revert" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.type(
      within(dialog).getByLabelText("Reason"),
      "staging is red",
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Revert the task" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [sent] = posts(requests);
    expect(sent?.path).toBe(`/api/v1/work-items/${item.id}/actions/primary`);
    expect(sent?.body).toEqual({
      blocker_id: BLOCKER,
      reason: "staging is red",
    });
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("drafts a fix of the deployment the blocker names", async () => {
    const item = unverified("draft_fix", ["accept_unverified"]);
    const { requests } = fakeApi(
      routes(item, {
        [`${detailUrl(DEPLOYMENT_ID)}/actions/draft-fix`]: {
          status: 202,
          body: { id: "0193f0c4-1b2a-7c3d-9e8f-0011223344d3" },
        },
      }),
    );
    renderWithProviders(<TaskPage workItemId={item.id} />);
    const alert = await screen.findByRole("region", { name: "Blocker" });
    await userEvent.click(
      within(alert).getByRole("button", { name: "Draft a fix" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Draft the fix" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [sent] = posts(requests);
    expect(sent?.path).toBe(`${detailUrl(DEPLOYMENT_ID)}/actions/draft-fix`);
    expect(sent?.body).toEqual({ note: null });
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });
});
