import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
const TASK_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334501";
const SESSION_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344f1";
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const AT = "2026-10-31T10:00:00.000Z";

const task = {
  id: TASK_ID,
  project_id: PROJECT_ID,
  origin: "spec_step",
  spec_id: null,
  spec_revision_id: null,
  step_id: "S1",
  step_index: 1,
  task_key: "tasks-done/s1",
  work: "Add a done command",
  repo_id: "r",
  branch: "saka/tasks-done/s1",
  state: "in_stage",
  stage: "dev",
  stage_phase: "running_session",
  priority: 0,
  waiting_on: [],
  change_request: null,
  gates: [],
  deploy_gates: [],
  blocker: null,
  session: { id: SESSION_ID, cli: "claude", state: "running", started_at: AT },
  pending_permission_count: 0,
  available_actions: ["cancel"],
  version: 1,
  created_at: AT,
  updated_at: AT,
  admitted_at: AT,
  done_at: null,
  stage_runs: [],
  reviews: [],
};

const session = {
  id: SESSION_ID,
  project_id: PROJECT_ID,
  owner: {
    kind: "stage_run",
    work_item_id: TASK_ID,
    stage_run_id: "s",
  },
  cli: "claude",
  cli_version: "2.1.0",
  model: "opus",
  state: "running",
  turns: 1,
  running_ms: 60_000,
  cost_usd: null,
  input_tokens: null,
  output_tokens: null,
  resumed_from_session_id: null,
  resume_cause: null,
  end_reason: null,
  available_actions: ["steer", "interrupt", "stop"],
  started_at: AT,
  ended_at: null,
};

function row(offset: number) {
  return {
    offset,
    at: AT,
    kind: "message",
    summary: `row ${offset}`,
    data: {},
  };
}

type Server = { total: number; notices: number[] };

async function mockApi(page: Page, server: Server) {
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    if (path === "/api/v1/events") {
      const deadline = Date.now() + 10_000;
      while (server.notices.length === 0 && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      const lines = server.notices.splice(0).map(
        (offset) =>
          `event: runtime.session.activity.v1\ndata: ${JSON.stringify({
            session_id: SESSION_ID,
            owner: session.owner,
            offset,
            kind: "message",
          })}\n\n`,
      );
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: `retry: 100\n\n${lines.join("")}`,
      });
    } else if (path === `/api/v1/work-items/${TASK_ID}`) {
      await route.fulfill({ json: task });
    } else if (path === "/api/v1/sessions") {
      await route.fulfill({ json: { items: [session] } });
    } else if (path === `/api/v1/sessions/${SESSION_ID}/timeline`) {
      const after = url.searchParams.get("after");
      const from = after === null ? 0 : Number(after) + 1;
      const items = [];
      for (let o = from; o < server.total; o++) {
        items.push(row(o));
      }
      await route.fulfill({
        json: { items, prev_before: null, next_after: null },
      });
    } else if (path === "/api/v1/work-items") {
      await route.fulfill({ json: { items: [task], next_cursor: null } });
    } else {
      await route.fulfill({
        status: 404,
        json: { code: "not_found", params: {} },
      });
    }
  });
}

async function atBottom(page: Page): Promise<boolean> {
  return page
    .getByRole("log")
    .evaluate(
      (log) => log.scrollHeight - log.scrollTop - log.clientHeight <= 48,
    );
}

test("the timeline follows new activity until the operator scrolls up", async ({
  page,
}) => {
  await page.setViewportSize({ width: 375, height: 800 });
  const server: Server = { total: 40, notices: [] };
  await mockApi(page, server);
  await page.goto(`/delivery/tasks/${TASK_ID}`);
  const log = page.getByRole("log", { name: "Session timeline" });
  await expect(log.getByText("row 39", { exact: true })).toBeVisible();
  expect(await atBottom(page)).toBe(true);

  server.total = 41;
  server.notices.push(40);
  await expect(log.getByText("row 40", { exact: true })).toBeVisible();
  await expect.poll(() => atBottom(page)).toBe(true);

  await log.evaluate((el) => {
    el.scrollTop = 0;
  });
  await expect.poll(() => atBottom(page)).toBe(false);
  server.total = 42;
  server.notices.push(41);
  const newer = page.getByRole("button", { name: "1 new activity" });
  await expect(log.getByText("row 41", { exact: true })).toBeAttached();
  await expect(newer).toBeVisible();
  expect(await atBottom(page)).toBe(false);
  await newer.click();
  await expect.poll(() => atBottom(page)).toBe(true);
  await expect(newer).toBeHidden();
});

for (const width of [375, 1280]) {
  test(`the stop dialog is accessible at ${width} px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await mockApi(page, { total: 5, notices: [] });
    await page.goto(`/delivery/tasks/${TASK_ID}`);
    await expect(
      page.getByRole("log").getByText("row 4", { exact: true }),
    ).toBeVisible();
    const page_ = await new AxeBuilder({ page }).withTags(AXE_TAGS).analyze();
    expect(page_.violations).toEqual([]);
    await page.getByRole("button", { name: "Stop" }).click();
    const dialog = page.getByRole("dialog", { name: "Stop the agent session" });
    await expect(dialog).toBeVisible();
    const results = await new AxeBuilder({ page }).withTags(AXE_TAGS).analyze();
    expect(results.violations).toEqual([]);
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(page.getByRole("button", { name: "Stop" })).toBeFocused();
  });
}
