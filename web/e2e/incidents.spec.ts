import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
const INCIDENT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a1";
const OTHER_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a2";
const DEPLOYMENT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a3";
const TASK_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a4";
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const AT = "2026-11-06T10:00:00.000Z";

const incident = {
  id: INCIDENT_ID,
  project_id: PROJECT_ID,
  source: "prod-alerts",
  fingerprint: "HighErrorRate/checkout",
  state: "open",
  signal_status: "firing",
  severity: "high",
  title: "Checkout error rate above 5% on the production cluster",
  summary: "5xx responses on /checkout rose after the last deployment.",
  environment: "staging",
  url: "https://alerts.example.com/alert/42",
  labels: { service: "checkout", alertname: "HighErrorRate" },
  occurrences: 3,
  started_at: AT,
  first_seen: AT,
  last_seen: AT,
  deployment: {
    id: DEPLOYMENT_ID,
    state: "succeeded",
    sha: "feedface".padEnd(40, "0"),
    phase: null,
    finished_at: AT,
  },
};

const other = {
  ...incident,
  id: OTHER_ID,
  title: "Queue depth growing",
  severity: "low",
  state: "resolved",
  resolution: "signal_resolved",
  occurrences: 1,
};

const detail = {
  ...incident,
  range: {
    previous_sha: "c0ffee".padEnd(40, "0"),
    sha: "feedface".padEnd(40, "0"),
  },
  suspects: [
    { kind: "task", work_item: { id: TASK_ID, key: "tasks-done/s1" } },
    {
      kind: "change",
      change: { number: 12, url: "https://github.com/acme/app/pull/12" },
    },
  ],
};

async function mockApi(page: Page) {
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/v1/events") {
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: "retry: 600000\n\n",
      });
    } else if (path === `/api/v1/projects/${PROJECT_ID}/incidents`) {
      await route.fulfill({
        json: { items: [incident, other], next_cursor: null },
      });
    } else if (path === `/api/v1/incidents/${INCIDENT_ID}`) {
      await route.fulfill({ json: detail });
    } else if (path === `/api/v1/projects/${PROJECT_ID}/environments`) {
      await route.fulfill({ json: { items: [] } });
    } else {
      await route.fulfill({
        status: 404,
        json: { code: "not_found", params: {} },
      });
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

async function touchable(targets: Locator) {
  for (const target of await targets.all()) {
    if (!(await target.isVisible())) {
      continue;
    }
    const box = await target.boundingBox();
    expect(box?.height ?? 0).toBeGreaterThanOrEqual(44);
  }
}

for (const width of [375, 1280]) {
  test(`an operator reads an incident and opens its actions at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await mockApi(page);
    await page.goto(`/incidents/${PROJECT_ID}`);

    await expect(
      page.getByRole("heading", { level: 1, name: "Incidents" }),
    ).toBeVisible();
    await expect(page.getByText("Queue depth growing")).toBeVisible();
    await noOverflow(page);
    await accessible(page);

    await page
      .getByRole("link", { name: /^Checkout error rate above 5%/ })
      .click();
    const panel = page.getByRole("region", {
      name: /^Checkout error rate above 5%/,
    });
    await expect(
      panel.getByRole("link", { name: "tasks-done/s1" }),
    ).toBeVisible();
    await expect(panel.getByText("c0ffee000000 → feedface0000")).toBeVisible();
    await noOverflow(page);
    await touchable(page.getByRole("main").getByRole("button"));
    await touchable(page.getByRole("main").getByRole("link"));
    await accessible(page);

    await panel.getByRole("button", { name: "Revert" }).click();
    const revert = page.getByRole("dialog", { name: "Revert tasks-done/s1" });
    await expect(revert).toBeVisible();
    await touchable(revert.getByRole("button"));
    await accessible(page);
    await page.keyboard.press("Escape");
    await expect(revert).toBeHidden();

    await panel.getByRole("button", { name: "Dismiss" }).click();
    const dismiss = page.getByRole("dialog", { name: "Dismiss this incident" });
    await expect(dismiss.getByLabel("Reason")).toBeFocused();
    await accessible(page);
    await page.keyboard.press("Escape");
    await expect(dismiss).toBeHidden();
  });
}

test("incidents follow the chosen language", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 900 });
  await mockApi(page);
  await page.goto(`/incidents/${PROJECT_ID}?incident=${INCIDENT_ID}`);
  await page
    .getByRole("combobox", { name: "Language" })
    .selectOption("Bahasa Indonesia");
  await expect(page.locator("html")).toHaveAttribute("lang", "id");
  await expect(
    page.getByRole("heading", { level: 1, name: "Insiden" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Buat draf perbaikan" }),
  ).toBeVisible();
  await accessible(page);
});
