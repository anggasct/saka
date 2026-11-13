import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { AUTHORSHIP, botAuthorship, LINK } from "@/test/authorship-fixtures";
import { DIAGNOSTICS, report } from "@/test/diagnostics-fixtures";
import {
  ENVIRONMENTS,
  environment,
  OTHER_DEPLOYMENT_ID,
} from "@/test/deploy-fixtures";
import { fakeApi, page } from "@/test/fake-api";
import { INCIDENT_SOURCES } from "@/test/incident-fixtures";
import type { Routes } from "@/test/fake-api";
import { project, PROJECT_ID } from "@/test/fixtures";
import { announce, HOOK_RUNS, HOOKS } from "@/test/hooks-fixtures";
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
    [HOOKS]: { body: { items: [{ ...announce, environments: ["staging"] }] } },
    [HOOK_RUNS]: { body: page([]) },
    [ENVIRONMENTS]: { body: { items: [] } },
    [INCIDENT_SOURCES]: { body: { items: [] } },
    ...extra,
  };
}

async function section() {
  return within(await screen.findByRole("region", { name: "Environments" }));
}

describe("Environments", () => {
  it("lists each environment the project defines, read only", async () => {
    fakeApi(
      routes({
        [ENVIRONMENTS]: {
          body: {
            items: [
              environment(),
              environment({
                name: "production",
                action: "webhook",
                gate: false,
                on_failure: "fix",
                target_url: null,
                verification: {
                  ready: false,
                  commands: 0,
                  warnings: ["verify_missing", "ready_missing"],
                },
                last_succeeded: null,
              }),
            ],
          },
        },
      }),
    );
    renderApp(SETTINGS);
    const environments = await section();
    const staging = (await environments.findByText("staging")).closest(
      "li",
    ) as HTMLElement;
    expect(within(staging).getByText("acme/app · main")).toBeInTheDocument();
    expect(within(staging).getByText("Run on the runner")).toBeInTheDocument();
    expect(
      within(staging).getByText("Yes: tasks wait for a healthy deployment"),
    ).toBeInTheDocument();
    expect(within(staging).getByText("Revert the change")).toBeInTheDocument();
    expect(
      within(staging).getByText("Readiness probe, 2 commands"),
    ).toBeInTheDocument();
    expect(
      within(staging).getByText("https://staging.example.com"),
    ).toBeInTheDocument();
    expect(within(staging).getByText("c0ffee000000")).toBeInTheDocument();
    expect(
      within(staging).getByRole("link", { name: "Open the deployments" }),
    ).toHaveAttribute(
      "href",
      `/deployments/${PROJECT_ID}?environment=staging&deployment=${OTHER_DEPLOYMENT_ID}`,
    );
    expect(
      within(staging).queryByText("No verification commands"),
    ).not.toBeInTheDocument();

    const production = environments
      .getByText("production")
      .closest("li") as HTMLElement;
    expect(within(production).getByText("Call a webhook")).toBeInTheDocument();
    expect(
      within(production).getByText("No: tasks finish without waiting"),
    ).toBeInTheDocument();
    expect(within(production).getByText("Draft a fix")).toBeInTheDocument();
    expect(
      within(production).getByText("No verification commands"),
    ).toBeInTheDocument();
    expect(
      within(production).getByText("No readiness probe", { selector: "span" }),
    ).toBeInTheDocument();
    expect(
      within(production).getByText("No readiness probe, no commands"),
    ).toBeInTheDocument();
    expect(within(production).getByText("Never")).toBeInTheDocument();
  });

  it("says so when the project defines none", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    const environments = await section();
    expect(
      await environments.findByText(
        "This project defines no deployment environment.",
      ),
    ).toBeInTheDocument();
  });

  it("shows the environments a hook is limited to", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    const hooks = within(await screen.findByRole("region", { name: "Hooks" }));
    expect(await hooks.findByText("Environments")).toBeInTheDocument();
    expect(hooks.getByText("staging")).toBeInTheDocument();
  });

  it("shows an error with a retry when the environments cannot load", async () => {
    fakeApi(
      routes({
        [ENVIRONMENTS]: () => ({
          status: 500,
          body: { code: "internal", params: {} },
        }),
      }),
    );
    renderApp(SETTINGS);
    const environments = await section();
    expect(
      await environments.findByText("Could not load the environments"),
    ).toBeVisible();
  });
});
