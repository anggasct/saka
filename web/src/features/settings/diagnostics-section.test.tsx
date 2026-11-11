import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { AUTHORSHIP, botAuthorship, LINK } from "@/test/authorship-fixtures";
import {
  DIAGNOSTICS,
  gitlabReport,
  report,
  unknownReport,
} from "@/test/diagnostics-fixtures";
import { fakeApi, page } from "@/test/fake-api";
import type { Routes } from "@/test/fake-api";
import { project, PROJECT_ID } from "@/test/fixtures";
import { renderApp } from "@/test/render";
import {
  PROJECT,
  PROJECTS,
  problemBody,
  SECRETS,
} from "@/test/settings-fixtures";

const SETTINGS = `/settings/${PROJECT_ID}`;

function routes(extra: Routes = {}): Routes {
  return {
    [PROJECTS]: { body: page([project]) },
    [PROJECT]: { body: project },
    [SECRETS]: { body: { items: [] } },
    [AUTHORSHIP]: { body: botAuthorship },
    [LINK]: problemBody(404, "user_link_not_found"),
    [DIAGNOSTICS]: { body: report },
    ...extra,
  };
}

async function open() {
  const section = within(
    await screen.findByRole("region", { name: "Diagnostics" }),
  );
  await userEvent.click(section.getByText("Show diagnostics"));
  return section;
}

const diagnosticCalls = (calls: string[]) =>
  calls.filter((c) => c.startsWith(DIAGNOSTICS));

describe("Diagnostics", () => {
  it("asks nothing while closed and once when opened", async () => {
    const { calls } = fakeApi(routes());
    renderApp(SETTINGS);
    const section = within(
      await screen.findByRole("region", { name: "Diagnostics" }),
    );
    await screen.findByRole("region", { name: "Secrets" });
    expect(diagnosticCalls(calls)).toEqual([]);
    await userEvent.click(section.getByText("Show diagnostics"));
    expect(await section.findByText(/1 failing, 1 warnings/)).toBeVisible();
    expect(diagnosticCalls(calls)).toEqual([
      `${DIAGNOSTICS}?project_id=${PROJECT_ID}`,
    ]);
  });

  it("groups results in order with counts, sentences, and fixes", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    const section = await open();
    expect(
      await section.findByText("1 failing, 1 warnings, 2 passed, 1 skipped"),
    ).toBeInTheDocument();
    const summaries = section
      .getAllByText(/^(Failing|Warnings|Passed|Skipped) \(/)
      .map((e) => e.textContent);
    expect(summaries).toEqual([
      "Failing (1)",
      "Warnings (1)",
      "Passed (2)",
      "Skipped (1)",
    ]);
    expect(section.getByText("agent.login")).toBeInTheDocument();
    expect(section.getByText("claude:default is not logged in.")).toBeVisible();
    expect(section.getByText("saka agent login").tagName).toBe("CODE");
    expect(
      section.getByText(/Repo web overlaps app of project shop\./),
    ).toBeVisible();
    expect(section.getByText("The server is ready.")).toBeInTheDocument();
  });

  it("shows an unknown code as a generic result", async () => {
    fakeApi(routes({ [DIAGNOSTICS]: { body: unknownReport } }));
    renderApp(SETTINGS);
    const section = await open();
    expect(await section.findByText("Result something_new.")).toBeVisible();
  });

  it("names the provider-neutral scm checks and words their GitLab findings", async () => {
    fakeApi(routes({ [DIAGNOSTICS]: { body: gitlabReport } }));
    renderApp(SETTINGS);
    const section = await open();
    expect(await section.findByText("scm.credential")).toBeInTheDocument();
    expect(section.getByText("scm.repo_access")).toBeInTheDocument();
    expect(section.getByText("scm.repo_webhook")).toBeInTheDocument();
    expect(
      section.getByText("The GitLab token saka works (scopes: api)."),
    ).toBeInTheDocument();
    expect(
      section.getByText(/level 40 \(Maintainer\) is recommended/),
    ).toBeVisible();
    expect(
      section.getByText(
        "The project has no webhook that posts to /hooks/gitlab.",
      ),
    ).toBeVisible();
    expect(
      section.getByText(
        /Add a project webhook at https:\/\/gitlab\.example\/acme\/app\/-\/hooks/,
      ),
    ).toBeVisible();
    expect(section.queryByText(/^Result /)).toBeNull();
  });

  it("runs again on request", async () => {
    const { calls } = fakeApi(routes());
    renderApp(SETTINGS);
    const section = await open();
    await userEvent.click(
      await section.findByRole("button", { name: "Run again" }),
    );
    await waitFor(() => {
      expect(diagnosticCalls(calls)).toHaveLength(2);
    });
  });

  it("shows an error card when the checks cannot run", async () => {
    fakeApi(routes({ [DIAGNOSTICS]: problemBody(500, "internal") }));
    renderApp(SETTINGS);
    const section = await open();
    expect(
      await section.findByText("Diagnostics could not be run"),
    ).toBeInTheDocument();
    expect(
      section.getByRole("button", { name: "Try again" }),
    ).toBeInTheDocument();
  });
});
