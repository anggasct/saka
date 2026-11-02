import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { startEventStream } from "@/api/events";
import {
  cancelledDraft,
  committingDraft,
  DRAFT,
  DRAFT_ID,
  DRAFT_PATH,
  DRAFT_SESSION_ID,
  draft,
  DRAFTS,
  draftSession,
  failedOpenFile,
  failedOpenWritten,
  failedRetry,
  preparingDraft,
  queuedDraft,
  succeededDraft,
  timeline,
} from "@/test/draft-fixtures";
import { fakeApi, page, type Recorded, type Routes } from "@/test/fake-api";
import { fakeStream } from "@/test/fake-stream";
import { project, PROJECT_ID } from "@/test/fixtures";
import type { SpecDraft } from "./queries";
import { renderApp } from "@/test/render";

const DOCS = `/docs/${PROJECT_ID}`;
const PAGE = `${DOCS}/drafts/${DRAFT_ID}`;

function routes(current: () => SpecDraft, extra: Routes = {}): Routes {
  return {
    [DRAFT]: () => ({ body: current() }),
    [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
    "/api/v1/sessions": { body: { items: [draftSession()] } },
    [`/api/v1/sessions/${DRAFT_SESSION_ID}/timeline`]: {
      body: timeline(["Reading the PRD", "Writing the spec"]),
    },
    ...extra,
  };
}

function show(item: SpecDraft, extra: Routes = {}) {
  const api = fakeApi(routes(() => item, extra));
  const rendered = renderApp(PAGE);
  return { api, ...rendered };
}

async function header() {
  return screen.findByRole("heading", { level: 1, name: DRAFT_PATH });
}

describe("Draft page", () => {
  it("says what a queued draft waits for", async () => {
    show(queuedDraft);
    await header();
    expect(screen.getByText("Queued")).toBeInTheDocument();
    expect(
      screen.getByText("Waiting for a free architect slot."),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        "Waiting for the agent account claude-main: its five_hour window is at 92%.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText("Not started")).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Architect session" }),
    ).toBeNull();
    expect(screen.getByRole("button", { name: "Cancel draft" })).toBeVisible();
  });

  it("shows a preparing draft", async () => {
    show(preparingDraft);
    await header();
    expect(screen.getByText("Preparing")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Preparing a workspace with the docs for the architect.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText("No check yet, up to 4")).toBeInTheDocument();
  });

  it("shows a drafting draft with its checks, budget, session, and controls", async () => {
    const { api } = show(draft());
    await header();
    expect(screen.getByText("Drafting")).toBeInTheDocument();
    expect(screen.getByText("Check 1 of 4")).toBeInTheDocument();
    expect(screen.getByText("Attempt 1 (first try)")).toBeInTheDocument();
    expect(screen.getByText("3.5 of 30")).toBeInTheDocument();

    const report = screen.getByRole("region", { name: "Latest check" });
    const lint = within(report);
    expect(lint.getByText("1 issue · 1 open decision")).toBeInTheDocument();
    const items = lint.getAllByRole("listitem");
    expect(items.map((li) => li.textContent)).toEqual([
      "Line 22Step S2 names AC-09, which is not in the acceptance criteria.",
      "Line 16Decision Q1 is not answered yet: check one option.",
    ]);

    const log = await screen.findByRole("log", { name: "Session timeline" });
    expect(await within(log).findByText("Writing the spec")).toBeVisible();
    expect(
      screen.getByRole("textbox", { name: "Steer the agent" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Interrupt" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Stop" })).toBeVisible();
    const sessions = api.requests.find((r) => r.path === "/api/v1/sessions");
    expect(sessions?.search.get("spec_draft_id")).toBe(DRAFT_ID);
  });

  it("picks among the sessions of the running attempt only", async () => {
    const resumed = draftSession({
      id: "0193f0c4-1b2a-7c3d-9e8f-0011223344e2",
      resumed_from_session_id: DRAFT_SESSION_ID,
    });
    const older = draftSession({
      id: "0193f0c4-1b2a-7c3d-9e8f-0011223344e3",
      owner: {
        kind: "draft_attempt",
        spec_draft_id: DRAFT_ID,
        attempt_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344a0",
      },
    });
    show(draft(), {
      "/api/v1/sessions": {
        body: { items: [older, draftSession(), resumed] },
      },
      [`/api/v1/sessions/${resumed.id}/timeline`]: {
        body: timeline(["Resumed"]),
      },
    });
    const picker = await screen.findByRole("combobox", { name: "Session" });
    expect(within(picker).getAllByRole("option")).toHaveLength(2);
    expect(picker).toHaveValue(resumed.id);
  });

  it("shows a committing draft", async () => {
    show(committingDraft);
    await header();
    expect(
      screen.getByText("The spec passed the checks and is being committed."),
    ).toBeInTheDocument();
    expect(screen.getByText("Passes · 1 open decision")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Cancel draft" })).toBeNull();
  });

  it("links a succeeded draft to the editor", async () => {
    show(succeededDraft);
    await header();
    expect(screen.getByText("Succeeded")).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Open in editor" }),
    ).toHaveAttribute("href", `${DOCS}?path=${encodeURIComponent(DRAFT_PATH)}`);
  });

  it("opens the editor when the draft succeeds while the page is open", async () => {
    let current: SpecDraft = committingDraft;
    fakeApi(
      routes(() => current, {
        [`/api/v1/projects/${PROJECT_ID}/files`]: { body: { files: [] } },
      }),
    );
    const fake = fakeStream();
    const { router, queryClient } = renderApp(PAGE);
    startEventStream(queryClient, () => fake.stream);
    await header();
    current = succeededDraft;
    fake.emit("spec.draft.succeeded.v1", {
      seq: 12,
      stream_type: "spec_draft",
    });
    await waitFor(() => {
      expect(router.state.location.pathname).toBe(DOCS);
    });
    expect(router.state.location.search).toEqual({ path: DRAFT_PATH });
    expect(router.history.length).toBe(1);
  });

  it("offers Try again first when the failure is retried", async () => {
    const { api } = show(failedRetry, {
      [`${DRAFT}/actions/retry`]: { status: 202, body: queuedDraft },
    });
    await header();
    const card = screen.getByRole("alert");
    expect(
      within(card).getByText("The architect session was lost."),
    ).toBeVisible();
    const buttons = within(card).getAllByRole("button");
    expect(buttons.map((b) => b.textContent)).toEqual([
      "Try again",
      "Cancel draft",
    ]);
    const user = userEvent.setup();
    await user.click(within(card).getByRole("button", { name: "Try again" }));
    const dialog = await screen.findByRole("dialog", {
      name: "Try the draft again",
    });
    const submit = within(dialog).getByRole("button", { name: "Try again" });
    expect(submit).toBeDisabled();
    await user.type(
      within(dialog).getByRole("textbox", { name: "Reason" }),
      "runner restarted",
    );
    await user.type(
      within(dialog).getByRole("textbox", {
        name: "Note for the architect (optional)",
      }),
      "Keep the decisions short.",
    );
    await user.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    const sent = api.requests.find(
      (r: Recorded) => r.path === `${DRAFT}/actions/retry`,
    );
    expect(sent?.body).toEqual({
      reason: "runner restarted",
      note: "Keep the decisions short.",
    });
    expect(sent?.headers.get("Idempotency-Key")).not.toBeNull();
  });

  it("explains the retry limit", async () => {
    show(failedRetry, {
      [`${DRAFT}/actions/retry`]: {
        status: 409,
        body: {
          code: "retry_limit_reached",
          params: { limit: 3 },
          status: 409,
        },
      },
    });
    await header();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Try again" }));
    const dialog = await screen.findByRole("dialog");
    await user.type(
      within(dialog).getByRole("textbox", { name: "Reason" }),
      "again",
    );
    await user.click(within(dialog).getByRole("button", { name: "Try again" }));
    expect(
      await within(dialog).findByText("This draft already used its 3 retries."),
    ).toBeVisible();
  });

  it("opens what was written when the checks never passed", async () => {
    show(failedOpenWritten);
    await header();
    const card = screen.getByRole("alert");
    expect(
      within(card).getByText("The spec still had issues after 4 checks."),
    ).toBeVisible();
    const primary = within(card).getByRole("link", {
      name: "Open what was written",
    });
    expect(primary).toHaveAttribute(
      "href",
      `${DOCS}?path=${encodeURIComponent(DRAFT_PATH)}&draft=${DRAFT_ID}`,
    );
    expect(
      within(card)
        .getAllByRole("button")
        .map((b) => b.textContent),
    ).toEqual(["Try again", "Cancel draft"]);
  });

  it("opens the committed file when registering failed", async () => {
    show(failedOpenFile);
    await header();
    const card = screen.getByRole("alert");
    expect(
      within(card).getByText(
        "The spec was committed but could not be registered (spec_slug_conflict).",
      ),
    ).toBeVisible();
    expect(
      within(card).getByRole("link", { name: "Open file" }),
    ).toHaveAttribute("href", `${DOCS}?path=${encodeURIComponent(DRAFT_PATH)}`);
    expect(
      within(card)
        .getAllByRole("button")
        .map((b) => b.textContent),
    ).toEqual(["Cancel draft"]);
  });

  it("cancels a draft with a reason", async () => {
    const { api } = show(draft(), {
      [`${DRAFT}/actions/cancel`]: { status: 202, body: cancelledDraft },
    });
    await header();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Cancel draft" }));
    const dialog = await screen.findByRole("dialog", {
      name: "Cancel the draft",
    });
    await user.type(
      within(dialog).getByRole("textbox", { name: "Reason" }),
      "wrong idea",
    );
    await user.click(
      within(dialog).getByRole("button", { name: "Cancel draft" }),
    );
    await waitFor(() => {
      expect(
        api.requests.find((r) => r.path === `${DRAFT}/actions/cancel`)?.body,
      ).toEqual({ reason: "wrong idea" });
    });
  });

  it("shows a cancelled draft", async () => {
    show(cancelledDraft);
    await header();
    expect(screen.getByText("Cancelled")).toBeInTheDocument();
    expect(
      screen.getByText("This draft was cancelled. Nothing was committed."),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Cancel draft" })).toBeNull();
  });

  it("says when the draft does not exist", async () => {
    fakeApi({});
    renderApp(PAGE);
    expect(
      await screen.findByRole("heading", { name: "Draft not found" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "All drafts" })).toHaveAttribute(
      "href",
      `${DOCS}/drafts`,
    );
  });

  it("offers a retry when loading fails", async () => {
    fakeApi({
      [DRAFT]: {
        status: 500,
        body: { code: "internal", params: {}, status: 500 },
      },
    });
    renderApp(PAGE);
    expect(
      await screen.findByRole("heading", {
        name: "The draft could not be loaded",
      }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Try again" })).toBeVisible();
  });
});

describe("Draft list", () => {
  it("lists active drafts by default and all drafts on request", async () => {
    const api = fakeApi({
      [DRAFTS]: (request: Recorded) => ({
        body: page(
          request.search.get("state") === "all"
            ? [draft(), succeededDraft]
            : [draft()],
        ),
      }),
      [`/api/v1/projects/${PROJECT_ID}/files`]: {
        body: {
          commit: "9f1c".padEnd(40, "0"),
          base_branch: "main",
          project_path: "docs",
          files: [],
          truncated: false,
        },
      },
    });
    const user = userEvent.setup();
    renderApp(`${DOCS}/drafts`);
    const table = await screen.findByRole("table", { name: "Spec drafts" });
    expect(within(table).getAllByRole("row")).toHaveLength(2);
    expect(
      within(table).getByRole("link", { name: DRAFT_PATH }),
    ).toHaveAttribute("href", PAGE);
    expect(within(table).getByText("Drafting")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Active" })).toHaveAttribute(
      "aria-current",
      "page",
    );
    expect(
      api.requests.find((r) => r.path === DRAFTS)?.search.get("state"),
    ).toBe("active");

    await user.click(screen.getByRole("link", { name: "All" }));
    await waitFor(() => {
      expect(within(table).getAllByRole("row")).toHaveLength(3);
    });
    expect(within(table).getByText("Succeeded")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "All" })).toHaveAttribute(
      "aria-current",
      "page",
    );
  });

  it("invites a first draft when there is none", async () => {
    fakeApi({
      [DRAFTS]: { body: page([]) },
      [`/api/v1/projects/${PROJECT_ID}/files`]: {
        body: {
          commit: "9f1c".padEnd(40, "0"),
          base_branch: "main",
          project_path: "docs",
          files: [],
          truncated: false,
        },
      },
    });
    renderApp(`${DOCS}/drafts?state=all`);
    expect(
      await screen.findByRole("heading", { name: "No drafts yet" }),
    ).toBeInTheDocument();
    expect(
      await screen.findAllByRole("button", { name: "Draft with architect" }),
    ).toHaveLength(1);
  });

  it("offers a retry when the list fails", async () => {
    fakeApi({
      [DRAFTS]: {
        status: 500,
        body: { code: "internal", params: {}, status: 500 },
      },
    });
    renderApp(`${DOCS}/drafts`);
    expect(
      await screen.findByRole("heading", {
        name: "The drafts could not be loaded",
      }),
    ).toBeInTheDocument();
  });
});
