import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { fakeApi, page } from "@/test/fake-api";
import { detail, project, PROJECT_ID, SPEC_ID, task } from "@/test/fixtures";
import { specWithSteps } from "@/test/fixtures";
import { renderWithProviders } from "@/test/render";

import type { WorkItemDetail } from "./queries";
import { TaskPage } from "./task-page";

type Fix = NonNullable<WorkItemDetail["fix"]>;

function render(fix: Fix | null, runs: WorkItemDetail["stage_runs"] = []) {
  const item = detail(task(1, { state: "in_stage", fix }), {
    stage_runs: runs,
  });
  fakeApi({
    [`/api/v1/work-items/${item.id}`]: { body: item },
    [`/api/v1/specs/${SPEC_ID}`]: { body: specWithSteps([["S1", []]]) },
    [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
    "/api/v1/work-items": { body: page([item]) },
  });
  renderWithProviders(<TaskPage workItemId={item.id} />);
}

describe("FixSection", () => {
  it("is absent before a fix round ran", async () => {
    render(null);
    await screen.findByRole("heading", { level: 1 });
    expect(screen.queryByRole("region", { name: "Fix" })).toBeNull();
  });

  it("shows the round, its limit, and what it answers", async () => {
    render({ round: 1, max_rounds: 2, cause: "review" });
    const section = await screen.findByRole("region", { name: "Fix" });
    expect(within(section).getByText("Round 1 of 2")).toBeInTheDocument();
    expect(
      within(section).getByText("Answering the findings of the reviewer."),
    ).toBeInTheDocument();
    expect(within(section).queryByText(/beyond the limit/)).toBeNull();
  });

  it("says when the operator granted rounds past the limit and what a conflict round resolves", async () => {
    render({ round: 3, max_rounds: 2, cause: "conflict" });
    const section = await screen.findByRole("region", { name: "Fix" });
    expect(within(section).getByText("Round 3 of 2")).toBeInTheDocument();
    expect(
      within(section).getByText("Resolving a conflict with the base branch."),
    ).toBeInTheDocument();
    expect(
      within(section).getByText(
        "The operator granted rounds beyond the limit.",
      ),
    ).toBeInTheDocument();
  });

  it("marks the stage runs that belong to a round", async () => {
    render({ round: 1, max_rounds: 2, cause: "review" }, [
      {
        id: "0193f0c4-1b2a-7c3d-9e8f-001122334471",
        attempt: 1,
        stage: "fix",
        state: "succeeded",
        trigger: "auto",
        round: 1,
      },
      {
        id: "0193f0c4-1b2a-7c3d-9e8f-001122334472",
        attempt: 1,
        stage: "review",
        state: "succeeded",
        trigger: "auto",
        round: null,
      },
    ]);
    const runs = await screen.findByRole("region", { name: "Stage runs" });
    expect(within(runs).getAllByText("Round 1")).toHaveLength(1);
  });
});
