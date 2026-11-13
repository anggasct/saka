import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fakeApi, page, pending } from "@/test/fake-api";
import type { ProjectItem } from "@/features/delivery/queries";
import { PROJECT_ID } from "@/test/fixtures";
import { twoRepos } from "@/test/settings-fixtures";
import {
  doneIntake,
  draftingIntake,
  exhaustedIntake,
  failedIntake,
  issueProject,
  waitingIntake,
} from "@/test/issue-fixtures";
import { renderWithProviders } from "@/test/render";

import { IssueGroup } from "./issue-group";

const INTAKES = `/api/v1/projects/${PROJECT_ID}/issue-intakes`;

function at<T>(items: T[], index: number): T {
  const found = items[index];
  if (found === undefined) {
    throw new Error(`no row at ${index}`);
  }
  return found;
}

function group(): HTMLElement {
  return screen.getByRole("region", { name: "From issues" });
}

describe("IssueGroup", () => {
  it("shows nothing for a project without an issue entry", () => {
    fakeApi({});
    renderWithProviders(
      <IssueGroup
        project={{
          ...issueProject,
          workflow: { ...issueProject.workflow, issue: null },
        }}
      />,
    );
    expect(screen.queryByRole("region")).toBeNull();
  });

  it("shows a busy skeleton while the intakes load", async () => {
    fakeApi({ [INTAKES]: pending });
    renderWithProviders(<IssueGroup project={issueProject} />);
    expect(
      await screen.findByRole("status", { name: "Loading issues" }),
    ).toHaveAttribute("aria-busy", "true");
  });

  it("says how an issue gets in when none has", async () => {
    fakeApi({ [INTAKES]: { body: page([]) } });
    renderWithProviders(<IssueGroup project={issueProject} />);
    expect(
      await screen.findByText(/adds the label saka:intake to an issue/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add issue" })).toBeVisible();
  });

  it("shows the error from its code and retries", async () => {
    let fail = true;
    fakeApi({
      [INTAKES]: () =>
        fail
          ? { status: 500, body: { code: "internal", params: {} } }
          : { body: page([waitingIntake]) },
    });
    renderWithProviders(<IssueGroup project={issueProject} />);
    const alert = await screen.findByRole("alert");
    expect(
      within(alert).getByText("Something went wrong on the server."),
    ).toBeInTheDocument();
    fail = false;
    await userEvent.click(
      within(alert).getByRole("button", { name: "Try again" }),
    );
    expect(await screen.findByText("Crash on start")).toBeInTheDocument();
  });

  it("lists each intake with its issue link, kind, lane, state, and spec or draft", async () => {
    fakeApi({
      [INTAKES]: {
        body: page([waitingIntake, draftingIntake, failedIntake, doneIntake]),
      },
    });
    renderWithProviders(<IssueGroup project={issueProject} />);
    await screen.findAllByRole("listitem");
    const rows = within(group()).getAllByRole("listitem");
    expect(rows).toHaveLength(4);
    const row = (index: number) => within(at(rows, index));
    const first = row(0);
    expect(first.getByRole("link", { name: "Issue #41" })).toHaveAttribute(
      "href",
      waitingIntake.issue.url,
    );
    expect(first.getByText("Waiting for approval")).toBeInTheDocument();
    expect(first.getByText("Bugfix")).toBeInTheDocument();
    expect(first.getByText("Quick bugfix spec")).toBeInTheDocument();
    expect(
      first.getByRole("link", {
        name: "Spec docs/specs/bugfix-crash-on-start-41.md",
      }),
    ).toBeInTheDocument();
    const second = row(1);
    expect(second.getByText("Architect draft")).toBeInTheDocument();
    expect(second.getByText("Draft: Drafting")).toBeInTheDocument();
    const third = row(2);
    expect(third.getByText("Failed: draft_failed")).toBeInTheDocument();
    expect(third.getByRole("button", { name: "Retry" })).toBeVisible();
    expect(third.getByRole("button", { name: "Dismiss" })).toBeVisible();
    const last = row(3);
    expect(last.queryByRole("button")).toBeNull();
  });

  it("retries a failed intake and refreshes the list", async () => {
    let retried = false;
    const { requests } = fakeApi({
      [INTAKES]: () => ({
        body: page([
          retried ? { ...failedIntake, state: "accepted" } : failedIntake,
        ]),
      }),
      [`/api/v1/issue-intakes/${failedIntake.id}/actions/retry`]: () => {
        retried = true;
        return { status: 202, body: { ...failedIntake, state: "accepted" } };
      },
    });
    renderWithProviders(<IssueGroup project={issueProject} />);
    await userEvent.click(await screen.findByRole("button", { name: "Retry" }));
    const dialog = await screen.findByRole("dialog");
    expect(
      within(dialog).getByText(/You can retry 2 more times/),
    ).toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Retry" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    const sent = requests.filter((r) => r.method === "POST");
    expect(sent).toHaveLength(1);
    expect(sent[0]?.headers.get("idempotency-key")).toBeTruthy();
    expect(await screen.findByText("Accepted")).toBeInTheDocument();
  });

  it("offers no retry once the three retries are used", async () => {
    fakeApi({ [INTAKES]: { body: page([exhaustedIntake]) } });
    renderWithProviders(<IssueGroup project={issueProject} />);
    await screen.findByText("Failed: draft_failed");
    expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
    expect(screen.getByRole("button", { name: "Dismiss" })).toBeVisible();
  });

  it("dismisses with an optional reason", async () => {
    const { requests } = fakeApi({
      [INTAKES]: { body: page([waitingIntake]) },
      [`/api/v1/issue-intakes/${waitingIntake.id}/actions/dismiss`]: {
        body: { ...waitingIntake, state: "dismissed" },
      },
    });
    renderWithProviders(<IssueGroup project={issueProject} />);
    await userEvent.click(
      await screen.findByRole("button", { name: "Dismiss" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(
      within(dialog).getByLabelText("Reason, posted on the issue (optional)"),
      "duplicate of #7",
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Dismiss" }),
    );
    await waitFor(() => {
      expect(requests.filter((r) => r.method === "POST")).toHaveLength(1);
    });
    expect(requests.find((r) => r.method === "POST")?.body).toEqual({
      reason: "duplicate of #7",
    });
  });

  it("adds an issue by its number and says why a refused one was refused", async () => {
    let answer: { status: number; body: unknown } = {
      status: 404,
      body: { code: "issue_not_found", params: {} },
    };
    const { requests } = fakeApi({
      [INTAKES]: (request) =>
        request.method === "POST" ? answer : { body: page([]) },
    });
    renderWithProviders(<IssueGroup project={issueProject} />);
    await userEvent.click(
      await screen.findByRole("button", { name: "Add issue" }),
    );
    const dialog = await screen.findByRole("dialog");
    const submit = within(dialog).getByRole("button", { name: "Add issue" });
    expect(submit).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText("Issue number"), "99");
    await userEvent.click(submit);
    expect(
      await within(dialog).findByText(
        "That issue does not exist in this repo.",
      ),
    ).toBeInTheDocument();
    answer = { status: 201, body: waitingIntake };
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    const posts = requests.filter((r) => r.method === "POST");
    expect(posts.map((r) => r.body)).toEqual([
      { number: 99, repo: null },
      { number: 99, repo: null },
    ]);
  });

  it("asks which repo when the project has several", async () => {
    const [first] = twoRepos.repos;
    const several = {
      ...issueProject,
      repos: [
        { ...first, name: "api" },
        { ...first, name: "site" },
      ],
    } as ProjectItem;
    const { requests } = fakeApi({
      [INTAKES]: (request) =>
        request.method === "POST"
          ? { status: 201, body: waitingIntake }
          : { body: page([]) },
    });
    renderWithProviders(<IssueGroup project={several} />);
    await userEvent.click(
      await screen.findByRole("button", { name: "Add issue" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Issue number"), "5");
    const submit = within(dialog).getByRole("button", { name: "Add issue" });
    expect(submit).toBeDisabled();
    await userEvent.selectOptions(
      within(dialog).getByLabelText("Repo"),
      "site",
    );
    await userEvent.click(submit);
    await waitFor(() => {
      expect(requests.filter((r) => r.method === "POST")).toHaveLength(1);
    });
    expect(requests.find((r) => r.method === "POST")?.body).toEqual({
      number: 5,
      repo: "site",
    });
  });
});
