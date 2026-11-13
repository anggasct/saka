import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fakeApi, page } from "@/test/fake-api";
import type { Routes } from "@/test/fake-api";
import {
  detail,
  project,
  PROJECT_ID,
  SPEC_ID,
  specWithSteps,
  task,
} from "@/test/fixtures";
import { decision } from "@/test/decision-fixtures";
import { renderWithProviders } from "@/test/render";

import type { WorkItem, WorkItemDetail } from "./queries";
import { TaskPage } from "./task-page";

const HEAD = "a1b2c3d4e5f6".padEnd(40, "0");
const MERGE = "9e8f7a6b5c4d".padEnd(40, "0");

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

const blocked = detail(
  task(1, {
    state: "blocked",
    change_request: change,
    blocker: {
      id: "0193f0c4-1b2a-7c3d-9e8f-001122334488",
      code: "ci_red",
      params: {
        source: "saka",
        checks: ["make test"],
        head_sha: HEAD,
        command: "make test",
        exit_code: 2,
        log: "/var/lib/saka/logs/gate-a1b2c3d4e5f6-0.log",
        log_tail: "FAIL store_test.go:12",
      },
      opened_at: "2026-10-31T10:05:00.000Z",
      stage: "merge",
      primary_action: { kind: "retry", params: {} },
    },
    gates: [
      {
        source: "scm",
        required: true,
        head_sha: HEAD,
        outcome: "passed",
        checks: [
          {
            name: "test",
            kind: "check_run",
            status: "completed",
            conclusion: "success",
            url: "https://github.com/acme/tasks/runs/1",
          },
        ],
        observed_at: "2026-10-31T10:04:00.000Z",
      },
      {
        source: "saka",
        required: true,
        head_sha: HEAD,
        outcome: "failed",
        checks: [
          {
            name: "make test",
            kind: "command",
            status: "completed",
            conclusion: "failure",
            url: null,
          },
        ],
        observed_at: "2026-10-31T10:05:00.000Z",
      },
    ],
    environment: {
      id: "0193f0c4-1b2a-7c3d-9e8f-0011223344aa",
      state: "ready",
      ports: { start: 41000, count: 3 },
      recipe_sha256: "9f2c1d0e8b7a".padEnd(64, "0"),
      toolchains: [{ tool: "node", version: "26.13.0" }],
      docs: {
        repo_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344dd",
        full_name: "acme/docs",
        commit: "5e6f7a8b9c0d".padEnd(40, "1"),
        project_path: "docs",
        path: "/var/lib/saka/envs/e-1/docs/docs",
        mode: "read_only",
        branch: null,
        base_sha: "5e6f7a8b9c0d".padEnd(40, "1"),
      },
    },
    budget: {
      stage_run_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344bb",
      warn_percent: 80,
      running_minutes: { used: 12.5, limit: 60 },
      turns: { used: 9, limit: 10 },
      tokens: { used: 41000, limit: 0 },
      cost_usd: { used: "0.4100", limit: "0" },
    },
    available_actions: ["retry", "cancel"],
  }),
  {
    stage_runs: [
      {
        id: "0193f0c4-1b2a-7c3d-9e8f-0011223344c1",
        stage: "dev",
        attempt: 1,
        state: "succeeded",
        trigger: "auto",
        reason: null,
        head_sha: HEAD,
        failure: null,
        started_at: "2026-10-31T10:00:00.000Z",
        ended_at: "2026-10-31T10:02:00.000Z",
      },
      {
        id: "0193f0c4-1b2a-7c3d-9e8f-0011223344c2",
        stage: "merge",
        attempt: 1,
        state: "blocked",
        trigger: "auto",
        reason: null,
        head_sha: HEAD,
        failure: {
          code: "ci_red",
          params: { source: "scm", checks: ["lint"], head_sha: HEAD },
        },
        started_at: "2026-10-31T10:02:00.000Z",
        ended_at: "2026-10-31T10:05:00.000Z",
      },
    ],
  },
);

function routes(
  item: WorkItemDetail,
  siblings: WorkItem[] = [item],
  extra: Routes = {},
): Routes {
  return {
    [`/api/v1/work-items/${item.id}`]: { body: item },
    [`/api/v1/specs/${SPEC_ID}`]: {
      body: specWithSteps([
        ["S1", []],
        ["S2", ["S1"]],
      ]),
    },
    [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
    "/api/v1/work-items": { body: page(siblings) },
    ...extra,
  };
}

describe("TaskPage", () => {
  it("explains a failed gate hook and links to the project's hook runs", async () => {
    const hookBlocked = detail(
      task(1, {
        state: "blocked",
        change_request: change,
        blocker: {
          id: "0193f0c4-1b2a-7c3d-9e8f-001122334489",
          code: "hook_failed",
          params: {
            hook: "smoke",
            hook_run_id: "0193f0c4-1b2a-7c3d-9e8f-0011223355c1",
            action: "run",
            error: { code: "hook_command_failed", params: { exit_code: 2 } },
          },
          opened_at: "2026-10-31T10:05:00.000Z",
          stage: "sync",
          primary_action: { kind: "retry", params: {} },
        },
      }),
    );
    fakeApi(routes(hookBlocked));
    renderWithProviders(<TaskPage workItemId={hookBlocked.id} />);
    const blocker = await screen.findByRole("region", { name: "Blocker" });
    expect(
      within(blocker).getByText(
        "The gate hook smoke (run) failed: the command exited with code 2.",
      ),
    ).toBeInTheDocument();
    expect(
      within(blocker).getByRole("link", {
        name: "See the hook runs in project settings",
      }),
    ).toHaveAttribute("href", `/settings/${PROJECT_ID}`);
  });

  it("shows the questions the agent asked, with a form for the open one", async () => {
    const asking = detail(task(1, { state: "in_stage", stage: "dev" }));
    const { requests } = fakeApi(
      routes(asking, [asking], {
        "/api/v1/decision-requests": {
          body: page([
            decision(1),
            decision(2, {
              state: "delivered",
              question: "Which format?",
              answer: {
                option: 0,
                note: null,
                answered_by: "operator:local",
                answered_at: "2026-11-06T11:00:00.000Z",
              },
            }),
          ]),
        },
      }),
    );
    renderWithProviders(<TaskPage workItemId={asking.id} />);
    const section = await screen.findByRole("region", {
      name: "Questions from the agent",
    });
    expect(
      within(section).getByRole("button", { name: "Send answer" }),
    ).toBeInTheDocument();
    expect(within(section).getAllByRole("article")).toHaveLength(2);
    expect(
      requests
        .find((r) => r.path === "/api/v1/decision-requests")
        ?.search.get("work_item_id"),
    ).toBe(asking.id);
  });

  it("names the autonomy level the task runs at", async () => {
    const strict = detail(task(1, { autonomy: "strict" }));
    fakeApi(routes(strict));
    renderWithProviders(<TaskPage workItemId={strict.id} />);
    expect(await screen.findByText("Autonomy")).toBeInTheDocument();
    expect(screen.getByText("Strict")).toBeInTheDocument();
  });

  it("says when the loop will merge by itself", async () => {
    const auto = detail(
      task(1, {
        state: "in_stage",
        stage: "merge",
        stage_phase: "awaiting_ci",
        merge_auto: true,
      }),
    );
    fakeApi(routes(auto));
    renderWithProviders(<TaskPage workItemId={auto.id} />);
    expect(
      await screen.findByText(
        "Saka merges this by itself once the review approves and the checks pass.",
      ),
    ).toBeInTheDocument();
  });

  it("shows the evidence of a blocked task", async () => {
    fakeApi(routes(blocked));
    renderWithProviders(<TaskPage workItemId={blocked.id} />);
    expect(
      await screen.findByRole("heading", { level: 1, name: "Step S1" }),
    ).toBeInTheDocument();
    expect(await screen.findByText("Mark tasks as done")).toBeInTheDocument();
    expect(await screen.findByText("tasks")).toBeInTheDocument();

    const blocker = screen.getByRole("region", { name: "Blocker" });
    expect(
      within(blocker).getByText(
        "The Done when command make test failed with exit code 2.",
      ),
    ).toBeInTheDocument();
    expect(within(blocker).getByText("FAIL store_test.go:12")).toBeVisible();
    expect(
      within(blocker).getByText(
        "Full log on the server: /var/lib/saka/logs/gate-a1b2c3d4e5f6-0.log",
      ),
    ).toBeInTheDocument();

    const pr = screen.getByRole("region", { name: "Pull request" });
    expect(within(pr).getByRole("link", { name: "PR #12" })).toHaveAttribute(
      "href",
      change.url,
    );

    const gates = screen.getByRole("region", { name: "Gates" });
    expect(within(gates).getByText("CI on the code host")).toBeInTheDocument();
    expect(within(gates).getByText("Passed")).toBeInTheDocument();
    expect(within(gates).getByRole("link", { name: "test" })).toHaveAttribute(
      "href",
      "https://github.com/acme/tasks/runs/1",
    );
    expect(
      within(gates).getByText("Done when commands run by Saka"),
    ).toBeInTheDocument();
    expect(within(gates).getByText("Failed")).toBeInTheDocument();

    const runs = screen.getByRole("region", { name: "Stage runs" });
    expect(
      within(runs).getByText("Development · attempt 1"),
    ).toBeInTheDocument();
    expect(within(runs).getByText("Merge · attempt 1")).toBeInTheDocument();
    expect(within(runs).getByText("CI failed: lint.")).toBeInTheDocument();
    expect(within(runs).getAllByText("2 minutes")).toHaveLength(1);

    const env = screen.getByRole("region", { name: "Environment" });
    expect(within(env).getByText("41000–41002")).toBeInTheDocument();
    expect(within(env).getByText("node 26.13.0")).toBeInTheDocument();
    expect(within(env).getByText("Project docs")).toBeInTheDocument();
    expect(
      within(env).getByText("/var/lib/saka/envs/e-1/docs/docs"),
    ).toBeInTheDocument();
    expect(
      within(env).getByText("acme/docs/docs at 5e6f7a8b9c0d, read-only"),
    ).toBeInTheDocument();
    expect(within(env).getByText("12.5 of 60")).toBeInTheDocument();
    expect(within(env).getByText("9 of 10")).toBeInTheDocument();
    expect(within(env).getByText("near the limit")).toBeInTheDocument();
    expect(within(env).getByText("41000 (no limit)")).toBeInTheDocument();
  });

  it("names the flake a gate passed after, and says while the failed checks are re-run", async () => {
    const flaky = detail(
      task(1, {
        state: "in_stage",
        stage: "merge",
        stage_phase: "awaiting_merge",
        change_request: change,
        gates: [
          {
            source: "scm",
            required: true,
            head_sha: HEAD,
            outcome: "passed",
            checks: [],
            passed_after_rerun: { reruns: 1, checks: ["test", "lint"] },
            observed_at: "2026-10-31T10:04:00.000Z",
          },
        ],
      }),
    );
    fakeApi(routes(flaky));
    renderWithProviders(<TaskPage workItemId={flaky.id} />);
    const gates = await screen.findByRole("region", { name: "Gates" });
    expect(
      within(gates).getByText(
        "Passed after 1 re-run of the failed checks: test, lint. The earlier failure was a flake.",
      ),
    ).toBeInTheDocument();
  });

  it("shows that a re-run of the failed checks is under way", async () => {
    const waiting = detail(
      task(1, {
        state: "in_stage",
        stage: "merge",
        stage_phase: "awaiting_ci",
        change_request: {
          ...change,
          ci_rerun: {
            count: 1,
            checks: ["test"],
            pending: true,
            rerunnable: true,
          },
        },
      }),
    );
    fakeApi(routes(waiting));
    renderWithProviders(<TaskPage workItemId={waiting.id} />);
    const gates = await screen.findByRole("region", { name: "Gates" });
    expect(
      within(gates).getByText(
        "Re-running the failed check (test); the task waits at this head.",
      ),
    ).toBeInTheDocument();
  });

  it("titles the gates with the merge commit while it is verified", async () => {
    const verifying = detail(
      task(1, {
        state: "in_stage",
        stage: "sync",
        stage_phase: "verifying",
        change_request: {
          ...change,
          state: "merged",
          merge_commit_sha: MERGE,
          merged_at: "2026-10-31T10:10:00.000Z",
        },
        gates: [
          {
            source: "saka",
            required: true,
            head_sha: MERGE,
            outcome: "pending",
            checks: [],
            observed_at: "2026-10-31T10:11:00.000Z",
          },
        ],
      }),
    );
    fakeApi(routes(verifying));
    renderWithProviders(<TaskPage workItemId={verifying.id} />);
    expect(
      await screen.findByRole("region", {
        name: "Verifying merge commit 9e8f7a6b5c4d",
      }),
    ).toBeInTheDocument();
    expect(screen.getByText("Sync · checking the base branch")).toBeVisible();
  });

  it("shows the docs pull request next to the code pull request", async () => {
    const docsChange = {
      id: "0193f0c4-1b2a-7c3d-9e8f-0011223344ee",
      number: 13,
      url: "https://github.com/acme/docs/pull/13",
      state: "merged",
      head_sha: HEAD,
      base_branch: "main",
      merge_commit_sha: MERGE,
      merged_at: "2026-10-31T10:12:00.000Z",
      skipped: false,
    };
    const merging = detail(
      task(1, {
        state: "in_stage",
        stage: "sync",
        stage_phase: "merging_docs",
        change_request: { ...change, state: "merged", merge_commit_sha: HEAD },
        docs_change_request: docsChange,
      }),
    );
    fakeApi(routes(merging));
    renderWithProviders(<TaskPage workItemId={merging.id} />);
    const pr = await screen.findByRole("region", { name: "Pull request" });
    expect(within(pr).getByRole("link", { name: "PR #13" })).toHaveAttribute(
      "href",
      docsChange.url,
    );
    expect(within(pr).getByText("Docs merge commit")).toBeInTheDocument();
    expect(within(pr).getByText("9e8f7a6b5c4d")).toBeInTheDocument();
    expect(
      screen.getByText("Sync · merging the docs pull request"),
    ).toBeVisible();
  });

  it("says a docs pull request closed without merging was skipped", async () => {
    const skipped = detail(
      task(1, {
        state: "done",
        change_request: { ...change, state: "merged", merge_commit_sha: MERGE },
        docs_change_request: {
          id: "0193f0c4-1b2a-7c3d-9e8f-0011223344ee",
          number: 13,
          url: "https://github.com/acme/docs/pull/13",
          state: "closed",
          head_sha: HEAD,
          base_branch: "main",
          merge_commit_sha: null,
          merged_at: null,
          skipped: true,
        },
      }),
    );
    fakeApi(routes(skipped));
    renderWithProviders(<TaskPage workItemId={skipped.id} />);
    const pr = await screen.findByRole("region", { name: "Pull request" });
    expect(
      within(pr).getByText("Skipped: closed without merging"),
    ).toBeInTheDocument();
    expect(within(pr).queryByText("Docs merge commit")).toBeNull();
  });

  it("says how long after the merge of its dependency the step started", async () => {
    const first = task(1, {
      state: "done",
      change_request: {
        ...change,
        state: "merged",
        merged_at: "2026-10-31T10:29:00.000Z",
      },
    });
    const second = detail(
      task(2, {
        state: "admitted",
        admitted_at: "2026-10-31T10:30:30.000Z",
      }),
    );
    fakeApi(routes(second, [first, second]));
    renderWithProviders(<TaskPage workItemId={second.id} />);
    expect(
      await screen.findByText("Started 1 minute after S1 was merged."),
    ).toBeInTheDocument();
  });

  it("counts the steps a blocked task holds", async () => {
    const waitingStep = task(2, {
      waiting_on: [{ kind: "step", step_id: "S1", work_item_id: blocked.id }],
    });
    const waitingSpec = {
      ...task(5, {
        spec_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344dd",
      }),
      waiting_on: [
        { kind: "spec" as const, slug: "tasks-done", spec_id: SPEC_ID },
      ],
    };
    fakeApi(routes(blocked, [blocked, waitingStep, waitingSpec]));
    renderWithProviders(<TaskPage workItemId={blocked.id} />);
    expect(
      await screen.findByText("Holds 2 waiting steps"),
    ).toBeInTheDocument();
  });

  it("lists every reason a queued task waits for", async () => {
    const queued = detail(
      task(2, {
        waiting_on: [
          { kind: "step", step_id: "S1", work_item_id: blocked.id },
          { kind: "spec", slug: "tasks-store", spec_id: SPEC_ID },
          {
            kind: "quota",
            account: "claude:default",
            window: "seven_day",
            utilization: 0.93,
            resets_at: null,
          },
          { kind: "port_block" },
        ],
      }),
    );
    fakeApi(routes(queued));
    renderWithProviders(<TaskPage workItemId={queued.id} />);
    const waiting = await screen.findByRole("region", { name: "Waiting" });
    expect(
      within(waiting).getByRole("link", {
        name: "Waiting for step S1 to be done.",
      }),
    ).toHaveAttribute("href", `/delivery/tasks/${blocked.id}`);
    expect(
      within(waiting).getByText("Waiting for spec tasks-store to ship."),
    ).toBeInTheDocument();
    expect(
      within(waiting).getByText(
        "Waiting for the agent account claude:default: its seven_day window is at 93%.",
      ),
    ).toBeInTheDocument();
    expect(
      within(waiting).getByText("Waiting for a free port block."),
    ).toBeInTheDocument();
  });

  it("shows a task that does not exist as not found", async () => {
    fakeApi({});
    renderWithProviders(<TaskPage workItemId="missing" />);
    expect(
      await screen.findByRole("heading", { name: "Task not found" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Back to Delivery" }),
    ).toHaveAttribute("href", "/delivery");
  });

  it("shows a server error and loads again on retry", async () => {
    let fail = true;
    fakeApi({
      ...routes(blocked),
      [`/api/v1/work-items/${blocked.id}`]: () =>
        fail
          ? { status: 500, body: { code: "internal", params: {} } }
          : { body: blocked },
    });
    renderWithProviders(<TaskPage workItemId={blocked.id} />);
    const alert = await screen.findByRole("alert");
    expect(
      within(alert).getByText("Something went wrong on the server."),
    ).toBeInTheDocument();
    fail = false;
    await userEvent.click(
      within(alert).getByRole("button", { name: "Try again" }),
    );
    expect(
      await screen.findByRole("heading", { level: 1, name: "Step S1" }),
    ).toBeInTheDocument();
  });

  it("loads the work packet once, only when it is opened", async () => {
    const packetPath = `/api/v1/work-items/${blocked.id}/work-packet`;
    const { calls } = fakeApi(
      routes(blocked, [blocked], {
        [packetPath]: {
          body: {
            work_item_id: blocked.id,
            revision_id: "r",
            source_commit: "9f1c".padEnd(40, "0"),
            environment_id: "e",
            bytes: 18234,
            sha256: "sha256:" + "4b8e00112233".padEnd(64, "0"),
            documents: [
              {
                index: 1,
                kind: "spec",
                path: "docs/specs/tasks-done.md",
                origin: null,
                bytes: 6120,
                content_hash: "sha256:x",
              },
              {
                index: 2,
                kind: "context",
                path: "docs/architecture.md",
                origin: "link",
                bytes: 9800,
                content_hash: "sha256:y",
              },
            ],
            text: "<documents>packet body</documents>",
          },
        },
      }),
    );
    renderWithProviders(<TaskPage workItemId={blocked.id} />);
    const show = await screen.findByRole("button", {
      name: "Show the work packet",
    });
    expect(calls.filter((c) => c.startsWith(packetPath))).toHaveLength(0);
    await userEvent.click(show);
    expect(
      await screen.findByText("<documents>packet body</documents>"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("1. spec: docs/specs/tasks-done.md · 6,120 bytes"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "2. context (linked): docs/architecture.md · 9,800 bytes",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText("18,234 bytes · 4b8e00112233")).toBeInTheDocument();
    await userEvent.click(
      screen.getByRole("button", { name: "Hide the work packet" }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Show the work packet" }),
    );
    expect(calls.filter((c) => c.startsWith(packetPath))).toHaveLength(1);
  });

  for (const [reason, sentence] of [
    [
      "not_a_spec_task",
      "This task does not come from a spec, so it has no work packet.",
    ],
    [
      "environment_not_ready",
      "The work packet appears once the task environment is ready.",
    ],
  ] as const) {
    it(`explains a work packet that is unavailable: ${reason}`, async () => {
      fakeApi(
        routes(blocked, [blocked], {
          [`/api/v1/work-items/${blocked.id}/work-packet`]: {
            status: 409,
            body: { code: "work_packet_unavailable", params: { reason } },
          },
        }),
      );
      renderWithProviders(<TaskPage workItemId={blocked.id} />);
      await userEvent.click(
        await screen.findByRole("button", { name: "Show the work packet" }),
      );
      expect(await screen.findByText(sentence)).toBeInTheDocument();
    });
  }
});
