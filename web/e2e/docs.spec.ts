import { createHash } from "node:crypto";

import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { FULL_LOOP } from "./workflow";

const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
const SPEC_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334466";
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const AT = "2026-10-31T10:00:00.000Z";
const PATH = "docs/specs/tasks-done.md";
const COMMIT = "9f1c".padEnd(40, "0");

const SPEC = [
  "---",
  "saka: spec",
  "---",
  "# Mark tasks as done",
  "",
  "## Acceptance criteria",
  "",
  "- **AC-01**: A task can be marked done.",
  "",
  "## Steps",
  "",
  "| Step | Work | AC | Done when |",
  "|------|------|----|-----------|",
  "| S1 | Store the flag | AC-01 | `make test` |",
  "| S2 | Show the flag | AC-09 | `make test` |",
  "",
].join("\r\n");

function blobOf(content: string): string {
  const bytes = Buffer.from(content, "utf8");
  return createHash("sha1")
    .update(`blob ${bytes.length}\0`)
    .update(bytes)
    .digest("hex");
}

function hashOf(content: string): string {
  return `sha256:${createHash("sha256").update(content, "utf8").digest("hex")}`;
}

const project = {
  id: PROJECT_ID,
  name: "tasks",
  workspace: {
    repo_id: "r",
    full_name: "acme/tasks",
    project_path: "docs",
    base_branch: "main",
  },
  repos: [],
  hooks_skipped: [],
  authorship_mode: "bot",
  workflow: FULL_LOOP,
  config_commit: COMMIT,
  created_at: AT,
  updated_at: AT,
};

function task(step: number, extra: object) {
  return {
    id: `0193f0c4-1b2a-7c3d-9e8f-00112233450${step}`,
    project_id: PROJECT_ID,
    origin: "spec_step",
    spec_id: SPEC_ID,
    spec_revision_id: "rev",
    step_id: `S${step}`,
    step_index: step,
    task_key: `tasks-done/s${step}`,
    work: "w",
    repo_id: "r",
    branch: `saka/tasks-done/s${step}`,
    state: "queued",
    stage: null,
    stage_phase: null,
    priority: 0,
    waiting_on: [],
    change_request: null,
    gates: [],
    deploy_gates: [],
    blocker: null,
    session: null,
    pending_permission_count: 0,
    available_actions: [],
    version: 1,
    created_at: AT,
    updated_at: AT,
    admitted_at: null,
    done_at: null,
    ...extra,
  };
}

const tasks = [
  task(1, { state: "done", done_at: AT }),
  task(2, { state: "in_stage", stage: "dev", stage_phase: "running_session" }),
];

const spec = {
  id: SPEC_ID,
  project_id: PROJECT_ID,
  path: PATH,
  slug: "tasks-done",
  title: "Mark tasks as done",
  state: "approved",
  origin: "manual",
  priority: 0,
  depends_on: [],
  revision: {
    id: "rev",
    source_commit: COMMIT,
    content_hash: hashOf(SPEC),
    parser_version: 4,
    context_hash: "sha256:0",
    context: [],
    created_at: AT,
  },
  approved_revision: {
    id: "rev",
    source_commit: COMMIT,
    content_hash: hashOf(SPEC),
  },
  revision_plan: null,
  lint: { ok: true, issues: [] },
  steps: tasks.map((t, i) => ({
    id: t.step_id,
    index: i + 1,
    work: "w",
    ac: [],
    commands: [],
    after: [],
    work_item_id: t.id,
  })),
  decisions: [],
  approved_at: AT,
  shipped_at: null,
  created_at: AT,
  updated_at: AT,
};

type Server = { puts: { body: unknown; ifMatch: string | null }[] };

async function mockApi(page: Page, server: Server) {
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    const json = (body: unknown, status = 200) =>
      route.fulfill({
        status,
        contentType:
          status >= 400 ? "application/problem+json" : "application/json",
        json: body,
      });
    if (path === "/api/v1/events") {
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: "retry: 60000\n\n",
      });
    } else if (path === "/api/v1/projects") {
      await json({ items: [project], next_cursor: null });
    } else if (path === `/api/v1/projects/${PROJECT_ID}/files`) {
      await json({
        commit: COMMIT,
        base_branch: "main",
        project_path: "docs",
        files: [
          { path: "docs/architecture.md", blob: blobOf("# A\n"), bytes: 4 },
          { path: PATH, blob: blobOf(SPEC), bytes: SPEC.length },
        ],
        truncated: false,
      });
    } else if (
      path === `/api/v1/projects/${PROJECT_ID}/file` &&
      request.method() === "PUT"
    ) {
      const body: unknown = request.postDataJSON();
      server.puts.push({
        body,
        ifMatch: request.headers()["if-match"] ?? null,
      });
      if (server.puts.length === 1) {
        await json(
          {
            code: "file_stale",
            params: { blob: "b".repeat(40), commit: "c".repeat(40) },
          },
          412,
        );
      } else {
        const content = (body as { content: string }).content;
        await json({
          path: PATH,
          commit: "d".repeat(40),
          blob: blobOf(content),
          bytes: content.length,
          committed: true,
          spec: null,
          spec_problem: null,
        });
      }
    } else if (path === `/api/v1/projects/${PROJECT_ID}/file`) {
      await json({
        path: PATH,
        commit: COMMIT,
        blob: blobOf(SPEC),
        bytes: SPEC.length,
        content: SPEC,
      });
    } else if (path === `/api/v1/projects/${PROJECT_ID}/specs`) {
      await json({ items: [spec], next_cursor: null });
    } else if (path === "/api/v1/work-items") {
      await json({ items: tasks, next_cursor: null });
    } else if (path === "/api/v1/spec-template") {
      await json({ content: SPEC, locale: "en", parser_version: 4 });
    } else if (path === "/api/v1/permission-requests") {
      await json({ items: [], next_cursor: null });
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

async function touchable(targets: Locator) {
  for (const target of await targets.all()) {
    if (!(await target.isVisible())) {
      continue;
    }
    const box = await target.boundingBox();
    expect(box?.height ?? 0).toBeGreaterThanOrEqual(44);
    expect(box?.width ?? 0).toBeGreaterThanOrEqual(44);
  }
}

for (const width of [375, 1280]) {
  test(`operator edits and saves a spec at ${width} px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    const server: Server = { puts: [] };
    await mockApi(page, server);

    await page.goto("/docs");
    await expect(page).toHaveURL(new RegExp(`/docs/${PROJECT_ID}`));
    if (width < 1024) {
      const files = page.getByRole("button", { name: "Files", exact: true });
      await expect(files).toHaveAttribute("aria-expanded", "false");
      await files.click();
    }
    const tree = page.getByRole("navigation", { name: "Files" });
    await tree.getByRole("button", { name: "specs" }).click();
    await tree.getByRole("link", { name: "tasks-done.md" }).click();

    const editor = page.getByRole("textbox", { name: `Editor for ${PATH}` });
    await expect(editor).toBeVisible();
    await expect(page.locator(".cm-chip")).toHaveText([
      "Done",
      "Development · agent working",
    ]);
    await expect(page.locator(".cm-lint-marker-error").first()).toBeVisible();

    await noOverflow(page);
    await touchable(page.getByRole("button", { name: "Save" }));
    await touchable(page.getByRole("button", { name: "Source" }));
    await touchable(page.getByRole("button", { name: "New spec" }));
    await accessible(page);

    await editor.click();
    await page.keyboard.press("Control+End");
    await page.keyboard.press("Enter");
    await page.keyboard.type("Done.");
    await page.keyboard.press("Control+s");
    await expect(
      page.getByText("This file changed on main since you opened it."),
    ).toBeVisible();
    expect(server.puts[0]).toEqual({
      body: { content: `${SPEC}\r\nDone.` },
      ifMatch: `"${blobOf(SPEC)}"`,
    });
    await expect(
      page.getByText(/^Changed since the approved revision 9f1c00000000/),
    ).toBeVisible();

    await page.getByRole("button", { name: "Take theirs" }).click();
    const confirm = page.getByRole("dialog", {
      name: "Take the latest version?",
    });
    await expect(confirm).toBeVisible();
    await touchable(confirm.getByRole("button"));
    await accessible(page);
    await page.keyboard.press("Escape");
    await expect(confirm).toBeHidden();

    await page.getByRole("button", { name: "Keep mine" }).click();
    await expect(page.getByText("Saved · dddddddddddd")).toBeVisible();
    expect(server.puts[1]).toEqual({
      body: { content: `${SPEC}\r\nDone.` },
      ifMatch: `"${"b".repeat(40)}"`,
    });

    if (width < 1024) {
      const files = page.getByRole("button", { name: "Files", exact: true });
      await expect(files).toHaveAttribute("aria-expanded", "false");
      await files.click();
    }
    await touchable(page.getByRole("button", { name: "New spec" }));
    await page.getByRole("button", { name: "New spec" }).click();
    const dialog = page.getByRole("dialog", { name: "New spec" });
    await expect(dialog).toBeVisible();
    await touchable(dialog.getByRole("button"));
    await accessible(page);
    await noOverflow(page);
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();

    await touchable(page.getByRole("button", { name: "Quick task" }));
    await page.getByRole("button", { name: "Quick task" }).click();
    const quick = page.getByRole("dialog", { name: "Quick task" });
    await expect(quick).toBeVisible();
    await quick.getByRole("radio", { name: /Bugfix/ }).check();
    await quick.getByRole("textbox", { name: "Title" }).fill("Login loop");
    await quick.getByRole("textbox", { name: /^Done when/ }).fill("make test");
    await quick
      .getByRole("textbox", { name: /^Regression/ })
      .fill("cargo test --test login");
    await expect(
      quick.getByText("File: docs/specs/bugfix-login-loop.md"),
    ).toBeVisible();
    await touchable(quick.getByRole("button"));
    await accessible(page);
    await noOverflow(page);
    await page.keyboard.press("Escape");
    await expect(quick).toBeHidden();
  });
}
