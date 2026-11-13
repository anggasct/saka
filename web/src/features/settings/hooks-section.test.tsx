import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { AUTHORSHIP, botAuthorship, LINK } from "@/test/authorship-fixtures";
import { DIAGNOSTICS, report } from "@/test/diagnostics-fixtures";
import { ENVIRONMENTS } from "@/test/deploy-fixtures";
import { fakeApi, page } from "@/test/fake-api";
import type { Routes } from "@/test/fake-api";
import { project, PROJECT_ID } from "@/test/fixtures";
import {
  announce,
  deploy,
  failedRun,
  gateRun,
  hookRun,
  HOOK_RUNS,
  HOOKS,
  pendingRun,
} from "@/test/hooks-fixtures";
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
    [ENVIRONMENTS]: { body: { items: [] } },
    [HOOKS]: { body: { items: [deploy, announce] } },
    [HOOK_RUNS]: {
      body: page([hookRun(), failedRun, pendingRun, gateRun]),
    },
    ...extra,
  };
}

async function section() {
  return within(await screen.findByRole("region", { name: "Hooks" }));
}

describe("Hooks", () => {
  it("says which operator hooks the repo file switched off", async () => {
    fakeApi(
      routes({
        [PROJECT]: { body: { ...project, hooks_skipped: ["announce"] } },
      }),
    );
    renderApp(SETTINGS);
    const hooks = await section();
    expect(
      await hooks.findByText(
        "Switched off for this project by saka.toml: announce.",
      ),
    ).toBeInTheDocument();
  });

  it("lists the configured hooks with their target but no secret value", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    const hooks = await section();
    expect(await hooks.findByText("deploy-staging")).toBeInTheDocument();
    expect(
      hooks.getByText("on merged, after, run", { exact: false }),
    ).toBeVisible();
    expect(hooks.getByText("./scripts/deploy.sh staging")).toBeVisible();
    expect(hooks.getByText("DEPLOY_TOKEN")).toBeVisible();
    expect(hooks.getByText("https://ci.example.com/saka")).toBeVisible();
    expect(hooks.getByText("HOOK_SIGNING_KEY")).toBeVisible();
    expect(hooks.getByText("300 s")).toBeVisible();
  });

  it("shows the recent runs with state, task link, and the failure sentence", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    const hooks = await section();
    expect(await hooks.findByText("Succeeded")).toBeVisible();
    expect(hooks.getByText("Pending")).toBeVisible();
    expect(
      hooks.getByText("The receiver rejected the request with status 404."),
    ).toBeVisible();
    expect(hooks.getByText("The command exited with code 2.")).toBeVisible();
    expect(hooks.getByText("FAIL smoke")).toBeVisible();
    const links = hooks.getAllByRole("link", { name: "tasks/s1" });
    expect(links[0]).toHaveAttribute(
      "href",
      expect.stringContaining("/delivery/tasks/"),
    );
  });

  it("offers a re-run only for finished runs that no task waits on", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    const hooks = await section();
    await hooks.findByText("Succeeded");
    expect(
      hooks.getAllByRole("button", { name: /^Run .* again$/ }),
    ).toHaveLength(2);
    expect(
      hooks.queryByRole("button", { name: "Run smoke again" }),
    ).not.toBeInTheDocument();
  });

  it("re-runs a failed run with a reason and refreshes the list", async () => {
    let listed = 0;
    const { requests } = fakeApi(
      routes({
        [HOOK_RUNS]: () => {
          listed += 1;
          return { body: page([failedRun]) };
        },
        [`${HOOK_RUNS}/${failedRun.id}/actions/rerun`]: {
          status: 202,
          body: { ...pendingRun, rerun_of: failedRun.id },
        },
      }),
    );
    renderApp(SETTINGS);
    const hooks = await section();
    await userEvent.click(
      await hooks.findByRole("button", { name: "Run announce again" }),
    );
    const dialog = await screen.findByRole("dialog");
    const submit = within(dialog).getByRole("button", { name: "Run again" });
    await userEvent.type(
      within(dialog).getByLabelText("Reason"),
      "receiver fixed",
    );
    await userEvent.click(submit);
    await waitFor(() => {
      expect(listed).toBe(2);
    });
    const post = requests.find((r) => r.method === "POST");
    expect(post?.path).toBe(`${HOOK_RUNS}/${failedRun.id}/actions/rerun`);
    expect(post?.body).toEqual({ reason: "receiver fixed" });
    expect(post?.headers.get("idempotency-key")).toBeTruthy();
  });

  it("explains a refused re-run in the dialog", async () => {
    fakeApi(
      routes({
        [HOOK_RUNS]: { body: page([failedRun]) },
        [`${HOOK_RUNS}/${failedRun.id}/actions/rerun`]: {
          status: 409,
          body: {
            type: "urn:saka:problem:hook_not_configured",
            title: "hook_not_configured",
            status: 409,
            code: "hook_not_configured",
            params: { hook: "announce" },
            correlation_id: null,
          },
        },
      }),
    );
    renderApp(SETTINGS);
    const hooks = await section();
    await userEvent.click(
      await hooks.findByRole("button", { name: "Run announce again" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "try");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Run again" }),
    );
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "The hook announce is no longer in the operator config",
    );
  });

  it("shows the empty states with a pointer to the operator config", async () => {
    fakeApi(
      routes({
        [HOOKS]: { body: { items: [] } },
        [HOOK_RUNS]: { body: page([]) },
      }),
    );
    renderApp(SETTINGS);
    const hooks = await section();
    expect(await hooks.findByText(/Add \[\[hook\]\] entries/)).toBeVisible();
    expect(
      hooks.getByText("No hook has run for this project yet."),
    ).toBeVisible();
  });

  it("shows an error with a retry when the hooks cannot be loaded", async () => {
    let tries = 0;
    fakeApi(
      routes({
        [HOOKS]: () => {
          tries += 1;
          return tries === 1
            ? problemBody(500, "internal")
            : { body: { items: [deploy] } };
        },
      }),
    );
    renderApp(SETTINGS);
    const hooks = await section();
    const alert = await hooks.findByRole("alert");
    expect(
      within(alert).getByText("The hooks could not be loaded."),
    ).toBeVisible();
    await userEvent.click(
      within(alert).getByRole("button", { name: "Try again" }),
    );
    expect(await hooks.findAllByText("deploy-staging")).not.toHaveLength(0);
  });
});
