import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { fakeApi, page } from "@/test/fake-api";
import { detail, project, PROJECT_ID, SPEC_ID, task } from "@/test/fixtures";
import { specWithSteps } from "@/test/fixtures";
import { renderWithProviders } from "@/test/render";

import type { WorkItemDetail } from "./queries";
import { TaskPage } from "./task-page";

type Regression = NonNullable<WorkItemDetail["regression"]>;

const COMMIT = "c0ffee12".padEnd(40, "0");

function render(regression: Regression | null, locale: "en" | "id" = "en") {
  const item = detail(
    task(1, { state: "in_stage", kind: "bugfix", regression }),
    { stage_runs: [] },
  );
  fakeApi({
    [`/api/v1/work-items/${item.id}`]: { body: item },
    [`/api/v1/specs/${SPEC_ID}`]: { body: specWithSteps([["S1", []]]) },
    [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
    "/api/v1/work-items": { body: page([item]) },
  });
  renderWithProviders(<TaskPage workItemId={item.id} />, locale);
}

describe("RegressionSection", () => {
  it("is absent for a task that is not a bugfix", async () => {
    render(null);
    await screen.findByRole("heading", { level: 1 });
    expect(
      screen.queryByRole("region", { name: "Regression evidence" }),
    ).toBeNull();
  });

  it("shows a bugfix that waits for its first check", async () => {
    render({
      state: "pending",
      attempts: 0,
      commit_sha: null,
      last_outcome: null,
      commands: [],
      paths: [],
    });
    const section = await screen.findByRole("region", {
      name: "Regression evidence",
    });
    expect(within(section).getByText("Pending")).toBeInTheDocument();
    expect(within(section).getByText("Check 0 of 3")).toBeInTheDocument();
    expect(within(section).queryByText(/Test commit/)).toBeNull();
  });

  it("shows the proof: the commit, the outcome and each command with its result", async () => {
    render({
      state: "reproduced",
      attempts: 2,
      commit_sha: COMMIT,
      last_outcome: "reproduced",
      commands: [
        {
          command: "make test-regression",
          result: "failed",
          exit_code: 1,
          duration_ms: 5,
        },
        {
          command: "make lint-tests",
          result: "passed",
          exit_code: 0,
          duration_ms: 5,
        },
      ],
      paths: ["tests/login.rs"],
    });
    const section = await screen.findByRole("region", {
      name: "Regression evidence",
    });
    expect(within(section).getByText("Reproduced")).toBeInTheDocument();
    expect(within(section).getByText("Check 2 of 3")).toBeInTheDocument();
    expect(
      within(section).getByText("Test commit c0ffee120000"),
    ).toBeInTheDocument();
    expect(
      within(section).getByText(
        "The regression commands fail at this commit, so the test catches the bug.",
      ),
    ).toBeInTheDocument();
    const commands = within(section).getAllByRole("listitem");
    const [first, second] = commands as [HTMLElement, HTMLElement];
    expect(within(first).getByText("make test-regression")).toBeInTheDocument();
    expect(within(first).getByText("Failed: the proof")).toBeInTheDocument();
    expect(within(second).getByText("Passed")).toBeInTheDocument();
    expect(within(section).queryByText("tests/login.rs")).toBeNull();
  });

  it("names the files that are not tests when the commit was refused", async () => {
    render({
      state: "pending",
      attempts: 1,
      commit_sha: COMMIT,
      last_outcome: "invalid_paths",
      commands: [],
      paths: ["src/lib.rs"],
    });
    const section = await screen.findByRole("region", {
      name: "Regression evidence",
    });
    expect(within(section).getByText("src/lib.rs")).toBeInTheDocument();
    expect(
      within(section).getByText(/The commit changes files that are not tests/),
    ).toBeInTheDocument();
  });

  it("says in Indonesian that the test was not proven after the checks ran out", async () => {
    render(
      {
        state: "failed",
        attempts: 3,
        commit_sha: COMMIT,
        last_outcome: "not_reproduced",
        commands: [
          {
            command: "make test-regression",
            result: "passed",
            exit_code: 0,
            duration_ms: 5,
          },
        ],
        paths: [],
      },
      "id",
    );
    const section = await screen.findByRole("region", {
      name: "Bukti regresi",
    });
    expect(within(section).getByText("Tidak terbukti")).toBeInTheDocument();
    expect(
      within(section).getByText("Pengecekan 3 dari 3"),
    ).toBeInTheDocument();
  });
});
