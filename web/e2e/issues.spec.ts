import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { FULL_LOOP } from "./workflow";

const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
const SPEC_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334466";
const REVISION_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334477";
const INTAKE_ID = "0193f0c4-1b2a-7c3d-9e8f-001122338801";
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const AT = "2026-11-13T10:00:00.000Z";
const TITLE =
  "The service crashes on start when the configuration file is empty and the data directory is missing";

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
  hooks_skipped: [],
  authorship_mode: "bot",
  workflow: {
    ...FULL_LOOP,
    issue: {
      trigger: "label",
      trigger_label: "saka:intake",
      labels: [],
      exclude_labels: ["wontfix"],
      kind_labels: { bug: "bugfix" },
      default_kind: "feature",
      include_confidential: false,
      max_open: 20,
      bugfix: { done_when: ["cargo test"], regression: ["cargo test crash"] },
    },
  },
  allow_unreviewed_chore: false,
  config_commit: "9f1c".padEnd(40, "0"),
  created_at: AT,
  updated_at: AT,
};

function intake(id: string, over: Record<string, unknown>) {
  return {
    id,
    project_id: PROJECT_ID,
    repo: { id: "r", name: "app", full_name: "acme/tasks" },
    issue: {
      number: 41,
      url: "https://github.com/acme/tasks/issues/41",
      title: TITLE,
      author: "alice",
    },
    state: "awaiting_approval",
    lane: "bugfix",
    kind: "bugfix",
    cause: "label",
    actor: "maintainer",
    spec: {
      id: SPEC_ID,
      path: "docs/specs/bugfix-the-service-crashes-on-start-41.md",
      state: "ready",
    },
    draft: null,
    failure: null,
    retries: 0,
    created_at: AT,
    updated_at: AT,
    ...over,
  };
}

const intakes = [
  intake(INTAKE_ID, {}),
  intake("0193f0c4-1b2a-7c3d-9e8f-001122338802", {
    state: "failed",
    lane: "draft",
    kind: "chore",
    spec: null,
    failure: { code: "draft_failed", params: {} },
    retries: 1,
    issue: {
      number: 42,
      url: "https://github.com/acme/tasks/issues/42",
      title: "Bump the linter",
      author: "bob",
    },
  }),
];

const issue = {
  url: "https://github.com/acme/tasks/issues/41",
  number: 41,
  title: TITLE,
  repo: "acme/tasks",
};

const inboxItems = [
  {
    id: "0193f0c4-1b2a-7c3d-9e8f-001122339901",
    project_id: PROJECT_ID,
    project_name: project.name,
    kind: "approval",
    subject: "spec",
    state: "open",
    work_item_id: null,
    spec_id: SPEC_ID,
    title: {
      task_key: null,
      stage: null,
      spec_slug: "bugfix-crash-41",
      spec_title: TITLE,
      spec_path: "docs/specs/bugfix-crash-41.md",
      draft_path: null,
    },
    params: { revision: REVISION_ID, issue },
    action: {
      kind: "approve_spec",
      params: { spec_id: SPEC_ID, revision: REVISION_ID },
    },
    dismissible: true,
    opened_at: AT,
    closed_at: null,
  },
  {
    id: "0193f0c4-1b2a-7c3d-9e8f-001122339902",
    project_id: PROJECT_ID,
    project_name: project.name,
    kind: "intake",
    subject: "intake",
    state: "open",
    work_item_id: null,
    spec_id: null,
    title: {
      task_key: null,
      stage: null,
      spec_slug: null,
      spec_title: null,
      spec_path: null,
      draft_path: null,
    },
    params: {
      issue: { ...issue, number: 42, title: "Bump the linter" },
      code: "draft_failed",
      failure: {},
      lane: "draft",
      kind: "chore",
      retries: 1,
      can_retry: true,
    },
    action: { kind: "retry_intake", params: { intake_id: INTAKE_ID } },
    dismissible: false,
    opened_at: AT,
    closed_at: null,
  },
];

const list = (items: unknown[]) => ({ items, next_cursor: null });

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
      await json(list([project]));
    } else if (path === `/api/v1/projects/${PROJECT_ID}/issue-intakes`) {
      await json(list(empty ? [] : intakes));
    } else if (path.endsWith("/specs")) {
      await json(list([]));
    } else if (path === "/api/v1/work-items") {
      await json(list([]));
    } else if (path === "/api/v1/permission-requests") {
      await json(list([]));
    } else if (path === "/api/v1/inbox") {
      await json(list(inboxItems));
    } else if (path === "/api/v1/inbox/summary") {
      await json({
        open: inboxItems.length,
        by_kind: { approval: 1, intake: 1 },
        running: 0,
        merged_today: 0,
      });
    } else if (path === `/api/v1/projects/${PROJECT_ID}`) {
      await json(project);
    } else {
      await route.fulfill({
        status: 404,
        json: { code: "not_found", params: {} },
      });
    }
  });
}

async function noOverflow(page: Page) {
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
}

async function accessible(page: Page) {
  const results = await new AxeBuilder({ page }).withTags(AXE_TAGS).analyze();
  expect(results.violations).toEqual([]);
}

for (const width of [375, 1280]) {
  test(`delivery shows the issues of a project at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await mockApi(page, false);
    await page.goto("/delivery");
    const group = page.getByRole("region", { name: "From issues" });
    await expect(group).toBeVisible();
    await expect(
      group.getByRole("link", { name: "Issue #41" }),
    ).toHaveAttribute("href", "https://github.com/acme/tasks/issues/41");
    await expect(group.getByText("Waiting for approval")).toBeVisible();
    await expect(group.getByText("Failed: draft_failed")).toBeVisible();
    await expect(group.getByRole("button", { name: "Retry" })).toBeVisible();
    await expect(
      group.getByRole("button", { name: "Add issue" }),
    ).toBeVisible();
    await noOverflow(page);
    await accessible(page);
  });

  test(`the add-issue dialog and the empty list are accessible at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await mockApi(page, true);
    await page.goto("/delivery");
    await expect(page.getByText(/adds the label saka:intake/)).toBeVisible();
    await noOverflow(page);
    await accessible(page);
    await page.getByRole("button", { name: "Add issue" }).click();
    await expect(page.getByRole("dialog")).toBeVisible();
    await accessible(page);
  });

  test(`the inbox links the issue and offers retry on a failed intake at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await mockApi(page, false);
    await page.goto("/");
    await expect(
      page.getByRole("link", { name: "From issue #41" }),
    ).toHaveAttribute("href", "https://github.com/acme/tasks/issues/41");
    const failed = page.getByRole("article", {
      name: "Issue #42: Bump the linter",
    });
    await expect(failed.getByRole("button", { name: "Retry" })).toBeVisible();
    await expect(failed.getByRole("button", { name: "Dismiss" })).toBeVisible();
    await noOverflow(page);
    await accessible(page);
  });
}
