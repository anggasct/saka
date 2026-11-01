import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
const SPEC_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334466";
const LONG =
  "Add a done command to the CLI store so that tasks can be closed from the terminal without opening the web UI";

const project = {
  id: PROJECT_ID,
  name: "tasks-with-a-rather-long-project-name",
  workspace: {
    repo_id: "0193f0c4-1b2a-7c3d-9e8f-001122334400",
    full_name: "acme/tasks",
    project_path: "docs",
    base_branch: "main",
  },
  repos: [],
  autonomy: "standard",
  workflow_preset: "full_loop",
  config_commit: "9f1c".padEnd(40, "0"),
  created_at: "2026-10-31T10:00:00.000Z",
  updated_at: "2026-10-31T10:00:00.000Z",
};

const spec = {
  id: SPEC_ID,
  project_id: PROJECT_ID,
  path: "docs/specs/tasks-done.md",
  slug: "tasks-done",
  title: "Mark tasks as done from the command line and show them in the list",
  state: "approved",
  origin: "manual",
  priority: 0,
  revision: {
    id: "r",
    source_commit: "c",
    content_hash: "h",
    created_at: "2026-10-31T10:00:00.000Z",
  },
  approved_revision_id: "r",
  lint: { ok: true, issues: [] },
  steps: [],
  approved_at: null,
  shipped_at: null,
  created_at: "2026-10-31T10:00:00.000Z",
  updated_at: "2026-10-31T10:00:00.000Z",
};

function task(step: number, extra: Record<string, unknown>) {
  return {
    id: `0193f0c4-1b2a-7c3d-9e8f-00112233450${step}`,
    project_id: PROJECT_ID,
    origin: "spec_step",
    spec_id: SPEC_ID,
    spec_revision_id: "r",
    step_id: `S${step}`,
    step_index: step,
    task_key: `tasks-done/s${step}`,
    work: LONG,
    repo_id: "x",
    branch: `saka/tasks-done/s${step}`,
    state: "queued",
    stage: null,
    stage_phase: null,
    priority: 0,
    change_request: null,
    gate: null,
    blocker: null,
    session: null,
    pending_permission_count: 0,
    available_actions: [],
    version: 1,
    created_at: "2026-10-31T10:00:00.000Z",
    updated_at: "2026-10-31T10:00:00.000Z",
    admitted_at: null,
    done_at: null,
    ...extra,
  };
}

const tasks = [
  task(1, {
    state: "blocked",
    change_request: {
      id: "c",
      number: 12,
      url: "https://github.com/acme/tasks/pull/12",
      state: "open",
      head_sha: "a",
      base_branch: "main",
      merge_commit_sha: null,
    },
    blocker: {
      id: "b",
      code: "ci_red",
      params: {
        checks: ["test-with-a-long-name", "lint", "build"],
        head_sha: "a",
      },
      opened_at: "2026-10-31T10:05:00.000Z",
      stage: "merge",
    },
  }),
  task(2, { state: "in_stage", stage: "dev", stage_phase: "running_session" }),
];

const page_ = (items: unknown[]) => ({ items, next_cursor: null });

async function mockApi(page: Page, empty: boolean) {
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    const json = (body: unknown) => route.fulfill({ json: body });
    if (path === "/api/v1/events") {
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: ": keep-alive\n\n",
      });
    } else if (path === "/api/v1/projects") {
      await json(page_(empty ? [] : [project]));
    } else if (path.endsWith("/specs")) {
      await json(page_([spec]));
    } else if (path === "/api/v1/work-items") {
      await json(page_(tasks));
    } else if (path === "/api/v1/permission-requests") {
      await json(page_([{ id: "p" }]));
    } else {
      await route.fulfill({
        status: 404,
        json: { code: "not_found", params: {} },
      });
    }
  });
}

for (const empty of [false, true]) {
  test(`delivery ${empty ? "empty" : "ready"} state at 375 px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width: 375, height: 800 });
    await mockApi(page, empty);
    await page.goto("/");
    await expect(page).toHaveURL(/\/delivery$/);
    const heading = empty
      ? "Nothing to deliver yet"
      : "Mark tasks as done from the command line and show them in the list";
    await expect(page.getByRole("heading", { name: heading })).toBeVisible();

    const overflow = await page.evaluate(
      () =>
        document.documentElement.scrollWidth -
        document.documentElement.clientWidth,
    );
    expect(overflow).toBeLessThanOrEqual(0);

    const results = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
      .analyze();
    expect(results.violations).toEqual([]);
  });
}
