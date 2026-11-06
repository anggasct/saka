import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { FULL_LOOP } from "./workflow";

const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
const SPEC_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334466";
const SESSION_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344f1";
const PERMISSION_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a1";
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const AT = "2026-10-31T10:00:00.000Z";
const HEAD = "a1b2c3d4e5f6".padEnd(40, "0");

const project = {
  id: PROJECT_ID,
  name: "tasks-with-a-rather-long-project-name",
  workspace: {
    repo_id: "r",
    full_name: "acme/tasks",
    project_path: "docs",
    base_branch: "main",
  },
  repos: [],
  autonomy: "standard",
  authorship_mode: "bot",
  workflow: FULL_LOOP,
  config_commit: "c",
  created_at: AT,
  updated_at: AT,
};

const spec = {
  id: SPEC_ID,
  project_id: PROJECT_ID,
  path: "docs/specs/tasks-done.md",
  slug: "tasks-done",
  title: "Mark tasks as done",
  state: "approved",
  origin: "manual",
  priority: 0,
  revision: { id: "r", source_commit: "c", content_hash: "h", created_at: AT },
  approved_revision: { id: "r", source_commit: "c", content_hash: "h" },
  lint: { ok: true, issues: [] },
  steps: [],
  approved_at: null,
  shipped_at: null,
  created_at: AT,
  updated_at: AT,
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
    work: "Add a done command to the CLI store so that tasks can be closed from the terminal",
    repo_id: "r",
    branch: `saka/tasks-done/s${step}`,
    state: "in_stage",
    stage: "merge",
    stage_phase: "awaiting_merge",
    priority: 0,
    waiting_on: [],
    change_request: {
      id: "c",
      number: 12,
      url: "https://github.com/acme/tasks/pull/12",
      state: "open",
      head_sha: HEAD,
      base_branch: "main",
      merge_commit_sha: null,
      merged_at: null,
    },
    gates: [
      {
        source: "scm",
        required: true,
        head_sha: HEAD,
        outcome: "passed",
        checks: [],
        observed_at: AT,
      },
    ],
    blocker: null,
    session: {
      id: SESSION_ID,
      cli: "claude",
      state: "waiting",
      started_at: AT,
    },
    pending_permission_count: 0,
    available_actions: ["merge", "cancel"],
    version: 3,
    created_at: AT,
    updated_at: AT,
    admitted_at: AT,
    done_at: null,
    stage_runs: [],
    ...extra,
  };
}

const ready = task(1, {});
const blocked = task(2, {
  state: "blocked",
  stage: "dev",
  stage_phase: null,
  change_request: null,
  gates: [],
  session: null,
  blocker: {
    id: "b",
    code: "session_failed",
    params: {},
    opened_at: AT,
    stage: "dev",
    primary_action: { kind: "retry", params: {} },
  },
  available_actions: ["retry", "cancel"],
});

const taken = task(3, {
  state: "taken_over",
  stage: null,
  stage_phase: null,
  session: null,
  available_actions: ["cancel", "return"],
});

const request = {
  id: PERMISSION_ID,
  project_id: PROJECT_ID,
  owner: { kind: "stage_run", work_item_id: ready.id, stage_run_id: "s" },
  session_id: SESSION_ID,
  tool: "Bash",
  summary:
    "npm install left-pad --save-exact --registry https://registry.npmjs.org",
  input: { command: "npm install left-pad" },
  state: "pending",
  requested_at: AT,
  answer_source: null,
  answered_at: null,
  message: null,
};

const session = {
  id: SESSION_ID,
  project_id: PROJECT_ID,
  owner: {
    kind: "stage_run",
    work_item_id: ready.id,
    stage_run_id: "s",
  },
  cli: "claude",
  cli_version: "2.1.0",
  model: "opus",
  state: "waiting",
  turns: 1,
  running_ms: 60_000,
  cost_usd: null,
  input_tokens: null,
  output_tokens: null,
  resumed_from_session_id: null,
  resume_cause: null,
  end_reason: null,
  available_actions: ["stop"],
  started_at: AT,
  ended_at: null,
};

type Server = {
  pending: boolean;
  posts: { path: string; ifMatch: string | undefined }[];
};

const page_ = (items: unknown[]) => ({ items, next_cursor: null });

async function mockApi(page: Page, server: Server) {
  await page.route("**/api/v1/**", async (route) => {
    const req = route.request();
    const path = new URL(req.url()).pathname;
    const json = (body: unknown, status = 200) =>
      route.fulfill({ status, json: body });
    const tasks = () => [
      { ...ready, pending_permission_count: server.pending ? 1 : 0 },
      blocked,
    ];
    if (req.method() === "POST") {
      server.posts.push({ path, ifMatch: req.headers()["if-match"] });
      if (path.endsWith("/answer")) {
        server.pending = false;
        await json({ ...request, state: "allowed" });
      } else {
        await json(ready, 202);
      }
    } else if (path === "/api/v1/events") {
      await route.fulfill({ status: 204 });
    } else if (path === "/api/v1/projects") {
      await json(page_([project]));
    } else if (path.endsWith("/specs")) {
      await json(page_([spec]));
    } else if (path === "/api/v1/work-items") {
      await json(page_(tasks()));
    } else if (path.startsWith("/api/v1/work-items/")) {
      const found = [...tasks(), taken].find((t) => path.endsWith(t.id));
      await (found === undefined
        ? json({ code: "not_found", params: {} }, 404)
        : json(found));
    } else if (path === `/api/v1/specs/${SPEC_ID}`) {
      await json(spec);
    } else if (path === `/api/v1/projects/${PROJECT_ID}`) {
      await json(project);
    } else if (path === "/api/v1/permission-requests") {
      await json(page_(server.pending ? [request] : []));
    } else if (path === "/api/v1/sessions") {
      const id = new URL(req.url()).searchParams.get("work_item_id");
      await json(page_(id === ready.id ? [session] : []));
    } else if (path === `/api/v1/sessions/${SESSION_ID}/timeline`) {
      await json({
        items: [
          {
            offset: 0,
            at: AT,
            kind: "message",
            summary: "Installing",
            data: {},
          },
          {
            offset: 1,
            at: AT,
            kind: "permission",
            summary: request.summary,
            data: { permission_request_id: PERMISSION_ID, state: "pending" },
          },
        ],
        prev_before: null,
        next_after: null,
      });
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

async function checkDialog(page: Page, trigger: Locator, name: string) {
  await trigger.click();
  const dialog = page.getByRole("dialog", { name });
  await expect(dialog).toBeVisible();
  await touchable(dialog.getByRole("button"));
  await accessible(page);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
}

for (const width of [375, 1280]) {
  test(`operator answers a permission and merges at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    const server: Server = { pending: true, posts: [] };
    await mockApi(page, server);

    await page.goto("/delivery");
    const panel = page.getByRole("region", { name: "Waiting for you (1)" });
    const card = panel.getByRole("article", {
      name: "Bash asks for permission",
    });
    await expect(card).toBeVisible();
    await noOverflow(page);
    await touchable(card.getByRole("button"));
    await accessible(page);
    await checkDialog(
      page,
      card.getByRole("button", { name: "Deny" }),
      "Deny the request",
    );
    await checkDialog(
      page,
      card.getByRole("button", { name: "Allow always for this repo" }),
      "Allow always for this repo",
    );

    await page.getByRole("link", { name: "Step S1" }).last().click();
    await expect(
      page.getByRole("heading", { level: 1, name: "Step S1" }),
    ).toBeVisible();
    const log = page.getByRole("log", { name: "Session timeline" });
    await expect(
      log.getByRole("article", { name: "Bash asks for permission" }),
    ).toBeVisible();
    await noOverflow(page);
    const header = page.getByRole("main");
    await touchable(header.getByRole("button"));
    await accessible(page);
    await checkDialog(
      page,
      header.getByRole("button", { name: "Cancel task" }),
      "Cancel the task",
    );

    await page.getByRole("link", { name: "Back to Delivery" }).click();
    await card.getByRole("button", { name: "Allow", exact: true }).click();
    await expect(panel).toBeHidden();
    expect(server.posts.map((p) => p.path)).toEqual([
      `/api/v1/permission-requests/${PERMISSION_ID}/answer`,
    ]);

    await page.goto(`/delivery/tasks/${blocked.id}`);
    const retry = page.getByRole("region", { name: "Blocker" });
    await checkDialog(
      page,
      retry.getByRole("button", { name: "Retry" }),
      "Retry the blocked stage",
    );

    await page.goto(`/delivery/tasks/${taken.id}`);
    await expect(
      page.getByRole("region", { name: "Taken over" }),
    ).toBeVisible();
    await noOverflow(page);
    await touchable(page.getByRole("main").getByRole("button"));
    await accessible(page);
    await checkDialog(
      page,
      page
        .getByRole("main")
        .getByRole("button", { name: "Return to the loop" }),
      "Return the task to the loop",
    );

    await page.goto(`/delivery/tasks/${ready.id}`);
    await expect(log.getByText("Waiting for an answer")).toBeVisible();
    await checkDialog(
      page,
      header.getByRole("button", { name: "Merge" }),
      "Merge the pull request",
    );
    await header.getByRole("button", { name: "Merge" }).click();
    const merge = page.getByRole("dialog", { name: "Merge the pull request" });
    await merge.getByRole("button", { name: "Merge the pull request" }).click();
    await expect(merge).toBeHidden();
    expect(server.posts.at(-1)).toEqual({
      path: `/api/v1/work-items/${ready.id}/actions/merge`,
      ifMatch: 'W/"3"',
    });
  });
}
