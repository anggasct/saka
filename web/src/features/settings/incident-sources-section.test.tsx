import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { AUTHORSHIP, botAuthorship, LINK } from "@/test/authorship-fixtures";
import { ENVIRONMENTS } from "@/test/deploy-fixtures";
import { DIAGNOSTICS, report } from "@/test/diagnostics-fixtures";
import { fakeApi, page } from "@/test/fake-api";
import type { Routes } from "@/test/fake-api";
import { project, PROJECT_ID } from "@/test/fixtures";
import { HOOK_RUNS, HOOKS } from "@/test/hooks-fixtures";
import { INCIDENT_SOURCES, incidentSource } from "@/test/incident-fixtures";
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
    [HOOKS]: { body: { items: [] } },
    [HOOK_RUNS]: { body: page([]) },
    [ENVIRONMENTS]: { body: { items: [] } },
    [INCIDENT_SOURCES]: { body: { items: [] } },
    ...extra,
  };
}

async function section() {
  return within(
    await screen.findByRole("region", { name: "Incident sources" }),
  );
}

describe("Incident sources", () => {
  it("lists each source with its endpoint and limits, read only", async () => {
    fakeApi(
      routes({
        [INCIDENT_SOURCES]: {
          body: {
            items: [
              incidentSource(),
              incidentSource({
                name: "web-errors",
                kind: "sentry",
                endpoint: null,
                endpoint_path: "/hooks/incident/web-errors",
                action: "inbox",
                min_severity: "critical",
                max_open: 10,
                open: 0,
                repo: null,
              }),
            ],
          },
        },
      }),
    );
    renderApp(SETTINGS);
    const sources = await section();
    const alerts = (await sources.findByText("prod-alerts")).closest(
      "li",
    ) as HTMLElement;
    expect(within(alerts).getByText("Alertmanager")).toBeInTheDocument();
    expect(
      within(alerts).getByText(
        "https://saka.example.com/hooks/incident/prod-alerts",
      ),
    ).toBeInTheDocument();
    expect(
      within(alerts).getByText("Drafts a fix from severity High up"),
    ).toBeInTheDocument();
    expect(within(alerts).getByText("3 of 50")).toBeInTheDocument();
    expect(within(alerts).getByText("app")).toBeInTheDocument();

    const web = sources.getByText("web-errors").closest("li") as HTMLElement;
    expect(within(web).getByText("Sentry")).toBeInTheDocument();
    expect(
      within(web).getByText("/hooks/incident/web-errors"),
    ).toBeInTheDocument();
    expect(within(web).getByText("Opens an inbox item")).toBeInTheDocument();
    expect(within(web).getByText("0 of 10")).toBeInTheDocument();
    expect(within(web).queryByText("Repository")).not.toBeInTheDocument();
    expect(sources.queryByRole("button")).not.toBeInTheDocument();
  });

  it("says so when the project has none", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    const sources = await section();
    expect(
      await sources.findByText(
        "This project has no incident source. Operators define them in the server config.",
      ),
    ).toBeInTheDocument();
  });

  it("shows an error when the sources cannot load", async () => {
    fakeApi(
      routes({
        [INCIDENT_SOURCES]: () => ({
          status: 500,
          body: { code: "internal", params: {} },
        }),
      }),
    );
    renderApp(SETTINGS);
    const sources = await section();
    expect(
      await sources.findByText("Could not load the incident sources"),
    ).toBeVisible();
  });
});
