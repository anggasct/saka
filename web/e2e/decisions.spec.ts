import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { FULL_LOOP } from "./workflow";

const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
const SPEC_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334466";
const TASK_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a1";
const DRAFT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344d1";
const ATTEMPT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a2";
const DECISION_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344e1";
const CLOSED_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344e2";
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const AT = "2026-11-06T10:00:00.000Z";
const COMMIT = "9f1c".padEnd(40, "0");
const QUESTION = "Where do we store the recorded audio files for every user?";

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
  hooks_skipped: [],
  authorship_mode: "bot",
  workflow: FULL_LOOP,
  config_commit: COMMIT,
  created_at: AT,
  updated_at: AT,
};

const options = [
  {
    label: "Local disk under the data folder with a long label to wrap",
    detail: "Simple and free; backups are on you.",
  },
  { label: "Cloudflare R2", detail: null },
];

function decision(over: Record<string, unknown> = {}) {
  return {
    id: DECISION_ID,
    project_id: PROJECT_ID,
    owner: { kind: "stage_run", work_item_id: TASK_ID, stage_run_id: "s" },
    state: "asked",
    question: QUESTION,
    context: "The recorder needs a place to keep files between sessions.",
    options,
    answer: null,
    asked_at: AT,
    expires_at: "2026-11-09T10:00:00.000Z",
    ...over,
  };
}

const closed = decision({
  id: CLOSED_ID,
  state: "delivered",
  question: "Which audio format?",
  answer: {
    option: 1,
    note: "Smaller files",
    answered_by: "operator:local",
    answered_at: AT,
  },
});

const inboxItem = {
  id: "0193f0c4-1b2a-7c3d-9e8f-001122339901",
  project_id: PROJECT_ID,
  project_name: project.name,
  kind: "decision",
  subject: "decision",
  state: "open",
  work_item_id: TASK_ID,
  spec_id: SPEC_ID,
  title: {
    task_key: "tasks-done/s1",
    stage: "dev",
    spec_slug: "tasks-done",
    spec_title: "Mark tasks as done",
    spec_path: "docs/specs/tasks-done.md",
    draft_path: null,
  },
  params: {
    question: QUESTION,
    context: "The recorder needs a place to keep files between sessions.",
    options,
    expires_at: "2026-11-09T10:00:00.000Z",
  },
  action: {
    kind: "answer_decision",
    params: { decision_request_id: DECISION_ID },
  },
  dismissible: false,
  opened_at: AT,
  closed_at: null,
};

const task = {
  id: TASK_ID,
  project_id: PROJECT_ID,
  origin: "spec_step",
  spec_id: SPEC_ID,
  spec_revision_id: "r",
  step_id: "S1",
  step_index: 1,
  task_key: "tasks-done/s1",
  work: "Record audio",
  repo_id: "x",
  branch: "saka/tasks-done/s1",
  state: "in_stage",
  stage: "dev",
  stage_phase: "running_session",
  priority: 0,
  change_request: null,
  gates: [],
  waiting_on: [],
  blocker: null,
  session: null,
  pending_permission_count: 0,
  available_actions: ["cancel"],
  version: 3,
  created_at: AT,
  updated_at: AT,
  admitted_at: AT,
  done_at: null,
  stage_runs: [],
  reviews: [],
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

const draft = {
  id: DRAFT_ID,
  project_id: PROJECT_ID,
  path: "docs/specs/export.md",
  slug: "export",
  idea: "Let people export their task list.",
  locale: "en",
  context_paths: [],
  state: "drafting",
  attempt: {
    id: ATTEMPT_ID,
    number: 1,
    trigger: "auto",
    reason: null,
    note: null,
    state: "running",
    base_sha: COMMIT,
    rounds: 0,
    max_rounds: 4,
    packet_sha256: "4b8e".padEnd(64, "0"),
    started_at: AT,
    ended_at: null,
  },
  last_round: null,
  session: null,
  budget: null,
  waiting_on: [],
  failure: null,
  primary_action: null,
  available_actions: ["cancel"],
  retries: 0,
  spec_id: null,
  commit: null,
  version: 4,
  created_at: AT,
  updated_at: AT,
  finished_at: null,
};

type Server = { answers: object[]; open: boolean };

async function mockApi(page: Page, server: Server) {
  await page.route("**/api/v1/**", async (route) => {
    const call = route.request();
    const url = new URL(call.url());
    const path = url.pathname;
    const json = (body: unknown, status = 200) =>
      route.fulfill({
        status,
        contentType:
          status >= 400 ? "application/problem+json" : "application/json",
        json: body,
      });
    const waiting = server.open ? [decision()] : [];
    if (path === "/api/v1/events") {
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: ": keep-alive\n\n",
      });
    } else if (path === "/api/v1/projects") {
      await json({ items: [project], next_cursor: null });
    } else if (path === `/api/v1/projects/${PROJECT_ID}`) {
      await json(project);
    } else if (path === "/api/v1/inbox") {
      await json({
        items: server.open ? [inboxItem] : [],
        next_cursor: null,
      });
    } else if (path === "/api/v1/inbox/summary") {
      await json({
        open: server.open ? 1 : 0,
        by_kind: {
          decision: server.open ? 1 : 0,
          approval: 0,
          permission: 0,
          blocker: 0,
          spec_unclear: 0,
        },
        running: 1,
        merged_today: 0,
      });
    } else if (path === "/api/v1/decision-requests") {
      await json({ items: [...waiting, closed], next_cursor: null });
    } else if (
      path === `/api/v1/decision-requests/${DECISION_ID}/answer` &&
      call.method() === "POST"
    ) {
      server.answers.push(call.postDataJSON() as object);
      server.open = false;
      await json(decision({ state: "answered" }));
    } else if (path === "/api/v1/work-items") {
      await json({ items: [task], next_cursor: null });
    } else if (path === `/api/v1/work-items/${TASK_ID}`) {
      await json(task);
    } else if (path === `/api/v1/specs/${SPEC_ID}`) {
      await json(spec);
    } else if (path.endsWith("/specs")) {
      await json({ items: [spec], next_cursor: null });
    } else if (path === `/api/v1/spec-drafts/${DRAFT_ID}`) {
      await json(draft);
    } else if (path === "/api/v1/sessions") {
      await json({ items: [] });
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
  }
}

async function form(page: Page, scope: Locator) {
  await expect(scope.getByRole("radio")).toHaveCount(2);
  await expect(scope.getByText("Recommended")).toHaveCount(1);
  await touchable(scope.locator("fieldset label"));
  await touchable(scope.getByRole("button"));
  await noOverflow(page);
  await accessible(page);
}

for (const width of [375, 1280]) {
  test(`a question is answered from the Inbox at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    const server: Server = { answers: [], open: true };
    await mockApi(page, server);
    await page.goto("/");
    const card = page.getByRole("article", { name: "tasks-done/s1" });
    await expect(card.getByText(QUESTION)).toBeVisible();
    await form(page, card);

    await card.getByLabel(/^Cloudflare R2/).check();
    await card.getByLabel("Note for the agent (optional)").fill("Cheaper");
    await card.getByRole("button", { name: "Send answer" }).click();
    await expect(page.getByText("Nothing is waiting for you")).toBeVisible();
    expect(server.answers).toEqual([{ option: 1, note: "Cheaper" }]);
    await accessible(page);
  });

  test(`the task page shows open and closed questions at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await mockApi(page, { answers: [], open: true });
    await page.goto(`/delivery/tasks/${TASK_ID}`);
    const section = page.getByRole("region", {
      name: "Questions from the agent",
    });
    await expect(section).toBeVisible();
    await expect(section.getByRole("article")).toHaveCount(2);
    await expect(section.getByText("Option 2: Cloudflare R2")).toBeVisible();
    await expect(section.getByText("Smaller files")).toBeVisible();
    await form(page, section.getByRole("article", { name: QUESTION }));
  });

  test(`the draft page shows the architect's question at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await mockApi(page, { answers: [], open: true });
    await page.goto(`/docs/${PROJECT_ID}/drafts/${DRAFT_ID}`);
    const section = page.getByRole("region", {
      name: "Questions from the agent",
    });
    await expect(section).toBeVisible();
    await form(page, section.getByRole("article", { name: QUESTION }));
  });
}

test("the question follows the chosen language", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 900 });
  await mockApi(page, { answers: [], open: true });
  await page.goto("/");
  const language = page.getByRole("combobox", { name: "Language" });
  await expect(page.getByRole("article")).toHaveCount(1);
  await language.selectOption("Bahasa Indonesia");
  await expect(page.locator("html")).toHaveAttribute("lang", "id");
  const card = page.getByRole("article", { name: "tasks-done/s1" });
  await expect(card.getByText("Disarankan")).toBeVisible();
  await expect(
    card.getByRole("button", { name: "Kirim jawaban" }),
  ).toBeVisible();
  await expect(card.getByText("Catatan untuk agent (opsional)")).toBeVisible();
  await accessible(page);
});
