import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fakeApi, page } from "@/test/fake-api";
import { detail, project, PROJECT_ID, SPEC_ID, task } from "@/test/fixtures";
import { specWithSteps } from "@/test/fixtures";
import { renderWithProviders } from "@/test/render";

import type { WorkItemDetail } from "./queries";
import { TaskPage } from "./task-page";

const HEAD = "a1b2c3d4e5f6".padEnd(40, "0");
const OLD = "0f9e8d7c6b5a".padEnd(40, "0");

type Verdict = WorkItemDetail["reviews"][number];

function verdict(extra: Partial<Verdict>): Verdict {
  return {
    id: "0193f0c4-1b2a-7c3d-9e8f-001122339901",
    outcome: "approved",
    basis: "step",
    head_sha: HEAD,
    current: true,
    summary: "Looks right.",
    finding_count: 0,
    posted: null,
    created_at: "2026-10-31T10:06:00.000Z",
    findings: [],
    questions: [],
    ...extra,
  };
}

function render(reviews: Verdict[]) {
  const item = detail(task(1, { state: "in_stage" }), { reviews });
  fakeApi({
    [`/api/v1/work-items/${item.id}`]: { body: item },
    [`/api/v1/specs/${SPEC_ID}`]: { body: specWithSteps([["S1", []]]) },
    [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
    "/api/v1/work-items": { body: page([item]) },
  });
  renderWithProviders(<TaskPage workItemId={item.id} />);
}

describe("ReviewSection", () => {
  it("is absent before any review ran", async () => {
    render([]);
    await screen.findByRole("heading", { level: 1 });
    expect(screen.queryByRole("region", { name: "Review" })).toBeNull();
  });

  it("shows an approval with the link to the posted review", async () => {
    render([
      verdict({
        posted: {
          event: "approve",
          url: "https://github.com/acme/tasks/pull/12#pullrequestreview-1",
        },
      }),
    ]);
    const section = await screen.findByRole("region", { name: "Review" });
    expect(within(section).getByText("Approved")).toBeInTheDocument();
    expect(within(section).getByText("Looks right.")).toBeInTheDocument();
    expect(within(section).getByText("No findings.")).toBeInTheDocument();
    expect(
      within(section).getByText("Posted as an approval"),
    ).toBeInTheDocument();
    const link = within(section).getByRole("link", { name: "View the review" });
    expect(link).toHaveAttribute(
      "href",
      "https://github.com/acme/tasks/pull/12#pullrequestreview-1",
    );
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
  });

  it("lists the findings of a verdict that asks for changes and marks a missing spec", async () => {
    render([
      verdict({
        outcome: "changes_requested",
        basis: "none",
        summary: "Something needs to change.",
        finding_count: 1,
        findings: [
          {
            path: "src/lib.rs",
            line: 7,
            severity: "major",
            body: "The retry loop never backs off.",
          },
        ],
        posted: { event: "request_changes", url: null },
      }),
    ]);
    const section = await screen.findByRole("region", { name: "Review" });
    expect(within(section).getByText("Changes requested")).toBeInTheDocument();
    expect(within(section).getByText("No spec")).toBeInTheDocument();
    expect(within(section).getByText("Major")).toBeInTheDocument();
    expect(within(section).getByText("src/lib.rs:7")).toBeInTheDocument();
    expect(
      within(section).getByText("The retry loop never backs off."),
    ).toBeInTheDocument();
    expect(
      within(section).getByText("Posted as a request for changes"),
    ).toBeInTheDocument();
    expect(within(section).queryByRole("link")).toBeNull();
  });

  it("asks the spec's questions when the reviewer could not judge", async () => {
    render([
      verdict({
        outcome: "spec_unclear",
        summary: "The spec does not say.",
        questions: ["Which store should hold the flag?"],
      }),
    ]);
    const section = await screen.findByRole("region", { name: "Review" });
    expect(within(section).getByText("Spec unclear")).toBeInTheDocument();
    expect(
      within(section).getByText("Which store should hold the flag?"),
    ).toBeInTheDocument();
    expect(within(section).queryByText("Findings")).toBeNull();
  });

  it("keeps verdicts of older heads in a collapsed list", async () => {
    render([
      verdict({}),
      verdict({
        id: "0193f0c4-1b2a-7c3d-9e8f-001122339902",
        outcome: "changes_requested",
        head_sha: OLD,
        current: false,
        created_at: "2026-10-31T10:01:00.000Z",
      }),
    ]);
    const section = await screen.findByRole("region", { name: "Review" });
    const toggle = within(section).getByText("Earlier reviews (1)");
    await userEvent.click(toggle);
    expect(within(section).getByText("0f9e8d7c6b5a")).toBeVisible();
    expect(within(section).getAllByText("Changes requested")).not.toHaveLength(
      0,
    );
  });

  it("lists the reviewer's questions on the blocker of an unclear spec", async () => {
    const item = detail(
      task(1, {
        state: "blocked",
        blocker: {
          id: "0193f0c4-1b2a-7c3d-9e8f-001122334488",
          code: "spec_unclear",
          params: {
            head_sha: HEAD,
            questions: ["Which store?"],
            step_id: "S1",
          },
          opened_at: "2026-10-31T10:05:00.000Z",
          stage: "review",
          primary_action: { kind: "edit_spec", params: {} },
        },
      }),
    );
    fakeApi({
      [`/api/v1/work-items/${item.id}`]: { body: item },
      [`/api/v1/specs/${SPEC_ID}`]: { body: specWithSteps([["S1", []]]) },
      [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
      "/api/v1/work-items": { body: page([item]) },
    });
    renderWithProviders(<TaskPage workItemId={item.id} />);
    const blocker = await screen.findByRole("region", { name: "Blocker" });
    expect(within(blocker).getByText("Which store?")).toBeInTheDocument();
  });
});
