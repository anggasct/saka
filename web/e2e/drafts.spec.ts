import { createHash } from "node:crypto";

import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { FULL_LOOP } from "./workflow";

const PROJECT_ID = "0193f0c4-1b2a-7c3d-9e8f-001122334455";
const DRAFT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344d1";
const FAILED_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344d2";
const ATTEMPT_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344a1";
const SESSION_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344e1";
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const AT = "2026-11-03T10:00:00.000Z";
const COMMIT = "9f1c".padEnd(40, "0");
const PATH = "docs/specs/avatar-upload-for-every-user-profile.md";

const SPEC = [
  "---",
  "saka: spec",
  "---",
  "# Avatar upload",
  "",
  "## Acceptance criteria",
  "",
  "- **AC-01**: A user can upload an avatar.",
  "",
  "## Done when",
  "",
  "- `make test`",
  "",
].join("\n");

function blobOf(content: string): string {
  const bytes = Buffer.from(content, "utf8");
  return createHash("sha1")
    .update(`blob ${bytes.length}\0`)
    .update(bytes)
    .digest("hex");
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

const attempt = {
  id: ATTEMPT_ID,
  number: 1,
  trigger: "auto",
  reason: null,
  note: null,
  state: "running",
  base_sha: COMMIT,
  rounds: 1,
  max_rounds: 4,
  packet_sha256: "4b8e".padEnd(64, "0"),
  started_at: AT,
  ended_at: null,
};

function draft(state: string, extra: object = {}) {
  return {
    id: DRAFT_ID,
    project_id: PROJECT_ID,
    path: PATH,
    slug: "avatar-upload-for-every-user-profile",
    idea: "Let users upload an avatar.",
    locale: "en",
    mode: "draft",
    target_kind: "new",
    context_paths: ["docs/architecture.md"],
    state,
    attempt,
    last_round: {
      attempt: 1,
      round: 1,
      outcome: "failed",
      bytes: 2140,
      content_hash: "sha256:" + "1a2b".padEnd(64, "0"),
      decisions_open: 1,
      parser_version: 4,
      issues: [
        {
          code: "step_ac_unknown",
          severity: "error",
          line: 22,
          params: { step: "S2", ac: "AC-09" },
        },
        {
          code: "decision_unanswered",
          severity: "error",
          line: 31,
          params: { id: "Q1" },
        },
      ],
      created_at: AT,
    },
    session: {
      id: SESSION_ID,
      state: "running",
      cli: "claude",
      started_at: AT,
      ended_at: null,
    },
    budget: {
      attempt_id: ATTEMPT_ID,
      warn_percent: 80,
      running_minutes: { used: 3.5, limit: 30 },
      turns: { used: 2, limit: 6 },
      tokens: { used: 41000, limit: 0 },
      cost_usd: { used: "0.4100", limit: "0" },
    },
    waiting_on: [],
    failure: null,
    primary_action: null,
    available_actions: ["cancel"],
    retries: 0,
    spec_id: null,
    commit: null,
    version: 6,
    created_at: AT,
    updated_at: AT,
    finished_at: null,
    ...extra,
  };
}

const failedDraft = {
  ...draft("failed"),
  id: FAILED_ID,
  path: "docs/specs/notifications.md",
  slug: "notifications",
  attempt: { ...attempt, state: "failed", rounds: 4, ended_at: AT },
  budget: null,
  failure: { code: "lint_rounds_exhausted", params: { rounds: 4 } },
  primary_action: "open_written",
  available_actions: ["retry", "cancel"],
};

const session = {
  id: SESSION_ID,
  project_id: PROJECT_ID,
  owner: {
    kind: "draft_attempt",
    spec_draft_id: DRAFT_ID,
    attempt_id: ATTEMPT_ID,
  },
  cli: "claude",
  cli_version: "2.1.316",
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

type Server = {
  state: string;
  created: unknown[];
  events: string[];
  posts?: { path: string; body: unknown }[];
};

const CHANGES = {
  files: [
    {
      path: "docs/notes/avatar-sizes.md",
      status: "added",
      diff: "--- /dev/null\n+++ b/docs/notes/avatar-sizes.md\n+# Avatar sizes\n",
      truncated: false,
    },
    {
      path: PATH,
      status: "added",
      diff: "--- /dev/null\n+++ b/" + PATH + "\n+# Avatar upload\n",
      truncated: false,
    },
  ],
  lint: { passed: true, findings: [] },
};

function conversation(state: string) {
  return draft(state, {
    mode: "session",
    available_actions:
      state === "awaiting_operator"
        ? ["message", "commit", "discard", "cancel"]
        : ["cancel"],
    last_round: {
      attempt: 1,
      round: 2,
      outcome: "passed",
      bytes: 2140,
      content_hash: "sha256:" + "1a2b".padEnd(64, "0"),
      decisions_open: 0,
      parser_version: 4,
      issues: [],
      created_at: AT,
    },
    session: {
      id: SESSION_ID,
      state: state === "awaiting_operator" ? "idle" : "running",
      cli: "claude",
      started_at: AT,
      ended_at: null,
    },
  });
}

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
      const deadline = Date.now() + 5_000;
      while (server.events.length === 0 && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      await route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: `retry: 100\n\n${server.events.splice(0).join("")}`,
      });
    } else if (path === "/api/v1/projects") {
      await json({ items: [project], next_cursor: null });
    } else if (path === `/api/v1/projects/${PROJECT_ID}`) {
      await json(project);
    } else if (path === `/api/v1/projects/${PROJECT_ID}/files`) {
      const files = [
        { path: "docs/architecture.md", blob: blobOf("# A\n"), bytes: 4 },
        { path: "docs/prd.md", blob: blobOf("# P\n"), bytes: 4 },
      ];
      if (server.state === "succeeded") {
        files.push({ path: PATH, blob: blobOf(SPEC), bytes: SPEC.length });
      }
      await json({
        commit: COMMIT,
        base_branch: "main",
        project_path: "docs",
        files,
        truncated: false,
      });
    } else if (path === `/api/v1/projects/${PROJECT_ID}/file`) {
      await json({
        path: url.searchParams.get("path"),
        commit: COMMIT,
        blob: blobOf(SPEC),
        bytes: SPEC.length,
        content: SPEC,
      });
    } else if (
      path === `/api/v1/projects/${PROJECT_ID}/spec-drafts` &&
      request.method() === "POST"
    ) {
      server.created.push(request.postDataJSON());
      server.state = "queued";
      await json(draft("queued", { attempt: null, last_round: null }), 201);
    } else if (path === `/api/v1/projects/${PROJECT_ID}/spec-drafts`) {
      await json({
        items: [{ ...draft("drafting"), idea: null }, failedDraft],
        next_cursor: null,
      });
    } else if (
      server.posts !== undefined &&
      request.method() === "POST" &&
      path.startsWith(`/api/v1/spec-drafts/${DRAFT_ID}/`)
    ) {
      server.posts.push({ path, body: request.postDataJSON() });
      server.state = path.endsWith("/message") ? "drafting" : "committing";
      await json(conversation(server.state), 202);
    } else if (
      server.posts !== undefined &&
      path === `/api/v1/spec-drafts/${DRAFT_ID}/changes`
    ) {
      await json(CHANGES);
    } else if (
      server.posts !== undefined &&
      path === `/api/v1/spec-drafts/${DRAFT_ID}`
    ) {
      await json(conversation(server.state));
    } else if (path === `/api/v1/spec-drafts/${DRAFT_ID}`) {
      if (server.state === "queued") {
        server.state = "drafting";
      }
      await json(
        server.state === "succeeded"
          ? draft("succeeded", {
              budget: null,
              available_actions: [],
              commit: "c0ffee".padEnd(40, "1"),
            })
          : draft(server.state),
      );
    } else if (path === `/api/v1/spec-drafts/${FAILED_ID}`) {
      await json(failedDraft);
    } else if (path === "/api/v1/sessions") {
      await json({ items: [session] });
    } else if (path === `/api/v1/sessions/${SESSION_ID}/timeline`) {
      await json({
        items: ["Reading the PRD", "Writing the spec"].map((summary, i) => ({
          offset: i,
          at: AT,
          kind: "message",
          summary,
          data: {},
        })),
        prev_before: null,
        next_after: null,
      });
    } else if (path === `/api/v1/projects/${PROJECT_ID}/specs`) {
      await json({ items: [], next_cursor: null });
    } else if (path === "/api/v1/work-items") {
      await json({ items: [], next_cursor: null });
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

async function openFiles(page: Page, width: number) {
  if (width < 1024) {
    await page.getByRole("button", { name: "Files", exact: true }).click();
  }
}

for (const width of [375, 1280]) {
  test(`operator drafts a spec with the architect at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    const server: Server = { state: "none", created: [], events: [] };
    await mockApi(page, server);

    await page.goto(`/docs/${PROJECT_ID}`);
    await openFiles(page, width);
    const tree = page.getByRole("navigation", { name: "Files" });
    await expect(tree.getByRole("link", { name: "Drafts (2)" })).toBeVisible();
    await touchable(tree.getByRole("button", { name: "Draft with architect" }));
    await tree.getByRole("button", { name: "Draft with architect" }).click();
    const dialog = page.getByRole("dialog", {
      name: "Draft a spec with the architect",
    });
    await expect(dialog).toBeVisible();
    await touchable(dialog.getByRole("button"));
    await noOverflow(page);
    await accessible(page);

    await dialog
      .getByRole("textbox", { name: "Idea" })
      .fill("Let users upload an avatar.");
    await dialog
      .getByRole("textbox", { name: "Path" })
      .fill("docs/specs/avatar-upload-for-every-user-profile");
    await dialog.getByRole("checkbox", { name: "docs/prd.md" }).check();
    await dialog.getByRole("button", { name: "Start drafting" }).click();

    await expect(page).toHaveURL(
      new RegExp(`/docs/${PROJECT_ID}/drafts/${DRAFT_ID}$`),
    );
    expect(server.created).toEqual([
      {
        idea: "Let users upload an avatar.",
        path: PATH,
        context_paths: ["docs/prd.md"],
        locale: "en",
        mode: "draft",
        target_kind: "new",
      },
    ]);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(PATH);
    const log = page.getByRole("log", { name: "Session timeline" });
    await expect(log.getByText("Writing the spec")).toBeVisible();
    await expect(page.getByText("Check 1 of 4")).toBeVisible();
    await noOverflow(page);
    await touchable(page.getByRole("button"));
    await accessible(page);

    server.state = "succeeded";
    server.events.push(
      `event: spec.draft.succeeded.v1\ndata: ${JSON.stringify({
        seq: 20,
        stream_type: "spec_draft",
      })}\n\n`,
    );
    await expect(page).toHaveURL(
      new RegExp(`/docs/${PROJECT_ID}\\?path=${encodeURIComponent(PATH)}$`),
    );
    await expect(
      page.getByRole("textbox", { name: `Editor for ${PATH}` }),
    ).toBeVisible();
  });

  test(`the draft list and a failed draft are accessible at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    await mockApi(page, { state: "drafting", created: [], events: [] });

    await page.goto(`/docs/${PROJECT_ID}/drafts`);
    const table = page.getByRole("table", { name: "Spec drafts" });
    await expect(table.getByRole("link", { name: PATH })).toBeVisible();
    await noOverflow(page);
    await touchable(page.getByRole("link", { name: "All", exact: true }));
    await touchable(table.getByRole("link"));
    await accessible(page);

    await table.getByRole("link", { name: PATH }).click();
    await expect(page).toHaveURL(
      new RegExp(`/docs/${PROJECT_ID}/drafts/${DRAFT_ID}$`),
    );

    await page.goto(`/docs/${PROJECT_ID}/drafts/${FAILED_ID}`);
    const card = page.getByRole("alert");
    await expect(
      card.getByText("The spec still had issues after 4 checks."),
    ).toBeVisible();
    await touchable(card.getByRole("link"));
    await touchable(card.getByRole("button"));
    await noOverflow(page);
    await accessible(page);

    await card.getByRole("button", { name: "Try again" }).click();
    const retry = page.getByRole("dialog", { name: "Try the draft again" });
    await expect(retry).toBeVisible();
    await touchable(retry.getByRole("button"));
    await accessible(page);
    await page.keyboard.press("Escape");
    await expect(retry).toBeHidden();
  });
}

for (const width of [375, 1280]) {
  test(`operator holds a conversation with the architect at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    const server: Server = {
      state: "awaiting_operator",
      created: [],
      events: [],
      posts: [],
    };
    await mockApi(page, server);

    await page.goto(`/docs/${PROJECT_ID}/drafts/${DRAFT_ID}`);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(PATH);
    await expect(page.getByText("Waiting for you").first()).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Changed files" }),
    ).toBeVisible();
    await expect(page.getByText("The spec passes the lint.")).toBeVisible();
    await page.getByText("docs/notes/avatar-sizes.md", { exact: true }).click();
    await expect(
      page.getByLabel("Changes to docs/notes/avatar-sizes.md"),
    ).toContainText("+# Avatar sizes");
    await noOverflow(page);
    await touchable(page.getByRole("button"));
    await touchable(page.getByRole("textbox"));
    await accessible(page);

    await page
      .getByRole("textbox", { name: "Your message" })
      .fill("Add the avatar sizes.");
    await page.getByRole("button", { name: "Send reply" }).click();
    await expect.poll(() => server.posts?.length).toBe(1);
    expect(server.posts?.[0]).toEqual({
      path: `/api/v1/spec-drafts/${DRAFT_ID}/message`,
      body: { text: "Add the avatar sizes." },
    });

    server.state = "awaiting_operator";
    server.events.push(
      `event: spec.draft.awaiting_operator.v1\ndata: ${JSON.stringify({
        seq: 30,
        stream_type: "spec_draft",
      })}\n\n`,
    );
    await page.getByRole("button", { name: "Commit" }).click();
    const dialog = page.getByRole("dialog", { name: "Commit the changes" });
    await expect(dialog).toBeVisible();
    await touchable(dialog.getByRole("button"));
    await accessible(page);
    await dialog
      .getByRole("textbox", { name: "Commit message (optional)" })
      .fill("docs: add the avatar spec");
    await dialog.getByRole("button", { name: "Commit" }).click();
    await expect.poll(() => server.posts?.length).toBe(2);
    expect(server.posts?.[1]).toEqual({
      path: `/api/v1/spec-drafts/${DRAFT_ID}/commit`,
      body: { message: "docs: add the avatar spec" },
    });
  });
}

test("the conversation follows the chosen language", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 900 });
  await mockApi(page, {
    state: "awaiting_operator",
    created: [],
    events: [],
    posts: [],
  });
  await page.goto(`/docs/${PROJECT_ID}/drafts/${DRAFT_ID}`);
  await expect(page.getByText("Waiting for you").first()).toBeVisible();
  await page
    .getByRole("combobox", { name: "Language" })
    .selectOption("Bahasa Indonesia");
  await expect(page.locator("html")).toHaveAttribute("lang", "id");
  await expect(
    page.getByRole("heading", { name: "Giliran Anda" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "File yang berubah" }),
  ).toBeVisible();
  await expect(page.getByText("Spec lolos lint.")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Kirim balasan" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "Buang" })).toBeVisible();
  await accessible(page);
});
