import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { fakeApi, page } from "@/test/fake-api";
import { detail, project, PROJECT_ID, SPEC_ID, task } from "@/test/fixtures";
import { specWithSteps } from "@/test/fixtures";
import { renderWithProviders } from "@/test/render";

import { TaskPage } from "./task-page";

function render(queue: { position: number; size: number } | null) {
  const item = detail(
    task(1, {
      state: "in_stage",
      stage: "merge",
      stage_phase: queue?.position === 0 ? "integrating" : "queued",
      merge_queue: queue && { base_branch: "main", ...queue },
    }),
  );
  fakeApi({
    [`/api/v1/work-items/${item.id}`]: { body: item },
    [`/api/v1/specs/${SPEC_ID}`]: { body: specWithSteps([["S1", []]]) },
    [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
    "/api/v1/work-items": { body: page([item]) },
  });
  renderWithProviders(<TaskPage workItemId={item.id} />);
}

describe("MergeQueueSection", () => {
  it("says how many tasks are ahead in the queue of the target branch", async () => {
    render({ position: 3, size: 4 });
    const region = await screen.findByRole("region", { name: "Merge queue" });
    expect(region).toHaveTextContent(
      "2 tasks ahead to merge into main, 4 tasks in the queue.",
    );
  });

  it("says the task is next when nothing is ahead", async () => {
    render({ position: 1, size: 1 });
    const region = await screen.findByRole("region", { name: "Merge queue" });
    expect(region).toHaveTextContent(
      "Next in line to merge into main, 1 task in the queue.",
    );
  });

  it("says the task holds the turn", async () => {
    render({ position: 0, size: 2 });
    const region = await screen.findByRole("region", { name: "Merge queue" });
    expect(region).toHaveTextContent(
      "This task holds the turn to merge into main.",
    );
    expect(
      screen.getAllByText(/bringing the branch up to date with the base/)
        .length,
    ).toBeGreaterThan(0);
  });

  it("is absent outside the queue", async () => {
    render(null);
    await screen.findByRole("heading", { level: 1 });
    expect(screen.queryByRole("region", { name: "Merge queue" })).toBeNull();
  });
});
