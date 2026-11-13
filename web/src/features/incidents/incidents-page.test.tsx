import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { ENVIRONMENTS, environment } from "@/test/deploy-fixtures";
import { fakeApi, page } from "@/test/fake-api";
import type { Recorded, Routes } from "@/test/fake-api";
import { project, PROJECT_ID } from "@/test/fixtures";
import {
  INCIDENT_DEPLOYMENT_ID,
  INCIDENT_DRAFT_ID,
  INCIDENT_ID,
  INCIDENT_REVERT_ID,
  INCIDENT_SPEC_ID,
  INCIDENT_TASK_ID,
  INCIDENTS,
  incident,
  incidentDetail,
  incidentUrl,
  OTHER_INCIDENT_ID,
} from "@/test/incident-fixtures";
import { renderApp } from "@/test/render";

const PAGE_URL = `/incidents/${PROJECT_ID}`;

const listed = [
  incident({ id: INCIDENT_ID }),
  incident({
    id: OTHER_INCIDENT_ID,
    title: "Queue depth growing",
    severity: "low",
    state: "resolved",
    environment: "production",
    occurrences: 1,
  }),
];

function routes(extra: Routes = {}): Routes {
  return {
    [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
    [ENVIRONMENTS]: {
      body: { items: [environment(), environment({ name: "production" })] },
    },
    [INCIDENTS]: { body: page(listed) },
    [incidentUrl(INCIDENT_ID)]: { body: incidentDetail() },
    ...extra,
  };
}

function posts(requests: Recorded[]): Recorded[] {
  return requests.filter((r) => r.method === "POST");
}

async function detailPanel() {
  return screen.findByRole("region", { name: "Checkout error rate above 5%" });
}

describe("incidents page", () => {
  it("lists the incidents with severity, environment, occurrences and state", async () => {
    fakeApi(routes());
    renderApp(PAGE_URL);
    expect(
      await screen.findByRole("heading", { name: "Incidents", level: 1 }),
    ).toBeInTheDocument();
    const first = (
      await screen.findByRole("link", { name: "Checkout error rate above 5%" })
    ).closest("li") as HTMLElement;
    expect(within(first).getByText("High")).toBeInTheDocument();
    expect(within(first).getByText("Open")).toBeInTheDocument();
    expect(within(first).getByText("staging")).toBeInTheDocument();
    expect(within(first).getByText("3 occurrences")).toBeInTheDocument();
    const second = screen
      .getByRole("link", { name: "Queue depth growing" })
      .closest("li") as HTMLElement;
    expect(within(second).getByText("Low")).toBeInTheDocument();
    expect(within(second).getByText("Resolved")).toBeInTheDocument();
    expect(within(second).getByText("1 occurrence")).toBeInTheDocument();
  });

  it("changes the request when a filter changes", async () => {
    const { requests } = fakeApi(routes());
    const { router } = renderApp(PAGE_URL);
    await screen.findByRole("link", { name: "Queue depth growing" });
    await userEvent.selectOptions(screen.getByLabelText("State"), "open");
    await userEvent.selectOptions(screen.getByLabelText("Severity"), "high");
    await userEvent.selectOptions(
      screen.getByLabelText("Environment"),
      "staging",
    );
    await waitFor(() => {
      expect(
        requests.some(
          (r) =>
            r.path === INCIDENTS &&
            r.search.get("state") === "open" &&
            r.search.get("severity") === "high" &&
            r.search.get("environment") === "staging",
        ),
      ).toBe(true);
    });
    expect(router.state.location.search).toEqual({
      state: "open",
      severity: "high",
      environment: "staging",
    });
    await userEvent.selectOptions(screen.getByLabelText("State"), "");
    await waitFor(() => {
      expect(router.state.location.search).toEqual({
        severity: "high",
        environment: "staging",
      });
    });
  });

  it("pages through the incidents", async () => {
    const { requests } = fakeApi(
      routes({
        [INCIDENTS]: (request) =>
          request.search.get("cursor") === "next"
            ? { body: page([listed[1]]) }
            : { body: { items: [listed[0]], next_cursor: "next" } },
      }),
    );
    renderApp(PAGE_URL);
    await screen.findByRole("link", { name: "Checkout error rate above 5%" });
    expect(
      screen.queryByRole("link", { name: "Queue depth growing" }),
    ).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Show more" }));
    await screen.findByRole("link", { name: "Queue depth growing" });
    expect(
      requests.some(
        (r) => r.path === INCIDENTS && r.search.get("cursor") === "next",
      ),
    ).toBe(true);
    expect(
      screen.queryByRole("button", { name: "Show more" }),
    ).not.toBeInTheDocument();
  });

  it("opens an incident from the list", async () => {
    fakeApi(routes());
    const { router } = renderApp(PAGE_URL);
    await userEvent.click(
      await screen.findByRole("link", { name: "Checkout error rate above 5%" }),
    );
    await detailPanel();
    expect(router.state.location.search).toEqual({ incident: INCIDENT_ID });
  });

  it("shows the signal, the deployment with its range, and the suspects", async () => {
    fakeApi(routes());
    renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
    const panel = await detailPanel();
    expect(
      within(panel).getByText(
        "5xx responses on /checkout rose after the last deployment.",
      ),
    ).toBeInTheDocument();
    expect(
      within(panel).getByRole("link", { name: "Open in the monitoring tool" }),
    ).toHaveAttribute("href", "https://alerts.example.com/alert/42");
    expect(within(panel).getByText("alertname")).toBeInTheDocument();
    expect(within(panel).getByText("HighErrorRate")).toBeInTheDocument();
    expect(within(panel).getByText("prod-alerts")).toBeInTheDocument();
    expect(
      within(panel).getByText("c0ffee000000 → feedface0000"),
    ).toBeInTheDocument();
    expect(
      within(panel).getByRole("link", { name: "Open the deployment" }),
    ).toHaveAttribute(
      "href",
      `/deployments/${PROJECT_ID}?deployment=${INCIDENT_DEPLOYMENT_ID}`,
    );
    expect(
      within(panel).getByRole("link", { name: "tasks-done/s1" }),
    ).toHaveAttribute("href", `/delivery/tasks/${INCIDENT_TASK_ID}`);
    expect(within(panel).getByRole("link", { name: "PR #12" })).toHaveAttribute(
      "href",
      "https://github.com/acme/app/pull/12",
    );
    expect(within(panel).getByText("merged outside Saka")).toBeInTheDocument();
    expect(
      within(panel).getAllByRole("button", { name: "Revert" }),
    ).toHaveLength(1);
  });

  it("shows the text of an alert as plain text and links only http urls", async () => {
    fakeApi(
      routes({
        [incidentUrl(INCIDENT_ID)]: {
          body: incidentDetail({
            title: "<img src=x onerror=alert(1)>",
            summary: "**bold** <script>alert(1)</script>",
            url: "javascript:alert(1)",
            labels: { "<i>k</i>": "<b>v</b>" },
          }),
        },
      }),
    );
    renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
    const panel = await screen.findByRole("region", {
      name: "<img src=x onerror=alert(1)>",
    });
    expect(panel.querySelector("img, script, b, i, strong")).toBeNull();
    expect(
      within(panel).getByText("**bold** <script>alert(1)</script>"),
    ).toBeInTheDocument();
    expect(within(panel).getByText("<b>v</b>")).toBeInTheDocument();
    expect(
      within(panel).queryByRole("link", {
        name: "Open in the monitoring tool",
      }),
    ).not.toBeInTheDocument();
  });

  it("says when the incident has no deployment and no suspect", async () => {
    fakeApi(
      routes({
        [incidentUrl(INCIDENT_ID)]: {
          body: incidentDetail({
            deployment: null,
            range: null,
            suspects: [],
            environment: null,
            summary: "",
            url: null,
            labels: {},
          }),
        },
      }),
    );
    renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
    const panel = await detailPanel();
    expect(
      within(panel).getByText(
        "No deployment of this environment had succeeded when the incident opened.",
      ),
    ).toBeInTheDocument();
    expect(
      within(panel).getByText(
        "No task or merged change is linked to this incident.",
      ),
    ).toBeInTheDocument();
    expect(
      within(panel).getByText("The source sent no summary."),
    ).toBeInTheDocument();
    expect(
      within(panel).queryByRole("button", { name: "Revert" }),
    ).not.toBeInTheDocument();
  });

  it("drafts a fix with an optional note", async () => {
    const { requests } = fakeApi(
      routes({
        [`${incidentUrl(INCIDENT_ID)}/actions/draft-fix`]: {
          status: 202,
          body: { id: INCIDENT_DRAFT_ID },
        },
      }),
    );
    renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
    const panel = await detailPanel();
    await userEvent.click(
      await within(panel).findByRole("button", { name: "Draft a fix" }),
    );
    const dialog = screen.getByRole("dialog", {
      name: "Draft a fix for this incident",
    });
    await userEvent.type(
      within(dialog).getByLabelText("Note for the architect (optional)"),
      "look at checkout",
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Draft the fix" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [sent] = posts(requests);
    expect(sent?.path).toBe(`${incidentUrl(INCIDENT_ID)}/actions/draft-fix`);
    expect(sent?.body).toEqual({ note: "look at checkout" });
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("explains a draft that cannot start", async () => {
    let code = "incident_handling";
    fakeApi(
      routes({
        [`${incidentUrl(INCIDENT_ID)}/actions/draft-fix`]: () => ({
          status: 409,
          body: { code, params: {} },
        }),
      }),
    );
    renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
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
        "A recovery for this incident is already in progress.",
      ),
    ).toBeInTheDocument();
    code = "architect_unavailable";
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Draft the fix" }),
    );
    expect(
      await within(dialog).findByText(
        "The architect is not available. Try again later.",
      ),
    ).toBeInTheDocument();
  });

  it("reverts a suspect task with a reason", async () => {
    const { requests } = fakeApi(
      routes({
        [`${incidentUrl(INCIDENT_ID)}/actions/revert`]: {
          status: 202,
          body: {},
        },
      }),
    );
    renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
    const panel = await detailPanel();
    await userEvent.click(
      await within(panel).findByRole("button", { name: "Revert" }),
    );
    const dialog = screen.getByRole("dialog", { name: "Revert tasks-done/s1" });
    const submit = within(dialog).getByRole("button", {
      name: "Revert the task",
    });
    expect(submit).toBeDisabled();
    await userEvent.type(
      within(dialog).getByLabelText("Reason"),
      "errors began with this task",
    );
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [sent] = posts(requests);
    expect(sent?.path).toBe(`${incidentUrl(INCIDENT_ID)}/actions/revert`);
    expect(sent?.body).toEqual({
      work_item_id: INCIDENT_TASK_ID,
      reason: "errors began with this task",
    });
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("explains a revert of a task that is not a suspect, and a closed incident", async () => {
    let code = "incident_suspect_unknown";
    fakeApi(
      routes({
        [`${incidentUrl(INCIDENT_ID)}/actions/revert`]: () => ({
          status: 409,
          body: { code, params: {} },
        }),
      }),
    );
    renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
    const panel = await detailPanel();
    await userEvent.click(
      await within(panel).findByRole("button", { name: "Revert" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "why");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Revert the task" }),
    );
    expect(
      await within(dialog).findByText(
        "That task is not a suspect of this incident.",
      ),
    ).toBeInTheDocument();
    code = "incident_closed";
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Revert the task" }),
    );
    expect(
      await within(dialog).findByText("This incident is already closed."),
    ).toBeInTheDocument();
  });

  it("dismisses an open incident only with a reason", async () => {
    const { requests } = fakeApi(
      routes({
        [`${incidentUrl(INCIDENT_ID)}/actions/dismiss`]: {
          body: incident({ state: "dismissed" }),
        },
      }),
    );
    renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
    const panel = await detailPanel();
    await userEvent.click(
      await within(panel).findByRole("button", { name: "Dismiss" }),
    );
    const dialog = screen.getByRole("dialog", {
      name: "Dismiss this incident",
    });
    const submit = within(dialog).getByRole("button", {
      name: "Dismiss the incident",
    });
    expect(submit).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText("Reason"), "test alert");
    await userEvent.click(submit);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const [sent] = posts(requests);
    expect(sent?.path).toBe(`${incidentUrl(INCIDENT_ID)}/actions/dismiss`);
    expect(sent?.body).toEqual({ reason: "test alert" });
    expect(sent?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("explains a dismissal of an incident that is no longer open", async () => {
    fakeApi(
      routes({
        [`${incidentUrl(INCIDENT_ID)}/actions/dismiss`]: () => ({
          status: 409,
          body: { code: "invalid_state", params: {} },
        }),
      }),
    );
    renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
    const panel = await detailPanel();
    await userEvent.click(
      await within(panel).findByRole("button", { name: "Dismiss" }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Reason"), "noise");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Dismiss the incident" }),
    );
    expect(
      await within(dialog).findByText(
        "Only an open incident can be dismissed.",
      ),
    ).toBeInTheDocument();
  });

  it("offers the recovery that is running and no action while handling", async () => {
    fakeApi(
      routes({
        [incidentUrl(INCIDENT_ID)]: {
          body: incidentDetail({
            state: "handling",
            draft: { id: INCIDENT_DRAFT_ID, state: "drafting" },
            spec: {
              id: INCIDENT_SPEC_ID,
              path: "docs/specs/fix-checkout.md",
              state: "approved",
            },
            revert: {
              id: INCIDENT_REVERT_ID,
              key: "tasks-done/revert",
              state: "in_stage",
            },
          }),
        },
      }),
    );
    renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
    const panel = await detailPanel();
    expect(
      within(panel).getByText(
        "A recovery is already in progress, so no other action is offered until it ends.",
      ),
    ).toBeInTheDocument();
    for (const name of ["Draft a fix", "Dismiss", "Revert"]) {
      expect(
        within(panel).queryByRole("button", { name }),
      ).not.toBeInTheDocument();
    }
    expect(
      within(panel).getByRole("link", { name: "Open the fix draft" }),
    ).toHaveAttribute(
      "href",
      `/docs/${PROJECT_ID}/drafts/${INCIDENT_DRAFT_ID}`,
    );
    expect(
      within(panel).getByRole("link", {
        name: "Spec docs/specs/fix-checkout.md",
      }),
    ).toHaveAttribute(
      "href",
      `/docs/${PROJECT_ID}?path=docs%2Fspecs%2Ffix-checkout.md`,
    );
    expect(
      within(panel).getByRole("link", {
        name: "Revert task tasks-done/revert",
      }),
    ).toHaveAttribute("href", `/delivery/tasks/${INCIDENT_REVERT_ID}`);
    expect(within(panel).getByText("Handling")).toBeInTheDocument();
  });

  it.each([
    ["resolved", "fixed", "Fixed"],
    ["resolved", "reverted", "Reverted"],
    ["resolved", "signal_resolved", "Resolved by the source"],
  ])(
    "offers no action on a %s incident (%s)",
    async (state, resolution, text) => {
      fakeApi(
        routes({
          [incidentUrl(INCIDENT_ID)]: {
            body: incidentDetail({
              state,
              resolution,
              resolved_at: "2026-11-06T11:00:00.000Z",
            }),
          },
        }),
      );
      renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
      const panel = await detailPanel();
      expect(within(panel).getByText(text)).toBeInTheDocument();
      expect(within(panel).queryByRole("button")).toBeNull();
    },
  );

  it("shows who dismissed an incident and why", async () => {
    fakeApi(
      routes({
        [incidentUrl(INCIDENT_ID)]: {
          body: incidentDetail({
            state: "dismissed",
            dismiss_reason: "test alert",
            actor: "operator",
            resolved_at: "2026-11-06T11:00:00.000Z",
          }),
        },
      }),
    );
    renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
    const panel = await detailPanel();
    expect(within(panel).getByText("test alert")).toBeInTheDocument();
    expect(within(panel).getByText("operator")).toBeInTheDocument();
    expect(within(panel).getByText("Dismissed")).toBeInTheDocument();
    expect(within(panel).queryByRole("button")).toBeNull();
  });

  it("says when nothing matches", async () => {
    fakeApi(routes({ [INCIDENTS]: { body: page([]) } }));
    renderApp(PAGE_URL);
    expect(
      await screen.findByText("No incident matches this filter."),
    ).toBeInTheDocument();
  });

  it("shows an error with a retry when the list cannot load", async () => {
    let failing = true;
    fakeApi(
      routes({
        [INCIDENTS]: () =>
          failing
            ? { status: 500, body: { code: "internal", params: {} } }
            : { body: page(listed) },
      }),
    );
    renderApp(PAGE_URL);
    const alert = await screen.findByRole("alert");
    expect(
      within(alert).getByText("Could not load the incidents"),
    ).toBeVisible();
    failing = false;
    await userEvent.click(
      within(alert).getByRole("button", { name: "Try again" }),
    );
    expect(
      await screen.findByRole("link", { name: "Queue depth growing" }),
    ).toBeVisible();
  });

  it("shows a missing incident as an error", async () => {
    fakeApi(
      routes({
        [incidentUrl(INCIDENT_ID)]: () => ({
          status: 404,
          body: { code: "not_found", params: {} },
        }),
      }),
    );
    renderApp(`${PAGE_URL}?incident=${INCIDENT_ID}`);
    expect(
      await screen.findByText("Could not load the incident"),
    ).toBeVisible();
  });
});
