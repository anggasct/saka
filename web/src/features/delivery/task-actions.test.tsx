import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fakeApi, page } from "@/test/fake-api";
import type { Recorded, Route, Routes } from "@/test/fake-api";
import {
  detail,
  outsideTask,
  project,
  PROJECT_ID,
  spec,
  task,
} from "@/test/fixtures";
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

const BLOCKER = "0193f0c4-1b2a-7c3d-9e8f-001122334488";

const BASE_BLOCKER = {
  id: BLOCKER,
  code: "session_failed",
  params: {},
  opened_at: "2026-10-31T10:05:00.000Z",
  stage: "dev",
  primary_action: { kind: "retry", params: {} },
};

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
      primary_action: { kind: "retry", params: {} },
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

  it("offers Start on a step that waits and sends the version it shows", async () => {
    const waiting = {
      ...ready,
      state: "awaiting_start",
      stage: null,
      stage_phase: null,
      available_actions: ["start", "cancel"],
    };
    const { requests } = fakeApi(
      routes(() => detail(waiting), { [`${DETAIL}/actions/start`]: ACCEPTED }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    expect(
      await screen.findByText("Waiting for you to start this step."),
    ).toBeInTheDocument();
    const dialog = await open("Start");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Start step" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [sent] = posts(requests, "start");
    expect(sent?.headers.get("If-Match")).toBe('W/"7"');
    expect(sent?.body).toEqual({});
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
      routes(() => blocked, { [`${DETAIL}/actions/primary`]: ACCEPTED }),
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
    expect(posts(requests, "retry")).toEqual([]);
    expect(posts(requests, "primary").map((r) => r.body)).toEqual([
      { blocker_id: BLOCKER, reason: "flaky" },
    ]);
  });

  it("offers the primary action once on a blocked task and the other action in the header", async () => {
    fakeApi(routes(() => blocked));
    renderWithProviders(<TaskPage workItemId={ID} />);
    const card = await screen.findByRole("region", { name: "Blocker" });
    const header = await screen.findByRole("banner");
    expect(within(card).getAllByRole("button")).toHaveLength(1);
    expect(
      within(header).queryByRole("button", { name: "Retry" }),
    ).not.toBeInTheDocument();
    expect(
      within(header).getByRole("button", { name: "Cancel task" }),
    ).toBeInTheDocument();
  });

  it("cancels from the blocker when cancel is the primary action", async () => {
    const closed = detail({
      ...blocked,
      available_actions: ["cancel"],
      blocker: {
        ...BASE_BLOCKER,
        code: "change_closed",
        params: { number: 12 },
        primary_action: { kind: "cancel", params: {} },
      },
    });
    const { requests } = fakeApi(
      routes(() => closed, { [`${DETAIL}/actions/primary`]: ACCEPTED }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const card = await screen.findByRole("region", { name: "Blocker" });
    expect(
      within(card).getByText("The pull request was closed without merging."),
    ).toBeInTheDocument();
    const header = await screen.findByRole("banner");
    expect(within(header).queryByRole("button")).not.toBeInTheDocument();
    await userEvent.click(
      within(card).getByRole("button", { name: "Cancel task" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "closed");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Cancel the task" }),
    );
    await waitFor(() => {
      expect(posts(requests, "primary")).toHaveLength(1);
    });
    expect(posts(requests, "primary")[0]?.body).toEqual({
      blocker_id: BLOCKER,
      reason: "closed",
    });
  });

  for (const [kind, code, href, name] of [
    [
      "set_secret",
      "secret_missing",
      `/settings/${PROJECT_ID}`,
      "Set the secrets",
    ],
    [
      "link_account",
      "user_link_required",
      `/settings/${PROJECT_ID}`,
      "Link the account",
    ],
    [
      "set_user_token",
      "user_token_invalid",
      `/settings/${PROJECT_ID}`,
      "Store a token",
    ],
    [
      "edit_spec",
      "spec_unclear",
      `/docs/${PROJECT_ID}?path=${encodeURIComponent(spec.path)}`,
      "Edit the spec",
    ],
  ] as const) {
    it(`sends ${kind} to where the operator can do it`, async () => {
      const guided = detail({
        ...blocked,
        blocker: {
          ...BASE_BLOCKER,
          code,
          params: { names: ["NPM_TOKEN"], cause: "missing" },
          primary_action: { kind, params: {} },
        },
      });
      fakeApi(routes(() => guided));
      renderWithProviders(<TaskPage workItemId={ID} />);
      const card = await screen.findByRole("region", { name: "Blocker" });
      expect(await within(card).findByRole("link", { name })).toHaveAttribute(
        "href",
        href,
      );
      expect(within(card).queryByRole("button")).not.toBeInTheDocument();
    });
  }

  it("says the blocker is gone when the primary action is late", async () => {
    const { requests } = fakeApi(
      routes(() => blocked, {
        [`${DETAIL}/actions/primary`]: problem(409, "blocker_not_open", {
          blocker_id: BLOCKER,
        }),
      }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const card = await screen.findByRole("region", { name: "Blocker" });
    await userEvent.click(within(card).getByRole("button", { name: "Retry" }));
    const dialog = screen.getByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "late");
    const reads = detailReads(requests);
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Retry the stage" }),
    );
    expect(
      await within(dialog).findByText(
        "This blocker is no longer open; the task has moved on.",
      ),
    ).toBeInTheDocument();
    await waitFor(() => {
      expect(detailReads(requests)).toBeGreaterThan(reads);
    });
  });

  it("explains the manual retry limit", async () => {
    fakeApi(
      routes(() => blocked, {
        [`${DETAIL}/actions/primary`]: problem(409, "retry_limit_reached", {
          stage: "dev",
          limit: 3,
        }),
      }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const card = await screen.findByRole("region", { name: "Blocker" });
    await userEvent.click(within(card).getByRole("button", { name: "Retry" }));
    const dialog = screen.getByRole("dialog");
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

describe("fix request", () => {
  const rejected = detail({
    ...outsideTask,
    stage_phase: "reviewed",
    available_actions: ["cancel", "fix"],
  });
  const FIX = `/api/v1/work-items/${rejected.id}`;

  it("asks for a fix round of the shown head with one key and closes", async () => {
    const { requests } = fakeApi({
      [FIX]: { body: rejected },
      [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
      "/api/v1/work-items": { body: page([rejected]) },
      "/api/v1/sessions": { body: page([]) },
      "/api/v1/permission-requests": { body: page([]) },
      [`${FIX}/actions/fix`]: { status: 202, body: rejected },
    });
    renderWithProviders(<TaskPage workItemId={rejected.id} />);
    const header = await screen.findByRole("banner");
    await userEvent.click(within(header).getByRole("button", { name: "Fix" }));
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByText("PR #7")).toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Start a fix round" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const sent = requests.filter(
      (r) => r.method === "POST" && r.path === `${FIX}/actions/fix`,
    );
    expect(sent).toHaveLength(1);
    expect(sent[0]?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
    expect(sent[0]?.body).toEqual({});
  });

  it("says why a fix cannot start and keeps the dialog open", async () => {
    fakeApi({
      [FIX]: { body: rejected },
      [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
      "/api/v1/work-items": { body: page([rejected]) },
      "/api/v1/sessions": { body: page([]) },
      "/api/v1/permission-requests": { body: page([]) },
      [`${FIX}/actions/fix`]: problem(409, "fix_not_available", {
        reason: "rounds_exhausted",
      }),
    });
    renderWithProviders(<TaskPage workItemId={rejected.id} />);
    const header = await screen.findByRole("banner");
    await userEvent.click(within(header).getByRole("button", { name: "Fix" }));
    const dialog = screen.getByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Start a fix round" }),
    );
    expect(
      await within(dialog).findByText(
        "The fix rounds of this pull request are spent.",
      ),
    ).toBeInTheDocument();
  });
});

describe("review request", () => {
  const waiting = detail(outsideTask);
  const REVIEW = `/api/v1/work-items/${waiting.id}`;

  it("asks for a review of the shown head with one key and closes", async () => {
    const { requests } = fakeApi({
      [REVIEW]: { body: waiting },
      [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
      "/api/v1/work-items": { body: page([waiting]) },
      "/api/v1/sessions": { body: page([]) },
      "/api/v1/permission-requests": { body: page([]) },
      [`${REVIEW}/actions/review`]: { status: 202, body: waiting },
    });
    renderWithProviders(<TaskPage workItemId={waiting.id} />);
    expect(
      await screen.findByRole("heading", { name: "app#7" }),
    ).toBeInTheDocument();
    expect(screen.getByText("No spec")).toBeInTheDocument();
    const header = screen.getByRole("banner");
    await userEvent.click(
      within(header).getByRole("button", { name: "Review" }),
    );
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByText("PR #7")).toBeInTheDocument();
    expect(within(dialog).getByText("c0ffee000000")).toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Ask for a review" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const sent = requests.filter(
      (r) => r.method === "POST" && r.path === `${REVIEW}/actions/review`,
    );
    expect(sent).toHaveLength(1);
    expect(sent[0]?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
    expect(sent[0]?.body).toEqual({});
  });

  it("says a head already has a review and keeps the dialog open", async () => {
    fakeApi({
      [REVIEW]: { body: waiting },
      [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
      "/api/v1/work-items": { body: page([waiting]) },
      "/api/v1/sessions": { body: page([]) },
      "/api/v1/permission-requests": { body: page([]) },
      [`${REVIEW}/actions/review`]: problem(409, "review_already_requested", {
        head_sha: "c0ffee".padEnd(40, "0"),
      }),
    });
    renderWithProviders(<TaskPage workItemId={waiting.id} />);
    const header = await screen.findByRole("banner");
    await userEvent.click(
      within(header).getByRole("button", { name: "Review" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Ask for a review" }),
    );
    expect(
      await within(dialog).findByText(
        "A review is already requested at c0ffee000000.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  it("takes a task over with a reason", async () => {
    const running = detail(
      task(1, {
        state: "in_stage",
        stage: "dev",
        stage_phase: "running_session",
        available_actions: ["cancel", "takeover"],
      }),
    );
    const { requests } = fakeApi(
      routes(() => running, { [`${DETAIL}/actions/takeover`]: ACCEPTED }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const dialog = await open("Take over");
    const submit = within(dialog).getByRole("button", {
      name: "Take the task over",
    });
    expect(submit).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText("Reason"), "by hand");
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(posts(requests, "takeover").map((r) => r.body)).toEqual([
      { reason: "by hand" },
    ]);
  });

  it("shows where a taken over task is worked on and returns it at the stage it left", async () => {
    const taken = detail(
      task(1, {
        state: "taken_over",
        stage: null,
        stage_phase: null,
        change_request: change,
        available_actions: ["cancel", "return"],
      }),
      {
        stage_runs: [
          {
            id: "0193f0c4-1b2a-7c3d-9e8f-001122334477",
            attempt: 1,
            stage: "merge",
            state: "cancelled",
            trigger: "auto",
          },
        ],
      },
    );
    const { requests } = fakeApi(
      routes(() => taken, { [`${DETAIL}/actions/return`]: ACCEPTED }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const banner = await screen.findByRole("region", { name: "Taken over" });
    expect(within(banner).getByText(taken.branch)).toBeInTheDocument();
    const dialog = await open("Return to the loop");
    const stage = within(dialog).getByLabelText("Stage to start");
    expect(stage).toHaveValue("merge");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "fixed");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Return to the loop" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(posts(requests, "return").map((r) => r.body)).toEqual([
      { stage: "merge", reason: "fixed" },
    ]);
  });

  it("offers only the development stage when the task has no pull request yet", async () => {
    const taken = detail(
      task(1, {
        state: "taken_over",
        stage: null,
        stage_phase: null,
        available_actions: ["cancel", "return"],
      }),
    );
    fakeApi(routes(() => taken));
    renderWithProviders(<TaskPage workItemId={ID} />);
    const dialog = await open("Return to the loop");
    const stage = within(dialog).getByLabelText("Stage to start");
    expect(stage).toHaveValue("dev");
    expect(within(stage).getAllByRole("option")).toHaveLength(1);
  });

  it("offers a review of the pull request, and a fix only after a rejection at its current head", async () => {
    const verdict = (outcome: string, current: boolean) => ({
      id: "0193f0c4-1b2a-7c3d-9e8f-001122339901",
      outcome,
      basis: "step",
      head_sha: HEAD,
      current,
      summary: "Needs work.",
      finding_count: 1,
      posted: null,
      created_at: "2026-10-31T10:06:00.000Z",
    });
    const takenOver = (review: ReturnType<typeof verdict>) =>
      detail(
        task(1, {
          state: "taken_over",
          stage: null,
          stage_phase: null,
          change_request: change,
          available_actions: ["cancel", "return"],
          review,
        }),
      );
    const options = async (item: WorkItemDetail) => {
      fakeApi(routes(() => item));
      const { unmount } = renderWithProviders(<TaskPage workItemId={ID} />);
      const dialog = await open("Return to the loop");
      const values = within(within(dialog).getByLabelText("Stage to start"))
        .getAllByRole("option")
        .map((option) => option.getAttribute("value"));
      unmount();
      return values;
    };
    expect(
      await options(takenOver(verdict("changes_requested", true))),
    ).toEqual(["dev", "review", "fix", "merge"]);
    expect(await options(takenOver(verdict("approved", true)))).toEqual([
      "dev",
      "review",
      "merge",
    ]);
    expect(
      await options(takenOver(verdict("changes_requested", false))),
    ).toEqual(["dev", "review", "merge"]);
  });

  it("takes over from the blocker when someone pushed to the branch", async () => {
    const moved = detail(
      task(1, {
        state: "blocked",
        stage: "dev",
        change_request: change,
        blocker: {
          ...BASE_BLOCKER,
          code: "branch_moved",
          params: { number: 12, head_sha: HEAD, previous_head_sha: HEAD },
          primary_action: { kind: "takeover", params: {} },
        },
        available_actions: ["retry", "cancel", "takeover"],
      }),
    );
    const { requests } = fakeApi(
      routes(() => moved, { [`${DETAIL}/actions/primary`]: ACCEPTED }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const card = await screen.findByRole("region", { name: "Blocker" });
    expect(
      within(card).getByText(/Someone pushed to the task branch/),
    ).toBeInTheDocument();
    const header = await screen.findByRole("banner");
    expect(
      within(header).queryByRole("button", { name: "Take over" }),
    ).not.toBeInTheDocument();
    await userEvent.click(
      within(card).getByRole("button", { name: "Take over" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "mine");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Take the task over" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(posts(requests, "primary").map((r) => r.body)).toEqual([
      { blocker_id: BLOCKER, reason: "mine" },
    ]);
  });

  it("updates the branch of an outside change that is behind its base from the blocker", async () => {
    const behind = detail(
      task(1, {
        state: "blocked",
        stage: "merge",
        change_request: change,
        blocker: {
          ...BASE_BLOCKER,
          code: "change_behind_base",
          params: {
            base_branch: "main",
            base_sha: HEAD,
            head_sha: HEAD,
            updatable: true,
          },
          primary_action: { kind: "update_branch", params: {} },
        },
        available_actions: ["cancel"],
      }),
    );
    const { requests } = fakeApi(
      routes(() => behind, { [`${DETAIL}/actions/primary`]: ACCEPTED }),
    );
    renderWithProviders(<TaskPage workItemId={ID} />);
    const card = await screen.findByRole("region", { name: "Blocker" });
    expect(
      within(card).getByText(
        /The pull request is behind main; update its branch/,
      ),
    ).toBeInTheDocument();
    await userEvent.click(
      within(card).getByRole("button", { name: "Update the branch" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "catch up");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Update the branch" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(posts(requests, "primary").map((r) => r.body)).toEqual([
      { blocker_id: BLOCKER, reason: "catch up" },
    ]);
  });
});
