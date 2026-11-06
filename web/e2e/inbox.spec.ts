import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
const SPEC_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334466";
const TASK_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a1";
const BLOCKER_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344b1";
const REQUEST_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344c1";
const REVISION_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334477";
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const AT = "2026-11-06T10:00:00.000Z";

const title = {
  task_key: "tasks-done/s1",
  stage: "dev",
  spec_slug: "tasks-done",
  spec_title: "Mark tasks as done",
  spec_path: "docs/specs/tasks-done.md",
  draft_path: null,
};

function item(id: string, over: Record<string, unknown>) {
  return {
    id,
    project_id: PROJECT_ID,
    project_name: "tasks-with-a-rather-long-project-name",
    state: "open",
    work_item_id: TASK_ID,
    spec_id: SPEC_ID,
    title,
    dismissible: false,
    opened_at: AT,
    closed_at: null,
    ...over,
  };
}

const ID = (n: number) => `0193f0c4-1b2a-7c3d-9e8f-00112233990${n}`;

const items = [
  item(ID(1), {
    kind: "permission",
    subject: "permission",
    params: { tool: "Bash", summary: "npm install left-pad --save-exact" },
    action: {
      kind: "answer_permission",
      params: { permission_request_id: REQUEST_ID },
    },
  }),
  item(ID(2), {
    kind: "blocker",
    subject: "blocker",
    params: { code: "ci_red", head_sha: "a9e3d95".padEnd(40, "0") },
    action: { kind: "retry", params: { blocker_id: BLOCKER_ID } },
  }),
  item(ID(3), {
    kind: "blocker",
    subject: "blocker",
    params: { code: "secret_missing", names: ["NPM_TOKEN"] },
    action: { kind: "set_secret", params: { names: ["NPM_TOKEN"] } },
  }),
  item(ID(4), {
    kind: "approval",
    subject: "spec",
    work_item_id: null,
    title: { ...title, task_key: null, stage: null },
    params: { revision: REVISION_ID },
    action: {
      kind: "approve_spec",
      params: { spec_id: SPEC_ID, revision: REVISION_ID },
    },
    dismissible: true,
  }),
  item(ID(5), {
    kind: "approval",
    subject: "merge",
    params: { head_sha: null },
    action: { kind: "merge", params: { work_item_id: TASK_ID, version: 9 } },
  }),
];

const summary = (open: number) => ({
  open,
  by_kind: {
    decision: 0,
    approval: 2,
    permission: 1,
    blocker: 2,
    spec_unclear: 0,
  },
  running: 3,
  merged_today: 2,
});

async function mockApi(page: Page, waiting: unknown[]) {
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    const json = (body: unknown, status = 200) =>
      route.fulfill({ status, json: body });
    if (path === "/api/v1/events") {
      await route.fulfill({ status: 204 });
    } else if (path === "/api/v1/inbox") {
      await json({ items: waiting, next_cursor: null });
    } else if (path === "/api/v1/inbox/summary") {
      await json(summary(waiting.length));
    } else {
      await json({ code: "not_found", params: {} }, 404);
    }
  });
}

async function accessible(page: Page) {
  const results = await new AxeBuilder({ page }).withTags(AXE_TAGS).analyze();
  expect(results.violations).toEqual([]);
}

async function noOverflow(page: Page) {
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
}

async function touchable(targets: Locator, wide = true) {
  for (const target of await targets.all()) {
    if (!(await target.isVisible())) {
      continue;
    }
    const box = await target.boundingBox();
    expect(box?.height ?? 0).toBeGreaterThanOrEqual(44);
    if (wide) {
      expect(box?.width ?? 0).toBeGreaterThanOrEqual(44);
    }
  }
}

for (const width of [375, 1280]) {
  test(`the inbox lists what waits and stays accessible at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await mockApi(page, items);
    await page.goto("/");

    await expect(
      page.getByRole("heading", { level: 1, name: "Inbox" }),
    ).toBeVisible();
    await expect(page.getByText("5 items need you")).toBeVisible();
    const nav = page.getByRole("navigation");
    await expect(nav.getByRole("link", { name: /^Inbox/ })).toContainText("5");
    const cards = page.getByRole("article");
    await expect(cards).toHaveCount(5);
    await noOverflow(page);
    await touchable(page.getByRole("main").getByRole("button"));
    await touchable(page.getByRole("main").getByRole("link"));
    await touchable(nav.getByRole("link"), false);
    await accessible(page);

    const approval = page.getByRole("article", { name: "Mark tasks as done" });
    await approval.getByRole("button", { name: "Dismiss" }).click();
    const dialog = page.getByRole("dialog", { name: "Dismiss this approval" });
    await expect(dialog).toBeVisible();
    await touchable(dialog.getByRole("button"));
    await accessible(page);
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
  });

  test(`an empty inbox says what is running at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await mockApi(page, []);
    await page.goto("/");
    await expect(page.getByText("Nothing is waiting for you")).toBeVisible();
    await expect(page.getByText("Running now")).toBeVisible();
    await noOverflow(page);
    await accessible(page);
  });
}

test("the inbox follows the chosen language", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 900 });
  await mockApi(page, items);
  await page.goto("/");
  await expect(page.getByRole("article")).toHaveCount(5);
  await page
    .getByRole("combobox", { name: "Language" })
    .selectOption("Bahasa Indonesia");
  await expect(page.locator("html")).toHaveAttribute("lang", "id");
  await expect(
    page.getByRole("heading", { level: 1, name: "Kotak masuk" }),
  ).toBeVisible();
  await expect(page.getByText("5 item butuh kamu")).toBeVisible();
  await accessible(page);
});
