import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { FULL_LOOP } from "./workflow";

const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
const AUTHORIZATION_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344aa";
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const AT = "2026-10-31T10:00:00.000Z";
const FUTURE = "2099-01-01T00:00:00.000Z";

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

const link = {
  id: "0193f0c4-1b2a-7c3d-9e8f-0011223344bb",
  integration_id: "github",
  state: "active",
  relink_cause: null,
  provider_user_id: 1,
  login: "octo",
  name: "Octo Cat",
  email: "1+octo@users.noreply.github.com",
  linked_at: AT,
  refreshed_at: null,
  access_expires_at: FUTURE,
  refresh_expires_at: FUTURE,
};

function authorization(state: string) {
  return {
    id: AUTHORIZATION_ID,
    integration_id: "github",
    state,
    user_code: "WDJB-MJHT",
    verification_uri: "https://github.com/login/device",
    interval_secs: 1,
    expires_at: FUTURE,
    failure: null,
    link: state === "completed" ? link : null,
  };
}

const report = {
  mode: "online",
  server_version: "0.1.0",
  generated_at: AT,
  summary: { pass: 1, warn: 0, fail: 1, skip: 0 },
  checks: [
    {
      id: "agent.login",
      status: "fail",
      code: "agent_not_logged_in",
      params: { account: "claude:default" },
      fix: { code: "run_agent_login", params: { command: "saka agent login" } },
      duration_ms: 4,
      subject: {
        kind: "agent_account",
        id: "claude:default",
        label: "claude:default",
      },
    },
    {
      id: "server.ready",
      status: "pass",
      code: "server_ready",
      params: {},
      fix: null,
      duration_ms: 1,
      subject: { kind: "instance", id: "instance", label: "instance" },
    },
  ],
};

const hook = {
  name: "deploy-staging",
  on: "merged",
  mode: "after",
  action: "run",
  repos: ["app"],
  branches: ["main"],
  codes: [],
  timeout_secs: 300,
  target: {
    command: "./scripts/deploy.sh staging",
    cwd: null,
    url: null,
    secret: null,
    secrets: ["DEPLOY_TOKEN"],
    channel: null,
  },
};

function hookRun(id: string, state: string, error: object | null) {
  return {
    id,
    project_id: PROJECT_ID,
    hook: "deploy-staging",
    action: "run",
    mode: "after",
    trigger: "merged",
    state,
    work_item_id: null,
    task_key: null,
    spec_id: null,
    repo_id: null,
    sha: null,
    context: {},
    rerun_of: null,
    attempts: 1,
    result: { exit_code: error === null ? 0 : 3, output_tail: "log line" },
    error,
    queued_at: AT,
    finished_at: AT,
    duration_ms: 10,
  };
}

type Server = {
  secrets: { name: string; updated_at: string }[];
  polls: number;
};

async function mockApi(page: Page, server: Server) {
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const method = request.method();
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
      await route.fulfill({ json: { items: server.secrets } });
    } else if (path.startsWith(`/api/v1/projects/${PROJECT_ID}/secrets/`)) {
      server.secrets.push({
        name: path.split("/").pop() ?? "",
        updated_at: AT,
      });
      await route.fulfill({ status: 204 });
    } else if (path === `/api/v1/projects/${PROJECT_ID}/authorship`) {
      await route.fulfill({ json: authorship });
    } else if (path === "/api/v1/scm/github/user-link") {
      await route.fulfill(
        server.polls >= 2
          ? { json: link }
          : { status: 404, json: { code: "user_link_not_found", params: {} } },
      );
    } else if (
      path === "/api/v1/scm/github/user-link/authorizations" &&
      method === "POST"
    ) {
      await route.fulfill({ status: 202, json: authorization("pending") });
    } else if (path.endsWith(`/authorizations/${AUTHORIZATION_ID}`)) {
      server.polls += 1;
      await route.fulfill({
        json: authorization(server.polls >= 2 ? "completed" : "pending"),
      });
    } else if (path === `/api/v1/projects/${PROJECT_ID}/hooks`) {
      await route.fulfill({ json: { items: [hook] } });
    } else if (path === "/api/v1/hook-runs") {
      await route.fulfill({
        json: {
          items: [
            hookRun("0193f0c4-1b2a-7c3d-9e8f-0011223355b2", "failed", {
              code: "hook_command_failed",
              params: { exit_code: 3 },
            }),
            hookRun("0193f0c4-1b2a-7c3d-9e8f-0011223355b1", "succeeded", null),
          ],
          next_cursor: null,
        },
      });
    } else if (path === `/api/v1/projects/${PROJECT_ID}/permission-rules`) {
      await route.fulfill({ json: { items: [], operator_rules: [] } });
    } else if (path === "/api/v1/permission-requests") {
      await route.fulfill({ json: { items: [], next_cursor: null } });
    } else if (path === "/api/v1/diagnostics") {
      await route.fulfill({ json: report });
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
  test(`operator manages project settings at ${width} px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await mockApi(page, { secrets: [], polls: 0 });

    await page.goto("/");
    await page
      .getByRole("banner")
      .getByRole("link", { name: "Settings", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "Settings of tasks" }),
    ).toBeVisible();
    await expect(page).toHaveURL(`/settings/${PROJECT_ID}`);
    await accessible(page);

    await page.getByRole("button", { name: "Add secret" }).click();
    const dialog = page.getByRole("dialog", { name: "Add secret" });
    await dialog.getByLabel("Name").fill("NPM_TOKEN");
    await dialog.getByLabel("Value", { exact: true }).fill("s3cret-value");
    await accessible(page);
    await dialog.getByRole("button", { name: "Save" }).click();
    await expect(dialog).toBeHidden();
    await expect(page.getByText("NPM_TOKEN", { exact: true })).toBeVisible();

    const author = page.getByRole("region", { name: "Author" });
    await author.getByRole("button", { name: "Link account" }).click();
    await expect(author.getByText("WDJB-MJHT")).toBeVisible();
    await accessible(page);
    await expect(author.getByText("Linked as octo")).toBeVisible({
      timeout: 10_000,
    });
    await expect(author.getByText("WDJB-MJHT")).toBeHidden();

    const hooks = page.getByRole("region", { name: "Hooks" });
    await expect(hooks.getByText("./scripts/deploy.sh staging")).toBeVisible();
    await expect(
      hooks.getByText("The command exited with code 3."),
    ).toBeVisible();
    await hooks
      .getByRole("button", { name: "Run deploy-staging again" })
      .first()
      .click();
    await expect(
      page.getByRole("dialog", { name: "Run deploy-staging again" }),
    ).toBeVisible();
    await accessible(page);
    await page.keyboard.press("Escape");

    const diagnostics = page.getByRole("region", { name: "Diagnostics" });
    await diagnostics.getByText("Show diagnostics").click();
    await expect(
      diagnostics.getByText("claude:default is not logged in."),
    ).toBeVisible();
    await expect(diagnostics.getByText("saka agent login")).toBeVisible();
    await accessible(page);

    const overflow = await page.evaluate(
      () =>
        document.documentElement.scrollWidth -
        document.documentElement.clientWidth,
    );
    expect(overflow).toBeLessThanOrEqual(0);
    if (width === 375) {
      await touchable(
        page.locator("main").locator("button, summary, select, fieldset label"),
      );
    }
  });
}

test("settings follow the chosen language", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 900 });
  await mockApi(page, { secrets: [], polls: 0 });
  await page.goto(`/settings/${PROJECT_ID}`);
  await expect(
    page.getByRole("heading", { name: "Settings of tasks" }),
  ).toBeVisible();
  await page
    .getByRole("combobox", { name: "Language" })
    .selectOption("Bahasa Indonesia");
  await expect(page.locator("html")).toHaveAttribute("lang", "id");
  await expect(
    page.getByRole("region", { name: "Secret", exact: true }),
  ).toBeVisible();
  await accessible(page);
});
