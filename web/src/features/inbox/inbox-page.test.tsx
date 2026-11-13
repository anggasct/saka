import { QueryClient } from "@tanstack/react-query";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { startEventStream } from "@/api/events";
import { fakeApi, page } from "@/test/fake-api";
import type { Recorded, Routes } from "@/test/fake-api";
import { fakeStream } from "@/test/fake-stream";
import {
  approvalItem,
  BLOCKER_ID,
  DECISION_ID,
  decisionItem,
  deploymentFixItem,
  deploymentItem,
  exhaustedIntakeItem,
  FAILED_DEPLOYMENT_ID,
  INTAKE_ID,
  intakeItem,
  issueApprovalItem,
  mergeItem,
  permissionItem,
  REQUEST_ID,
  REVISION_ID,
  retryItem,
  secretItem,
  startItem,
  summary,
  TASK_ID,
  unclearItem,
  unverifiedFixItem,
  unverifiedRevertItem,
} from "@/test/inbox-fixtures";
import { PROJECT_ID, SPEC_ID } from "@/test/fixtures";
import { renderApp, renderWithProviders } from "@/test/render";

import { InboxPage } from "./inbox-page";
import type { InboxItem } from "./queries";

const INBOX = "/api/v1/inbox";
const SUMMARY = "/api/v1/inbox/summary";

function routes(items: () => InboxItem[], extra: Routes = {}): Routes {
  return {
    [INBOX]: () => ({ body: page(items()) }),
    [SUMMARY]: () => ({ body: summary(items().length) }),
    ...extra,
  };
}

function nth(items: HTMLElement[], index: number): HTMLElement {
  const found = items[index];
  if (found === undefined) {
    throw new Error(`no card at ${index}`);
  }
  return found;
}

function posts(requests: Recorded[]): Recorded[] {
  return requests.filter((r) => r.method === "POST");
}

async function card(name: string) {
  return screen.findByRole("article", { name });
}

describe("inbox page", () => {
  it("is the home page and shows one card per item with one main action each", async () => {
    fakeApi(
      routes(() => [
        permissionItem,
        retryItem,
        secretItem,
        unclearItem,
        approvalItem,
        startItem,
        mergeItem,
      ]),
    );
    renderApp("/");
    expect(
      await screen.findByRole("heading", { name: "Inbox", level: 1 }),
    ).toBeInTheDocument();
    expect(await screen.findByText("7 items need you")).toBeInTheDocument();
    const cards = await screen.findAllByRole("article");
    expect(cards).toHaveLength(7);

    const main = (c: HTMLElement) =>
      within(c)
        .queryAllByRole("button")
        .concat(within(c).queryAllByRole("link"))
        .map((e) => e.textContent);

    const permission = nth(cards, 0);
    const retry = nth(cards, 1);
    const secret = nth(cards, 2);
    const unclear = nth(cards, 3);
    const approval = nth(cards, 4);
    const start = nth(cards, 5);
    const merge = nth(cards, 6);
    expect(main(permission)).toEqual(["Allow", "Deny", "Open task"]);
    expect(main(retry)).toEqual(["Retry", "Open task"]);
    expect(main(secret)).toEqual(["Set the secrets", "Open task"]);
    expect(main(unclear)).toEqual(["Edit the spec", "Open task"]);
    expect(main(approval)).toEqual(["Approve", "Dismiss", "Open spec"]);
    expect(main(start)).toEqual(["Start", "Open task"]);
    expect(main(merge)).toEqual(["Merge", "Open task"]);

    expect(
      within(secret).getByRole("link", {
        name: "Set the secrets",
      }),
    ).toHaveAttribute("href", `/settings/${PROJECT_ID}`);
    expect(
      within(unclear).getByRole("link", {
        name: "Edit the spec",
      }),
    ).toHaveAttribute(
      "href",
      `/docs/${PROJECT_ID}?path=docs%2Fspecs%2Ftasks-done.md`,
    );
    expect(
      within(permission).getByText("npm install left-pad"),
    ).toBeInTheDocument();
    expect(within(retry).getByText(/CI/)).toBeInTheDocument();
  });

  it("answers a permission from the card and drops it once the server does", async () => {
    let waiting = [permissionItem];
    const { requests } = fakeApi(
      routes(() => waiting, {
        [`/api/v1/permission-requests/${REQUEST_ID}/answer`]: () => {
          waiting = [];
          return { body: {} };
        },
      }),
    );
    renderApp("/");
    await userEvent.click(
      within(await card("tasks-done/s1")).getByRole("button", {
        name: "Allow",
      }),
    );
    await waitFor(() => {
      expect(posts(requests).map((r) => r.body)).toEqual([
        { decision: "allow", message: null, remember: false },
      ]);
    });
    expect(
      await screen.findByText("Nothing is waiting for you"),
    ).toBeInTheDocument();
  });

  it("shows a question with its options and answers it from the card", async () => {
    let waiting = [decisionItem];
    const { requests } = fakeApi(
      routes(() => waiting, {
        [`/api/v1/decision-requests/${DECISION_ID}/answer`]: () => {
          waiting = [];
          return { body: {} };
        },
      }),
    );
    renderApp("/");
    const found = await card("tasks-done/s1");
    expect(
      within(found).getByText("Where do we store audio?"),
    ).toBeInTheDocument();
    expect(
      within(found).getByText("The recorder needs a place to keep files."),
    ).toBeInTheDocument();
    expect(within(found).getAllByRole("radio")).toHaveLength(2);
    expect(within(found).getAllByText("Recommended")).toHaveLength(1);
    await userEvent.click(within(found).getByRole("radio", { name: /^R2/ }));
    await userEvent.click(
      within(found).getByRole("button", { name: "Send answer" }),
    );
    await waitFor(() => {
      expect(posts(requests).map((r) => r.body)).toEqual([
        { option: 1, note: null },
      ]);
    });
    expect(
      await screen.findByText("Nothing is waiting for you"),
    ).toBeInTheDocument();
  });

  it("denies a permission with a message", async () => {
    const { requests } = fakeApi(
      routes(() => [permissionItem], {
        [`/api/v1/permission-requests/${REQUEST_ID}/answer`]: { body: {} },
      }),
    );
    renderApp("/");
    await userEvent.click(
      within(await card("tasks-done/s1")).getByRole("button", { name: "Deny" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(within(dialog).getByRole("textbox"), "not this");
    await userEvent.click(
      within(dialog).getByRole("button", { name: /^Deny/ }),
    );
    await waitFor(() => {
      expect(posts(requests).map((r) => r.body)).toEqual([
        { decision: "deny", message: "not this", remember: false },
      ]);
    });
  });

  it("runs the blocker's primary action against the blocker it was shown for", async () => {
    const { requests } = fakeApi(
      routes(() => [retryItem], {
        [`/api/v1/work-items/${TASK_ID}/actions/primary`]: { body: {} },
      }),
    );
    renderApp("/");
    await userEvent.click(
      within(await card("tasks-done/s1")).getByRole("button", {
        name: "Retry",
      }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(within(dialog).getByRole("textbox"), "runner is back");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Retry the stage" }),
    );
    await waitFor(() => {
      expect(posts(requests).map((r) => r.body)).toEqual([
        { blocker_id: BLOCKER_ID, reason: "runner is back" },
      ]);
    });
  });

  it("shows a failed deployment that holds no task, and redeploys its environment", async () => {
    const { requests } = fakeApi(
      routes(() => [deploymentItem], {
        [`/api/v1/projects/${PROJECT_ID}/environments/staging/actions/deploy`]:
          { status: 202, body: {} },
      }),
    );
    renderApp("/");
    const item = await card("staging");
    expect(within(item).getByText("Deployment")).toBeInTheDocument();
    expect(
      within(item).getByText(
        "The deployment to staging failed at the deploy step: The deploy webhook could not be reached. No task waits for it.",
      ),
    ).toBeInTheDocument();
    expect(
      within(item).getByRole("link", { name: "Open deployment" }),
    ).toHaveAttribute(
      "href",
      `/deployments/${PROJECT_ID}?deployment=${FAILED_DEPLOYMENT_ID}`,
    );
    await userEvent.click(
      within(item).getByRole("button", { name: "Redeploy" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(
      within(dialog).getByRole("textbox"),
      "webhook is back",
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Redeploy" }),
    );
    await waitFor(() => {
      expect(posts(requests)).toHaveLength(1);
    });
    const [sent] = posts(requests);
    expect(sent?.body).toEqual({ reason: "webhook is back" });
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("drafts a fix of a failed deployment, with its failure in the sentence", async () => {
    const { requests } = fakeApi(
      routes(() => [deploymentFixItem], {
        [`/api/v1/deployments/${FAILED_DEPLOYMENT_ID}/actions/draft-fix`]: {
          status: 202,
          body: {},
        },
      }),
    );
    renderApp("/");
    const item = await card("production");
    expect(
      within(item).getByText(
        "The deployment to production failed at the verification: A verification command failed. No task waits for it.",
      ),
    ).toBeInTheDocument();
    await userEvent.click(
      within(item).getByRole("button", { name: "Draft a fix" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(within(dialog).getByRole("textbox"), "see the logs");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Draft the fix" }),
    );
    await waitFor(() => {
      expect(posts(requests)).toHaveLength(1);
    });
    expect(posts(requests)[0]?.body).toEqual({ note: "see the logs" });
  });

  it("drafts a fix from the unverified-deployment blocker of a task", async () => {
    const { requests } = fakeApi(
      routes(() => [unverifiedFixItem], {
        [`/api/v1/deployments/${FAILED_DEPLOYMENT_ID}/actions/draft-fix`]: {
          status: 202,
          body: {},
        },
      }),
    );
    renderApp("/");
    const item = await card("tasks-done/s1");
    expect(
      within(item).getByText(
        /The deployment to staging failed at the verification/,
      ),
    ).toBeInTheDocument();
    await userEvent.click(
      within(item).getByRole("button", { name: "Draft a fix" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Draft the fix" }),
    );
    await waitFor(() => {
      expect(posts(requests)).toHaveLength(1);
    });
    expect(posts(requests)[0]?.body).toEqual({ note: null });
  });

  it("reverts from the unverified-deployment blocker of a task", async () => {
    const { requests } = fakeApi(
      routes(() => [unverifiedRevertItem], {
        [`/api/v1/work-items/${TASK_ID}/actions/primary`]: { body: {} },
      }),
    );
    renderApp("/");
    const item = await card("tasks-done/s1");
    await userEvent.click(within(item).getByRole("button", { name: "Revert" }));
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(within(dialog).getByRole("textbox"), "staging is red");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Revert the task" }),
    );
    await waitFor(() => {
      expect(posts(requests).map((r) => r.body)).toEqual([
        { blocker_id: BLOCKER_ID, reason: "staging is red" },
      ]);
    });
  });

  it("approves the revision it showed, and starts and merges at the shown version", async () => {
    const { requests } = fakeApi(
      routes(() => [approvalItem, startItem, mergeItem], {
        [`/api/v1/specs/${SPEC_ID}/approve`]: { body: {} },
        [`/api/v1/work-items/${TASK_ID}/actions/start`]: { body: {} },
        [`/api/v1/work-items/${TASK_ID}/actions/merge`]: { body: {} },
      }),
    );
    renderApp("/");
    const cards = await screen.findAllByRole("article");
    for (const [target, trigger, submit] of [
      [0, "Approve", "Approve spec"],
      [1, "Start", "Start step"],
      [2, "Merge", "Merge the pull request"],
    ] as const) {
      await userEvent.click(
        within(nth(cards, target)).getByRole("button", { name: trigger }),
      );
      const dialog = await screen.findByRole("dialog");
      await userEvent.click(
        within(dialog).getByRole("button", { name: submit }),
      );
      await waitFor(() => {
        expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      });
    }
    expect(
      posts(requests).map((r) => [r.path, r.headers.get("if-match")]),
    ).toEqual([
      [`/api/v1/specs/${SPEC_ID}/approve`, `"${REVISION_ID}"`],
      [`/api/v1/work-items/${TASK_ID}/actions/start`, 'W/"3"'],
      [`/api/v1/work-items/${TASK_ID}/actions/merge`, 'W/"9"'],
    ]);
  });

  it("dismisses a spec approval with a reason", async () => {
    const { requests } = fakeApi(
      routes(() => [approvalItem], {
        [`${INBOX}/${approvalItem.id}/dismiss`]: { body: {} },
      }),
    );
    renderApp("/");
    await userEvent.click(
      within(await card("Mark tasks as done")).getByRole("button", {
        name: "Dismiss",
      }),
    );
    const dialog = await screen.findByRole("dialog");
    const submit = within(dialog).getByRole("button", { name: "Dismiss" });
    expect(submit).toBeDisabled();
    await userEvent.type(within(dialog).getByRole("textbox"), "later");
    await userEvent.click(submit);
    await waitFor(() => {
      expect(posts(requests).map((r) => r.body)).toEqual([{ reason: "later" }]);
    });
  });

  it("offers no dismiss on anything but a spec approval", async () => {
    fakeApi(routes(() => [startItem, mergeItem, permissionItem, retryItem]));
    renderApp("/");
    await screen.findAllByRole("article");
    expect(
      screen.queryByRole("button", { name: "Dismiss" }),
    ).not.toBeInTheDocument();
  });

  it("says nothing waits and shows what is running and merged today", async () => {
    fakeApi({
      [INBOX]: { body: page([]) },
      [SUMMARY]: { body: summary(0, { running: 3, merged_today: 2 }) },
    });
    renderApp("/");
    expect(
      await screen.findByText("Nothing is waiting for you"),
    ).toBeInTheDocument();
    const running = await screen.findByText("Running now");
    expect(running.nextElementSibling).toHaveTextContent("3");
    expect(
      screen.getByText("Merged today").nextElementSibling,
    ).toHaveTextContent("2");
  });

  it("shows an error with a retry when the list cannot load", async () => {
    let failing = true;
    fakeApi({
      [INBOX]: () =>
        failing
          ? { status: 500, body: { code: "internal", params: {} } }
          : { body: page([retryItem]) },
      [SUMMARY]: { body: summary(1) },
    });
    renderApp("/");
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText("Could not load the inbox")).toBeVisible();
    failing = false;
    await userEvent.click(
      within(alert).getByRole("button", { name: "Try again" }),
    );
    expect(await card("tasks-done/s1")).toBeInTheDocument();
  });

  it("speaks Indonesian", async () => {
    fakeApi(routes(() => [approvalItem]));
    renderWithProviders(<InboxPage />, "id");
    expect(
      await screen.findByRole("heading", { name: "Kotak masuk", level: 1 }),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("button", { name: "Setujui" }),
    ).toBeVisible();
  });
});

describe("inbox deep link", () => {
  it("opens the inbox with the linked item focused", async () => {
    fakeApi(routes(() => [retryItem, permissionItem]));
    renderApp(`/inbox/${permissionItem.id}`);
    const linked = await screen.findAllByRole("article");
    await waitFor(() => {
      expect(linked[1]).toHaveFocus();
    });
    expect(linked[0]).not.toHaveFocus();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("says so when the linked item no longer waits", async () => {
    fakeApi(routes(() => [retryItem]));
    renderApp("/inbox/0193f0c4-1b2a-7c3d-9e8f-0011223399ff");
    expect(
      await screen.findByText("That item is no longer waiting for you."),
    ).toBeInTheDocument();
    expect(await screen.findAllByRole("article")).toHaveLength(1);
  });
});

describe("inbox badge", () => {
  it("shows the number waiting and follows the event stream without a reload", async () => {
    let waiting = 2;
    fakeApi({
      [INBOX]: () => ({
        body: page(Array.from({ length: waiting }, () => permissionItem)),
      }),
      [SUMMARY]: () => ({ body: summary(waiting) }),
    });
    const fake = fakeStream();
    const { queryClient } = renderApp("/");
    const stop = startEventStream(queryClient, () => fake.stream);
    const nav = await screen.findByRole("navigation");
    const link = within(nav).getByRole("link", { name: /^Inbox/ });
    await waitFor(() => {
      expect(link).toHaveTextContent("Inbox2");
    });
    expect(
      within(link).getByText("2 items waiting for you"),
    ).toBeInTheDocument();

    waiting = 3;
    fake.emit("runtime.permission.requested.v1", {
      seq: 5,
      stream_type: "permission_request",
    });
    await waitFor(() => {
      expect(link).toHaveTextContent("Inbox3");
    });

    waiting = 0;
    fake.emit("delivery.inbox_item.dismissed.v1", {
      seq: 6,
      stream_type: "inbox_item",
    });
    await waitFor(() => {
      expect(link).toHaveTextContent(/^Inbox$/);
    });
    stop();
    expect(queryClient).toBeInstanceOf(QueryClient);
  });

  it("links the issue a spec came from on its approval card", async () => {
    fakeApi(routes(() => [issueApprovalItem]));
    renderApp("/");
    const approval = await card("Mark tasks as done");
    expect(
      within(approval).getByRole("link", { name: "From issue #41" }),
    ).toHaveAttribute("href", "https://github.com/acme/tasks/issues/41");
  });

  it("offers retry and dismiss on a failed intake and sends them to the intake", async () => {
    const { requests } = fakeApi(
      routes(() => [intakeItem], {
        [`/api/v1/issue-intakes/${INTAKE_ID}/actions/retry`]: {
          status: 202,
          body: {},
        },
        [`/api/v1/issue-intakes/${INTAKE_ID}/actions/dismiss`]: { body: {} },
      }),
    );
    renderApp("/");
    const failed = await card("Issue #41: Crash on start");
    expect(
      within(failed).getByText(
        /could not turn this issue into a spec \(draft_failed\)/,
      ),
    ).toBeInTheDocument();
    expect(within(failed).getByText("Issue")).toBeInTheDocument();
    await userEvent.click(
      within(failed).getByRole("button", { name: "Retry" }),
    );
    let dialog = await screen.findByRole("dialog");
    expect(
      within(dialog).getByText(/You can retry 2 more times/),
    ).toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Retry" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    await userEvent.click(
      within(failed).getByRole("button", { name: "Dismiss" }),
    );
    dialog = await screen.findByRole("dialog");
    await userEvent.type(within(dialog).getByRole("textbox"), "not ours");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Dismiss" }),
    );
    await waitFor(() => {
      expect(posts(requests).map((r) => [r.path, r.body])).toEqual([
        [`/api/v1/issue-intakes/${INTAKE_ID}/actions/retry`, {}],
        [
          `/api/v1/issue-intakes/${INTAKE_ID}/actions/dismiss`,
          { reason: "not ours" },
        ],
      ]);
    });
  });

  it("offers only dismiss on an intake that has no retries left", async () => {
    fakeApi(routes(() => [exhaustedIntakeItem]));
    renderApp("/");
    const failed = await card("Issue #41: Crash on start");
    expect(
      within(failed).queryByRole("button", { name: "Retry" }),
    ).not.toBeInTheDocument();
    expect(
      within(failed).getAllByRole("button", { name: "Dismiss" }),
    ).toHaveLength(1);
  });
});
