import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fakeApi, page } from "@/test/fake-api";
import type { Recorded, Routes } from "@/test/fake-api";
import {
  check,
  DEPLOYMENT_ID,
  DEPLOYMENTS,
  deployment,
  detailUrl,
  DRAFT_ID,
  ENVIRONMENTS,
  environment,
  failedDetail,
  OTHER_DEPLOYMENT_ID,
  REDEPLOY,
} from "@/test/deploy-fixtures";
import { project, PROJECT_ID } from "@/test/fixtures";
import { renderApp } from "@/test/render";

const PAGE_URL = `/deployments/${PROJECT_ID}`;

const listed = [
  deployment({ id: DEPLOYMENT_ID, state: "failed", cause: "merge" }),
  deployment({
    id: OTHER_DEPLOYMENT_ID,
    state: "succeeded",
    cause: "redeploy",
    sha: "c0ffee".padEnd(40, "0"),
  }),
];

function routes(extra: Routes = {}): Routes {
  return {
    [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
    [ENVIRONMENTS]: {
      body: {
        items: [environment(), environment({ name: "production" })],
      },
    },
    [DEPLOYMENTS]: { body: page(listed) },
    [detailUrl(DEPLOYMENT_ID)]: { body: failedDetail },
    ...extra,
  };
}

function posts(requests: Recorded[]): Recorded[] {
  return requests.filter((r) => r.method === "POST");
}

async function detailPanel(name = "Deployment of staging") {
  return screen.findByRole("region", { name });
}

describe("deployments page", () => {
  it("lists the environments and the history, newest first", async () => {
    fakeApi(routes());
    renderApp(PAGE_URL);
    expect(
      await screen.findByRole("heading", { name: "Deployments", level: 1 }),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("heading", { name: "production" }),
    ).toBeInTheDocument();
    const rows = await screen.findAllByRole("link", { name: "staging" });
    expect(rows).toHaveLength(2);
    expect(screen.getByText("Merge")).toBeInTheDocument();
    expect(screen.getAllByText("Redeploy").length).toBeGreaterThan(2);
    expect(screen.getAllByText("feedface0000").length).toBeGreaterThan(0);
    expect(screen.getAllByText("c0ffee000000").length).toBeGreaterThan(0);
  });

  it("changes the request when a filter changes", async () => {
    const { requests } = fakeApi(routes());
    renderApp(PAGE_URL);
    await screen.findAllByRole("link", { name: "staging" });
    await userEvent.selectOptions(
      screen.getByLabelText("Environment"),
      "production",
    );
    await waitFor(() => {
      expect(
        requests.some(
          (r) =>
            r.path === DEPLOYMENTS &&
            r.search.get("environment") === "production",
        ),
      ).toBe(true);
    });
    await userEvent.selectOptions(screen.getByLabelText("State"), "failed");
    await waitFor(() => {
      expect(
        requests.some(
          (r) =>
            r.path === DEPLOYMENTS &&
            r.search.get("environment") === "production" &&
            r.search.get("state") === "failed",
        ),
      ).toBe(true);
    });
  });

  it("shows the evidence of the deployment the url selects", async () => {
    fakeApi(routes());
    renderApp(`${PAGE_URL}?deployment=${DEPLOYMENT_ID}`);
    const panel = await detailPanel();
    expect(
      within(panel).getByText("1 of 2 verification commands failed."),
    ).toBeInTheDocument();
    expect(within(panel).getByText("Failed at Verification")).toBeVisible();
    expect(within(panel).getByText("Deploy action")).toBeInTheDocument();
    expect(within(panel).getByText("deploy log line")).toBeInTheDocument();
    expect(within(panel).getAllByText("npm run e2e")).toHaveLength(2);
    expect(within(panel).getByText("exit code 1")).toBeInTheDocument();
    expect(within(panel).getByText("HTTP 200")).toBeInTheDocument();
    expect(within(panel).getByText("3 attempts")).toBeInTheDocument();
    const tail = within(panel).getByText("<b>FAIL</b> e2e");
    expect(tail.tagName).toBe("PRE");
    expect(tail.querySelector("b")).toBeNull();
    expect(within(panel).getByRole("link", { name: "PR #12" })).toHaveAttribute(
      "href",
      "https://github.com/acme/app/pull/12",
    );
    expect(
      within(panel).getByRole("link", { name: "tasks-done/s1" }),
    ).toHaveAttribute(
      "href",
      "/delivery/tasks/0193f0c4-1b2a-7c3d-9e8f-001122334501",
    );
    expect(
      within(panel).getByRole("link", { name: "Open the fix draft" }),
    ).toHaveAttribute("href", `/docs/${PROJECT_ID}/drafts/${DRAFT_ID}`);
    expect(
      within(panel).getAllByText("curl -f https://staging.example.com/ready"),
    ).toHaveLength(2);
  });

  it("opens a deployment from the history", async () => {
    fakeApi(routes());
    const { router } = renderApp(PAGE_URL);
    const [first] = await screen.findAllByRole("link", { name: "staging" });
    if (first === undefined) {
      throw new Error("no deployment in the history");
    }
    await userEvent.click(first);
    await detailPanel();
    expect(router.state.location.search).toEqual({
      deployment: DEPLOYMENT_ID,
    });
  });

  it("redeploys with a reason and an idempotency key", async () => {
    const { requests } = fakeApi(
      routes({ [REDEPLOY]: { status: 202, body: deployment() } }),
    );
    renderApp(`${PAGE_URL}?deployment=${DEPLOYMENT_ID}`);
    const panel = await detailPanel();
    await userEvent.click(
      await within(panel).findByRole("button", { name: "Redeploy" }),
    );
    const dialog = screen.getByRole("dialog");
    const submit = within(dialog).getByRole("button", { name: "Redeploy" });
    expect(submit).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText("Reason"), "hotfix");
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [sent] = posts(requests);
    expect(sent?.path).toBe(REDEPLOY);
    expect(sent?.body).toEqual({ reason: "hotfix" });
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("redeploys from an environment card and explains a queued deployment", async () => {
    fakeApi(
      routes({
        [REDEPLOY]: () => ({
          status: 409,
          body: {
            code: "deployment_queued",
            params: { deployment_id: DEPLOYMENT_ID },
          },
        }),
      }),
    );
    renderApp(PAGE_URL);
    const card = (
      await screen.findAllByRole("heading", { name: "staging" })
    )[0]?.closest("li") as HTMLElement;
    await userEvent.click(
      within(card).getByRole("button", { name: "Redeploy" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "again");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Redeploy" }),
    );
    expect(
      await within(dialog).findByText(
        "A deployment of this environment is already queued.",
      ),
    ).toBeInTheDocument();
  });

  it("cancels a verifying deployment and explains a deploy that cannot be stopped", async () => {
    const verifying = deployment({
      state: "verifying",
      phase: "verify",
      checks: [check()],
    });
    let answer: { status: number; body: unknown } = {
      status: 409,
      body: { code: "deployment_running", params: {} },
    };
    const { requests } = fakeApi(
      routes({
        [detailUrl(DEPLOYMENT_ID)]: { body: verifying },
        [`${detailUrl(DEPLOYMENT_ID)}/actions/cancel`]: () => answer,
      }),
    );
    renderApp(`${PAGE_URL}?deployment=${DEPLOYMENT_ID}`);
    const panel = await detailPanel();
    await userEvent.click(
      await within(panel).findByRole("button", { name: "Cancel deployment" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.type(
      within(dialog).getByLabelText("Reason"),
      "wrong build",
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Cancel the deployment" }),
    );
    expect(
      await within(dialog).findByText(
        "The deploy action is running and cannot be stopped.",
      ),
    ).toBeInTheDocument();
    answer = { status: 200, body: { ...verifying, state: "cancelled" } };
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Cancel the deployment" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const sent = posts(requests);
    expect(sent.map((r) => r.body)).toEqual([
      { reason: "wrong build" },
      { reason: "wrong build" },
    ]);
    for (const request of sent) {
      expect(request.path).toBe(`${detailUrl(DEPLOYMENT_ID)}/actions/cancel`);
      expect(request.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
    }
  });

  it.each([
    ["succeeded", false],
    ["deploying", false],
    ["failed", false],
    ["cancelled", false],
    ["queued", true],
    ["awaiting_ready", true],
    ["verifying", true],
  ])("offers cancel for a %s deployment: %s", async (state, offered) => {
    fakeApi(
      routes({
        [detailUrl(DEPLOYMENT_ID)]: { body: deployment({ state }) },
      }),
    );
    renderApp(`${PAGE_URL}?deployment=${DEPLOYMENT_ID}`);
    const panel = await detailPanel();
    await within(panel).findByRole("button", { name: "Redeploy" });
    expect(
      within(panel).queryByRole("button", { name: "Cancel deployment" }) !==
        null,
    ).toBe(offered);
  });

  it("offers a fix only for a failed deployment", async () => {
    fakeApi(
      routes({
        [detailUrl(DEPLOYMENT_ID)]: {
          body: deployment({ state: "succeeded" }),
        },
      }),
    );
    renderApp(`${PAGE_URL}?deployment=${DEPLOYMENT_ID}`);
    const panel = await detailPanel();
    await within(panel).findByRole("button", { name: "Redeploy" });
    expect(
      within(panel).queryByRole("button", { name: "Draft a fix" }),
    ).not.toBeInTheDocument();
  });

  it("drafts a fix with an optional note", async () => {
    const { requests } = fakeApi(
      routes({
        [`${detailUrl(DEPLOYMENT_ID)}/actions/draft-fix`]: {
          status: 202,
          body: { id: DRAFT_ID },
        },
      }),
    );
    renderApp(`${PAGE_URL}?deployment=${DEPLOYMENT_ID}`);
    const panel = await detailPanel();
    await userEvent.click(
      await within(panel).findByRole("button", { name: "Draft a fix" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.type(
      within(dialog).getByLabelText("Note for the architect (optional)"),
      "look at the e2e",
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Draft the fix" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [sent] = posts(requests);
    expect(sent?.body).toEqual({ note: "look at the e2e" });
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("explains a fix that is already being drafted", async () => {
    fakeApi(
      routes({
        [`${detailUrl(DEPLOYMENT_ID)}/actions/draft-fix`]: () => ({
          status: 409,
          body: { code: "fix_draft_in_progress", params: {} },
        }),
      }),
    );
    renderApp(`${PAGE_URL}?deployment=${DEPLOYMENT_ID}`);
    const panel = await detailPanel();
    await userEvent.click(
      await within(panel).findByRole("button", { name: "Draft a fix" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Draft the fix" }),
    );
    expect(
      await within(dialog).findByText(
        "A fix for this deployment is already being drafted.",
      ),
    ).toBeInTheDocument();
  });

  it("says when nothing matches and when no environment exists", async () => {
    fakeApi(
      routes({
        [ENVIRONMENTS]: { body: { items: [] } },
        [DEPLOYMENTS]: { body: page([]) },
      }),
    );
    renderApp(PAGE_URL);
    expect(
      await screen.findByText("No environment is defined for this project."),
    ).toBeInTheDocument();
    expect(
      await screen.findByText("No deployment matches this filter."),
    ).toBeInTheDocument();
  });

  it("shows an error with a retry when the history cannot load", async () => {
    let failing = true;
    fakeApi(
      routes({
        [DEPLOYMENTS]: () =>
          failing
            ? { status: 500, body: { code: "internal", params: {} } }
            : { body: page(listed) },
      }),
    );
    renderApp(PAGE_URL);
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText("Could not load the history")).toBeVisible();
    failing = false;
    await userEvent.click(
      within(alert).getByRole("button", { name: "Try again" }),
    );
    expect(
      (await screen.findAllByRole("link", { name: "staging" })).length,
    ).toBeGreaterThan(0);
  });

  it("shows a missing deployment as an error", async () => {
    fakeApi(
      routes({
        [detailUrl(DEPLOYMENT_ID)]: () => ({
          status: 404,
          body: { code: "not_found", params: {} },
        }),
      }),
    );
    renderApp(`${PAGE_URL}?deployment=${DEPLOYMENT_ID}`);
    expect(
      await screen.findByText("Could not load the deployment"),
    ).toBeVisible();
  });
});
