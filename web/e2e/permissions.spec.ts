import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { FULL_LOOP } from "./workflow";

const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const AT = "2026-10-31T10:00:00.000Z";
const RULES = `/api/v1/projects/${PROJECT_ID}/permission-rules`;

const project = {
  id: PROJECT_ID,
  name: "tasks",
  workspace: {
    repo_id: "0193f0c4-1b2a-7c3d-9e8f-001122334400",
    full_name: "acme/tasks",
    project_path: "docs",
    base_branch: "main",
  },
  repos: [],
  hooks_skipped: [],
  authorship_mode: "bot",
  workflow: FULL_LOOP,
  config_commit: "9f1c".padEnd(40, "0"),
  config_sha256: `sha256:${"4b8e".padEnd(64, "0")}`,
  config_error: null,
  warnings: [],
  created_at: AT,
  updated_at: AT,
};

const authorship = {
  project_id: PROJECT_ID,
  mode: "bot",
  mode_updated_at: null,
  author: {
    source: "saka",
    name: "Saka",
    email: "saka@users.noreply.github.com",
    login: null,
  },
  integrations: [{ integration_id: "github", user_link_state: "missing" }],
  user_token: null,
};

const rule = {
  id: "0193f0c4-1b2a-7c3d-9e8f-0011223344c1",
  project_id: PROJECT_ID,
  effect: "deny",
  tool: "Bash",
  match: "glob",
  command: "rm *",
  path: null,
  note: "never delete",
  permission_request_id: null,
  created_by: "human:operator:local",
  created_at: AT,
  revoked_at: null,
  revoke_reason: null,
};

function request(n: number, source: string | null, summary: string) {
  return {
    id: `0193f0c4-1b2a-7c3d-9e8f-00112233${String(n).padStart(4, "0")}`,
    project_id: PROJECT_ID,
    owner: {
      kind: "stage_run",
      work_item_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344aa",
      stage_run_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344ab",
    },
    session_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344ac",
    tool: "Bash",
    summary,
    input: { command: summary },
    state: source === null ? "pending" : "allowed",
    answer_source: source,
    message: null,
    requested_at: AT,
    answered_at: source === null ? null : AT,
  };
}

type Server = { rules: object[]; saved: object[] };

async function mockApi(page: Page, server: Server) {
  await page.route("**/api/v1/**", async (route) => {
    const call = route.request();
    const url = new URL(call.url());
    const path = url.pathname;
    if (path === "/api/v1/events") {
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: "retry: 600000\n\n",
      });
    } else if (path === "/api/v1/projects") {
      await route.fulfill({ json: { items: [project], next_cursor: null } });
    } else if (path === `/api/v1/projects/${PROJECT_ID}`) {
      await route.fulfill({ json: project });
    } else if (path === `/api/v1/projects/${PROJECT_ID}/secrets`) {
      await route.fulfill({ json: { items: [] } });
    } else if (path === `/api/v1/projects/${PROJECT_ID}/authorship`) {
      await route.fulfill({ json: authorship });
    } else if (path === `/api/v1/projects/${PROJECT_ID}/hooks`) {
      await route.fulfill({ json: { items: [] } });
    } else if (path === "/api/v1/hook-runs") {
      await route.fulfill({ json: { items: [], next_cursor: null } });
    } else if (path === "/api/v1/scm/github/user-link") {
      await route.fulfill({
        status: 404,
        json: { code: "user_link_not_found", params: {} },
      });
    } else if (path === RULES && call.method() === "GET") {
      await route.fulfill({
        json: {
          items: server.rules,
          operator_rules: [
            {
              id: "safe-commands",
              effect: "allow",
              tools: ["Bash"],
              commands: ["go test *", "ls*"],
              paths: [],
            },
          ],
        },
      });
    } else if (path === `${RULES}/preview`) {
      await route.fulfill({
        json: {
          scanned: 12,
          changed: [
            {
              permission_request_id: "0193f0c4-1b2a-7c3d-9e8f-001122330001",
              tool: "Bash",
              command: "npm install",
              before: "ask",
              after: "allow",
            },
          ],
        },
      });
    } else if (path === RULES && call.method() === "POST") {
      server.saved.push(call.postDataJSON() as object);
      server.rules.push({ ...rule, id: "new", command: "npm *", note: null });
      await route.fulfill({ status: 201, json: server.rules.at(-1) });
    } else if (path === "/api/v1/permission-requests") {
      const policy = url.searchParams.get("source") === "policy";
      await route.fulfill({
        json: {
          items: policy
            ? [request(2, "policy:safe-commands", "go test ./...")]
            : [
                request(1, "human:operator:local", "npm install"),
                request(2, "policy:safe-commands", "go test ./..."),
                request(3, null, "make deploy && echo ok"),
              ],
          next_cursor: null,
        },
      });
    } else {
      await route.fulfill({
        status: 404,
        json: { code: "not_found", params: {} },
      });
    }
  });
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

async function accessible(page: Page) {
  const results = await new AxeBuilder({ page }).withTags(AXE_TAGS).analyze();
  expect(results.violations).toEqual([]);
}

for (const width of [375, 1280]) {
  test(`operator manages permission rules at ${width} px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    const server: Server = { rules: [rule], saved: [] };
    await mockApi(page, server);
    await page.goto(`/settings/${PROJECT_ID}`);

    const permissions = page.getByRole("region", { name: "Permissions" });
    await expect(
      permissions.getByText("safe-commands", { exact: true }),
    ).toBeVisible();
    await expect(permissions.getByText("never delete")).toBeVisible();
    await expect(permissions.getByText("Answered by you")).toBeVisible();
    await accessible(page);

    await permissions
      .getByRole("button", { name: "A rule", exact: true })
      .click();
    await expect(
      permissions.getByText("Operator rule safe-commands"),
    ).toBeVisible();
    await expect(permissions.getByText("npm install")).toBeHidden();

    await permissions.getByRole("button", { name: "Add a rule" }).click();
    const dialog = page.getByRole("dialog", { name: "Add a permission rule" });
    await dialog.getByLabel("Match").selectOption("glob");
    await dialog.getByLabel("Command").fill("npm *");
    await expect(
      dialog.getByRole("button", { name: "Save rule" }),
    ).toBeDisabled();
    await dialog.getByRole("button", { name: "Preview" }).click();
    await expect(
      dialog.getByText("1 of the last 12 requests would change."),
    ).toBeVisible();
    await accessible(page);
    await dialog.getByRole("button", { name: "Save rule" }).click();
    await expect(dialog).toBeHidden();
    expect(server.saved).toEqual([
      {
        effect: "allow",
        tool: "Bash",
        match: "glob",
        command: "npm *",
        path: null,
        note: null,
      },
    ]);

    await permissions.getByRole("button", { name: "Revoke rm *" }).click();
    await expect(
      page.getByRole("dialog", { name: "Revoke this rule?" }),
    ).toBeVisible();
    await accessible(page);
    await page.keyboard.press("Escape");

    const overflow = await page.evaluate(
      () =>
        document.documentElement.scrollWidth -
        document.documentElement.clientWidth,
    );
    expect(overflow).toBeLessThanOrEqual(0);
    if (width === 375) {
      await touchable(
        permissions.locator("button, select, summary, fieldset label"),
      );
    }
  });
}

test("permission rules follow the chosen language", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 900 });
  await mockApi(page, { rules: [rule], saved: [] });
  await page.goto(`/settings/${PROJECT_ID}`);
  await expect(page.getByRole("region", { name: "Permissions" })).toBeVisible();
  await page
    .getByRole("combobox", { name: "Language" })
    .selectOption("Bahasa Indonesia");
  const permissions = page.getByRole("region", { name: "Izin" });
  await expect(
    permissions.getByRole("heading", { name: "Aturan operator" }),
  ).toBeVisible();
  await expect(permissions.getByText("Dijawab oleh Anda")).toBeVisible();
  await permissions.getByRole("button", { name: "Tambah aturan" }).click();
  const dialog = page.getByRole("dialog", { name: "Tambah aturan izin" });
  await dialog.getByLabel("Perintah").fill("npm *");
  await dialog.getByRole("button", { name: "Pratinjau" }).click();
  await expect(
    dialog.getByText("1 dari 12 permintaan terakhir akan berubah."),
  ).toBeVisible();
  await accessible(page);
});
