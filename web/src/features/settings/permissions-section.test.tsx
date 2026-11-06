import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { AUTHORSHIP, botAuthorship, LINK } from "@/test/authorship-fixtures";
import { DIAGNOSTICS, report } from "@/test/diagnostics-fixtures";
import { fakeApi, page } from "@/test/fake-api";
import type { Routes } from "@/test/fake-api";
import { project, PROJECT_ID } from "@/test/fixtures";
import { HOOKS, HOOK_RUNS } from "@/test/hooks-fixtures";
import {
  allowRule,
  denyRule,
  historyItem,
  operatorRule,
  PREVIEW,
  REQUESTS,
  RULES,
} from "@/test/permission-fixtures";
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
    [RULES]: {
      body: { items: [denyRule, allowRule], operator_rules: [operatorRule] },
    },
    [REQUESTS]: {
      body: page([
        historyItem(1, "human:operator:local", "npm install"),
        historyItem(2, `policy:project:${allowRule.id}`, "git status"),
      ]),
    },
    ...extra,
  };
}

async function section() {
  return within(await screen.findByRole("region", { name: "Permissions" }));
}

async function openForm() {
  const permissions = await section();
  await userEvent.click(
    await permissions.findByRole("button", { name: "Add a rule" }),
  );
  return within(await screen.findByRole("dialog"));
}

describe("Permissions", () => {
  it("lists the operator rules as read-only text and the project rules with a revoke button each", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    const permissions = await section();
    expect(await permissions.findByText("safe-commands")).toBeVisible();
    expect(permissions.getByText("go test * ls*")).toBeVisible();
    expect(
      permissions.getByText(
        "Set in the operator's config file. They are read-only here.",
      ),
    ).toBeVisible();
    expect(permissions.getByText("rm *")).toBeVisible();
    expect(permissions.getByText("never delete")).toBeVisible();
    expect(permissions.getAllByText("git status")[0]).toBeVisible();
    expect(
      permissions.getByRole("button", { name: "Revoke rm *" }),
    ).toBeVisible();
    expect(
      permissions.getByRole("button", { name: "Revoke git status" }),
    ).toBeVisible();
    expect(
      permissions.queryByRole("button", { name: /safe-commands/ }),
    ).toBeNull();
  });

  it("asks for a reason before it revokes a rule", async () => {
    const { requests } = fakeApi(
      routes({
        [`/api/v1/permission-rules/${denyRule.id}/revoke`]: {
          body: { ...denyRule, revoked_at: "2026-10-31T11:00:00.000Z" },
        },
      }),
    );
    renderApp(SETTINGS);
    const permissions = await section();
    await userEvent.click(
      await permissions.findByRole("button", { name: "Revoke rm *" }),
    );
    const dialog = within(await screen.findByRole("dialog"));
    const submit = dialog.getByRole("button", { name: "Revoke rule" });
    expect(submit).toBeDisabled();
    await userEvent.type(dialog.getByLabelText("Reason"), "too strict");
    await userEvent.click(submit);
    await waitFor(() => {
      expect(requests.some((r) => r.method === "POST")).toBe(true);
    });
    const post = requests.find((r) => r.method === "POST");
    expect(post?.body).toEqual({ reason: "too strict" });
    expect(post?.headers.get("idempotency-key")).toBeTruthy();
  });

  it("saves a rule only after the current values were previewed", async () => {
    const { requests } = fakeApi(
      routes({
        [PREVIEW]: {
          body: {
            scanned: 12,
            changed: [
              {
                permission_request_id: "0193f0c4-1b2a-7c3d-9e8f-001122330001",
                tool: "Bash",
                command: "npm install",
                before: "ask",
                after: "allow",
              },
            ],
          },
        },
        [RULES]: (request) =>
          request.method === "POST"
            ? { status: 201, body: allowRule }
            : {
                body: {
                  items: [denyRule],
                  operator_rules: [operatorRule],
                },
              },
      }),
    );
    renderApp(SETTINGS);
    const form = await openForm();
    const save = form.getByRole("button", { name: "Save rule" });
    expect(save).toBeDisabled();
    expect(form.getByText("Preview the rule before saving it.")).toBeVisible();
    await userEvent.selectOptions(form.getByLabelText("Match"), "glob");
    await userEvent.type(form.getByLabelText("Command"), "npm *");
    await userEvent.click(form.getByRole("button", { name: "Preview" }));
    expect(
      await form.findByText("1 of the last 12 requests would change."),
    ).toBeVisible();
    expect(form.getByText("npm install")).toBeVisible();
    expect(form.getByText("Ask becomes Allow")).toBeVisible();
    expect(save).toBeEnabled();

    await userEvent.type(form.getByLabelText("Command"), "x");
    expect(save).toBeDisabled();
    expect(form.getByText("Preview the rule before saving it.")).toBeVisible();
    await userEvent.click(form.getByRole("button", { name: "Preview" }));
    await waitFor(() => {
      expect(save).toBeEnabled();
    });
    await userEvent.click(save);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    const post = requests.find((r) => r.method === "POST" && r.path === RULES);
    expect(post?.body).toEqual({
      effect: "allow",
      tool: "Bash",
      match: "glob",
      command: "npm *x",
      path: null,
      note: null,
    });
    expect(post?.headers.get("idempotency-key")).toBeTruthy();
  });

  it("explains a refused rule and keeps save off", async () => {
    fakeApi(
      routes({
        [PREVIEW]: problemBody(422, "rule_too_broad"),
      }),
    );
    renderApp(SETTINGS);
    const form = await openForm();
    await userEvent.type(form.getByLabelText("Command"), "*");
    await userEvent.click(form.getByRole("button", { name: "Preview" }));
    expect(await form.findByRole("alert")).toHaveTextContent(
      "An allow rule cannot match every command",
    );
    expect(form.getByRole("button", { name: "Save rule" })).toBeDisabled();
  });

  it("asks for a path instead of a command for a file tool", async () => {
    const { requests } = fakeApi(
      routes({ [PREVIEW]: { body: { scanned: 0, changed: [] } } }),
    );
    renderApp(SETTINGS);
    const form = await openForm();
    await userEvent.clear(form.getByLabelText("Tool"));
    await userEvent.type(form.getByLabelText("Tool"), "Edit");
    expect(form.queryByLabelText("Command")).toBeNull();
    await userEvent.type(form.getByLabelText("Path (optional)"), "docs/**");
    await userEvent.click(form.getByRole("button", { name: "Preview" }));
    expect(
      await form.findByText("None of the last 0 requests would change."),
    ).toBeVisible();
    const post = requests.find((r) => r.path === PREVIEW);
    expect(post?.body).toMatchObject({
      tool: "Edit",
      command: null,
      path: "docs/**",
    });
  });

  it("shows who answered each request and filters by it", async () => {
    const { requests } = fakeApi(
      routes({
        [REQUESTS]: (request) => ({
          body: page(
            request.search.get("source") === "policy"
              ? [historyItem(2, `policy:project:${allowRule.id}`, "git status")]
              : [
                  historyItem(1, "human:operator:local", "npm install"),
                  historyItem(2, "policy:safe-commands", "ls"),
                  historyItem(3, null, "make deploy"),
                ],
          ),
        }),
      }),
    );
    renderApp(SETTINGS);
    const permissions = await section();
    expect(await permissions.findByText("npm install")).toBeVisible();
    expect(permissions.getByText("Answered by you")).toBeVisible();
    expect(permissions.getByText("Operator rule safe-commands")).toBeVisible();
    expect(permissions.getByText("Waiting for an answer")).toBeVisible();
    await userEvent.click(permissions.getByRole("button", { name: "A rule" }));
    expect(await permissions.findByText("A project rule")).toBeVisible();
    expect(permissions.queryByText("npm install")).toBeNull();
    expect(requests.some((r) => r.search.get("source") === "policy")).toBe(
      true,
    );
  });

  it("pages the history with the cursor", async () => {
    const { requests } = fakeApi(
      routes({
        [REQUESTS]: (request) =>
          request.search.get("cursor") === "next"
            ? { body: page([historyItem(2, "human:operator:local", "second")]) }
            : {
                body: {
                  items: [historyItem(1, "human:operator:local", "first")],
                  next_cursor: "next",
                },
              },
      }),
    );
    renderApp(SETTINGS);
    const permissions = await section();
    expect(await permissions.findByText("first")).toBeVisible();
    await userEvent.click(
      permissions.getByRole("button", { name: "Show more" }),
    );
    expect(await permissions.findByText("second")).toBeVisible();
    expect(permissions.getByText("first")).toBeVisible();
    expect(permissions.queryByRole("button", { name: "Show more" })).toBeNull();
    expect(requests.filter((r) => r.path === REQUESTS)).toHaveLength(2);
  });

  it("says so when nothing is there yet", async () => {
    fakeApi(
      routes({
        [RULES]: { body: { items: [], operator_rules: [] } },
        [REQUESTS]: { body: page([]) },
      }),
    );
    renderApp(SETTINGS);
    const permissions = await section();
    expect(
      await permissions.findByText("The operator has no permission rules."),
    ).toBeVisible();
    expect(
      permissions.getByText(/This project has no rules yet/),
    ).toBeVisible();
    expect(
      permissions.getByText("No permission request matches."),
    ).toBeVisible();
  });
});
