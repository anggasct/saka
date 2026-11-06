import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
const TASK_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a1";
const RUN_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344c1";
const DECISION_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344e1";
const REQUEST_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344d1";
const FIRST = "0193f0c4-1b2a-7c3d-9e8f-001122335001";
const SECOND = "0193f0c4-1b2a-7c3d-9e8f-001122335002";
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const AT = "2026-11-06T10:00:00.000Z";
const TITLE = "tasks-done/s1-with-a-rather-long-task-key";

const owner = {
  kind: "stage_run",
  work_item_id: TASK_ID,
  stage_run_id: RUN_ID,
};

function active(id: string, over: Record<string, unknown>) {
  return {
    id,
    state: "running",
    role: "dev",
    project_id: PROJECT_ID,
    project_name: "tasks-with-a-rather-long-project-name",
    owner,
    title: TITLE,
    model: "opus",
    started_at: AT,
    last_activity_at: "2026-11-06T10:05:00.000Z",
    pending: { permissions: 0, decisions: 0 },
    ...over,
  };
}

const sessions = [
  active(FIRST, {
    state: "waiting",
    pending: { permissions: 1, decisions: 1 },
  }),
  active(SECOND, {
    role: "architect",
    title: "export",
    owner: { kind: "draft_attempt", spec_draft_id: "d", attempt_id: "a" },
  }),
];

const session = {
  id: FIRST,
  project_id: PROJECT_ID,
  project_name: "tasks-with-a-rather-long-project-name",
  role: "dev",
  title: TITLE,
  owner,
  environment: {
    branch: "saka/tasks-done/s1",
    toolchains: [{ tool: "node", version: "26.13.0" }],
    ports: { start: 41000, count: 4 },
    setup_state: "ready",
  },
  cli: "claude",
  cli_version: "2.1.0",
  model: "opus",
  state: "waiting",
  turns: 2,
  running_ms: 125000,
  cost_usd: "0.4100",
  input_tokens: 1200,
  output_tokens: 300,
  resumed_from_session_id: null,
  resume_cause: null,
  end_reason: null,
  available_actions: ["stop"],
  started_at: AT,
  ended_at: null,
};

const permission = {
  id: REQUEST_ID,
  project_id: PROJECT_ID,
  owner,
  session_id: FIRST,
  tool: "Bash",
  summary: "npm install left-pad --save-exact",
  input: { command: "npm install left-pad --save-exact" },
  state: "pending",
  requested_at: AT,
  answer_source: null,
  answered_at: null,
  message: null,
};

const decision = {
  id: DECISION_ID,
  project_id: PROJECT_ID,
  owner,
  state: "asked",
  question: "Where do we store the recorded audio files for every user?",
  context: "The recorder needs a place to keep files between sessions.",
  options: [
    { label: "Local disk under the data folder", detail: "Simple and free." },
    { label: "Cloudflare R2", detail: null },
  ],
  answer: null,
  asked_at: AT,
  expires_at: "2026-11-09T10:00:00.000Z",
};

const timeline = Array.from({ length: 12 }, (_, offset) => ({
  offset,
  at: AT,
  kind: offset % 2 === 0 ? "message" : "action",
  summary:
    offset % 2 === 0
      ? `Reading the spec and thinking about step ${offset} for a long while`
      : `Bash: make test --filter very-long-filter-name-${offset}`,
  data: offset % 2 === 0 ? {} : { tool: "Bash" },
}));

async function mockApi(page: Page, listed: unknown[]) {
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    const json = (body: unknown, status = 200) =>
      route.fulfill({ status, json: body });
    if (path === "/api/v1/events") {
      await route.fulfill({ status: 204 });
    } else if (path === "/api/v1/inbox") {
      await json({ items: [], next_cursor: null });
    } else if (path === "/api/v1/inbox/summary") {
      await json({
        open: 3,
        by_kind: {},
        running: 2,
        merged_today: 0,
      });
    } else if (path === "/api/v1/sessions") {
      await json({ items: listed, next_cursor: null });
    } else if (path === `/api/v1/sessions/${FIRST}`) {
      await json(session);
    } else if (path === `/api/v1/sessions/${FIRST}/timeline`) {
      await json({ items: timeline, next_after: null, prev_before: null });
    } else if (path === "/api/v1/permission-requests") {
      await json({ items: [permission], next_cursor: null });
    } else if (path === "/api/v1/decision-requests") {
      await json({ items: [decision], next_cursor: null });
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
  const all = await targets.all();
  expect(all.length).toBeGreaterThan(0);
  for (const target of all) {
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
  const narrow = width < 640;

  test(`the sessions list and the navigation are accessible at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 800 });
    await mockApi(page, sessions);
    await page.goto("/sessions");

    await expect(
      page.getByRole("heading", { level: 1, name: "Sessions" }),
    ).toBeVisible();
    await expect(page.getByText("2 sessions are active")).toBeVisible();
    const rows = page.getByRole("main").getByRole("link");
    await expect(rows).toHaveCount(2);
    await expect(rows.first()).toContainText("1 permission request waiting");
    await expect(rows.first()).toContainText("1 question waiting");

    const bottom = page.getByRole("navigation", { name: "Main" });
    if (narrow) {
      await expect(bottom).toBeVisible();
      const links = bottom.getByRole("link");
      await expect(links).toHaveCount(4);
      await expect(links.nth(0)).toContainText("Inbox");
      await expect(links.nth(0)).toContainText("3");
      await expect(links.nth(3)).toContainText("Sessions");
      await touchable(links);
      const box = await bottom.boundingBox();
      expect((box?.y ?? 0) + (box?.height ?? 0)).toBeCloseTo(800, 0);
      await expect(
        page.getByRole("banner").getByRole("link", { name: "Settings" }),
      ).toBeVisible();
      await touchable(page.getByRole("banner").getByRole("link"));
      await touchable(page.getByRole("banner").getByRole("combobox"), false);
    } else {
      await expect(bottom).toHaveCount(0);
      const nav = page.getByRole("navigation");
      await expect(nav.getByRole("link")).toHaveText([
        /^Inbox/,
        "Docs",
        "Delivery",
        "Sessions",
        "Settings",
      ]);
      await touchable(nav.getByRole("link"), false);
    }
    await touchable(rows);
    await noOverflow(page);
    await accessible(page);
  });

  test(`a session shows its timeline, waiting requests and controls at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 800 });
    await mockApi(page, sessions);
    await page.goto(`/sessions/${FIRST}`);

    await expect(
      page.getByRole("heading", { level: 1, name: TITLE }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Waiting for you" }),
    ).toBeVisible();
    await expect(page.getByRole("log")).toContainText("Reading the spec");
    await expect(page.getByText("saka/tasks-done/s1")).toBeVisible();
    await expect(
      page.getByRole("group", { name: "Your choice" }),
    ).toBeVisible();
    await expect(page.getByRole("button", { name: "Stop" })).toBeVisible();

    await touchable(page.getByRole("main").getByRole("button"));
    await touchable(page.getByRole("main").getByRole("link"));
    await noOverflow(page);
    await accessible(page);

    await page.getByRole("button", { name: "Stop" }).click();
    const dialog = page.getByRole("dialog", {
      name: "Stop the agent session",
    });
    await expect(dialog).toContainText(TITLE);
    await touchable(dialog.getByRole("button"));
    await accessible(page);
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
  });

  test(`an empty list and an unknown session say so at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 800 });
    await mockApi(page, []);
    await page.goto("/sessions");
    await expect(page.getByText("No agent session is running")).toBeVisible();
    await noOverflow(page);
    await accessible(page);

    await page.goto("/sessions/0193f0c4-1b2a-7c3d-9e8f-00112233ffff");
    await expect(page.getByText("This session does not exist")).toBeVisible();
    await expect(
      page.getByRole("main").getByRole("link", { name: "All sessions" }),
    ).toBeVisible();
    await accessible(page);
  });
}

test("the sessions area and the bottom navigation follow the chosen language", async ({
  page,
}) => {
  await page.setViewportSize({ width: 375, height: 800 });
  await mockApi(page, sessions);
  await page.goto("/sessions");
  await expect(page.getByRole("main").getByRole("link")).toHaveCount(2);
  await page
    .getByRole("combobox", { name: "Language" })
    .selectOption("Bahasa Indonesia");
  await expect(
    page.getByRole("heading", { level: 1, name: "Sesi" }),
  ).toBeVisible();
  const bottom = page.getByRole("navigation", { name: "Utama" });
  await expect(bottom.getByRole("link")).toHaveText([
    /^Kotak masuk/,
    "Docs",
    "Delivery",
    "Sesi",
  ]);
  await expect(page.getByText("2 sesi aktif")).toBeVisible();
  await touchable(bottom.getByRole("link"));
  await accessible(page);

  await page.goto(`/sessions/${FIRST}`);
  await expect(
    page.getByRole("heading", { name: "Menunggu kamu" }),
  ).toBeVisible();
  await noOverflow(page);
  await accessible(page);
});
