import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { AUTHORSHIP, botAuthorship, LINK } from "@/test/authorship-fixtures";
import { fakeApi, page } from "@/test/fake-api";
import type { Routes } from "@/test/fake-api";
import { project, PROJECT_ID, spec } from "@/test/fixtures";
import { HOOK_RUNS, HOOKS } from "@/test/hooks-fixtures";
import { renderApp } from "@/test/render";
import {
  invalidConfig,
  OTHER_PROJECT_ID,
  PROJECT,
  PROJECTS,
  problemBody,
  RELOAD,
  repoInUse,
  SECRET_ITEMS,
  SECRETS,
  twoRepos,
} from "@/test/settings-fixtures";

const SETTINGS = `/settings/${PROJECT_ID}`;

function routes(extra: Routes = {}): Routes {
  return {
    [PROJECTS]: { body: page([project]) },
    [PROJECT]: { body: project },
    [SECRETS]: { body: { items: [] } },
    [HOOKS]: { body: { items: [] } },
    [HOOK_RUNS]: { body: page([]) },
    [AUTHORSHIP]: { body: botAuthorship },
    [LINK]: problemBody(404, "user_link_not_found"),
    ...extra,
  };
}

describe("Settings navigation", () => {
  it("redirects to the only project", async () => {
    fakeApi(routes());
    const { router } = renderApp("/settings");
    expect(
      await screen.findByRole("heading", { name: "Settings of tasks" }),
    ).toBeInTheDocument();
    expect(router.state.location.pathname).toBe(SETTINGS);
  });

  it("lists several projects as links", async () => {
    fakeApi(
      routes({
        [PROJECTS]: {
          body: page([
            project,
            { ...project, id: OTHER_PROJECT_ID, name: "shop" },
          ]),
        },
      }),
    );
    renderApp("/settings");
    const link = await screen.findByRole("link", { name: /^shop/ });
    expect(link).toHaveAttribute("href", `/settings/${OTHER_PROJECT_ID}`);
    expect(screen.getByRole("link", { name: /^tasks/ })).toHaveAttribute(
      "href",
      SETTINGS,
    );
  });

  it("tells how to register a project when there is none", async () => {
    fakeApi(routes({ [PROJECTS]: { body: page([]) } }));
    renderApp("/settings");
    expect(await screen.findByText("No projects yet")).toBeInTheDocument();
    expect(screen.getByText("saka init")).toBeInTheDocument();
  });

  it("says a project is not found and links back", async () => {
    fakeApi(routes({ [PROJECT]: problemBody(404, "not_found") }));
    renderApp(SETTINGS);
    expect(await screen.findByText("Project not found")).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Back to settings" }),
    ).toHaveAttribute("href", "/settings");
  });

  it("shows an error card whose retry asks again", async () => {
    let fail = true;
    const { calls } = fakeApi(
      routes({
        [PROJECT]: () =>
          fail ? problemBody(500, "internal") : { body: project },
      }),
    );
    renderApp(SETTINGS);
    const alert = await screen.findByRole("alert");
    expect(
      within(alert).getByText("Something went wrong on the server."),
    ).toBeInTheDocument();
    fail = false;
    await userEvent.click(
      within(alert).getByRole("button", { name: "Try again" }),
    );
    expect(
      await screen.findByRole("heading", { name: "Settings of tasks" }),
    ).toBeInTheDocument();
    expect(calls.filter((c) => c === PROJECT)).toHaveLength(2);
  });

  it("puts Settings after Delivery in the header", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    const nav = await screen.findByRole("navigation");
    const links = within(nav)
      .getAllByRole("link")
      .map((l) => l.textContent);
    expect(links).toEqual(["Docs", "Delivery", "Settings"]);
  });

  it("links the Delivery project header to its settings", async () => {
    fakeApi({
      ...routes(),
      [`${PROJECT}/specs`]: { body: page([spec]) },
      "/api/v1/work-items": { body: page([]) },
      "/api/v1/permission-requests": { body: page([]) },
    });
    renderApp("/delivery");
    await waitFor(() => {
      const links = screen.getAllByRole("link", { name: "Settings" });
      expect(links.map((l) => l.getAttribute("href"))).toContain(SETTINGS);
    });
  });
});

describe("Repos and config", () => {
  it("shows the docs location, bindings, scopes, and warnings", async () => {
    fakeApi(routes({ [PROJECT]: { body: twoRepos } }));
    renderApp(SETTINGS);
    const repos = await screen.findByRole("region", { name: "Repos" });
    expect(within(repos).getByText("docs/saka.toml")).toBeInTheDocument();
    expect(
      within(repos).getByText(
        "Bindings come from docs/saka.toml on main. Edit that file and push, then reload.",
      ),
    ).toBeInTheDocument();
    expect(within(repos).getByText("default")).toBeInTheDocument();
    expect(within(repos).getAllByText("docs repo").length).toBeGreaterThan(0);
    expect(within(repos).getByText("develop")).toBeInTheDocument();
    expect(
      within(repos).getByRole("link", { name: "acme/web" }),
    ).toHaveAttribute("href", "https://github.com/acme/web");
    expect(within(repos).getAllByText("Effective path scope")).toHaveLength(1);
    expect(within(repos).getByText("Strict")).toBeInTheDocument();
    expect(
      within(repos).getByText(
        "The repo file asks for Autonomous; the operator allows Standard.",
      ),
    ).toBeInTheDocument();
    expect(
      within(repos).getByText(
        "Repo web overlaps shop of project storefront: both can change the same files.",
      ),
    ).toBeInTheDocument();
  });

  it("lists every issue of a rejected config with its line", async () => {
    fakeApi(routes({ [PROJECT]: { body: invalidConfig } }));
    renderApp(SETTINGS);
    const config = await screen.findByRole("region", { name: "Config" });
    expect(
      within(config).getByText(
        "Line 4: A required field is missing. (repo.url)",
      ),
    ).toBeInTheDocument();
    expect(
      within(config).getByText("Line 9: The file is not valid TOML."),
    ).toBeInTheDocument();
  });

  it("links the tasks that keep a removed repo", async () => {
    fakeApi(routes({ [PROJECT]: { body: repoInUse } }));
    renderApp(SETTINGS);
    const config = await screen.findByRole("region", { name: "Config" });
    expect(
      within(config).getByRole("link", { name: "tasks-done/s1" }),
    ).toHaveAttribute(
      "href",
      "/delivery/tasks/0193f0c4-1b2a-7c3d-9e8f-001122334501",
    );
  });

  it("reloads and reports the outcome once, refreshing the project", async () => {
    const { requests, calls } = fakeApi(
      routes({ [RELOAD]: { body: { project, outcome: "changed" } } }),
    );
    renderApp(SETTINGS);
    await userEvent.click(
      await screen.findByRole("button", { name: "Reload config" }),
    );
    expect(
      await screen.findByText("Config reloaded and applied."),
    ).toBeInTheDocument();
    const post = requests.find((r) => r.path === RELOAD);
    expect(post?.method).toBe("POST");
    expect(post?.headers.get("Idempotency-Key")).toBeTruthy();
    await waitFor(() => {
      expect(calls.filter((c) => c === PROJECT)).toHaveLength(2);
    });
  });

  it("reports an unchanged reload", async () => {
    fakeApi(routes({ [RELOAD]: { body: { project, outcome: "unchanged" } } }));
    renderApp(SETTINGS);
    await userEvent.click(
      await screen.findByRole("button", { name: "Reload config" }),
    );
    expect(
      await screen.findByText("Config reloaded; nothing changed."),
    ).toBeInTheDocument();
  });

  it("shows the issues of a rejected reload", async () => {
    fakeApi(
      routes({
        [RELOAD]: problemBody(422, "project_config_invalid", {
          issues: [{ code: "field_invalid", field: "repo.url", line: 3 }],
        }),
      }),
    );
    renderApp(SETTINGS);
    await userEvent.click(
      await screen.findByRole("button", { name: "Reload config" }),
    );
    const alert = await screen.findByRole("alert");
    expect(
      within(alert).getByText(
        "Line 3: A field has an invalid value. (repo.url)",
      ),
    ).toBeInTheDocument();
  });
});

describe("Workflow", () => {
  it("shows the full loop with its stages and no controls", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    const workflow = await screen.findByRole("region", { name: "Workflow" });
    expect(
      within(workflow).getByText("Full loop: spec to merged pull request"),
    ).toBeInTheDocument();
    expect(within(workflow).getByText("Approved specs")).toBeInTheDocument();
    expect(
      within(workflow).getByText("Development: automatic"),
    ).toBeInTheDocument();
    expect(within(workflow).getByText("Merge: on command")).toBeInTheDocument();
    expect(
      within(workflow).queryByRole("term", { name: "Labels" }),
    ).not.toBeInTheDocument();
    expect(
      workflow.querySelectorAll("input, select, textarea, button"),
    ).toHaveLength(0);
  });

  it("shows the entry point and the filters of a review-only project", async () => {
    const reviewOnly = {
      ...project,
      workflow: {
        preset: "review_only",
        spec: false,
        change_request: {
          trigger: "manual",
          labels: ["needs-review"],
          authors: [],
          branches: ["feature/**"],
          paths: [],
          include_forks: false,
          include_drafts: true,
          trigger_label: "saka:review",
        },
        stages: [{ stage: "review", trigger: "auto" }],
        fix_rounds: 2,
      },
    };
    fakeApi(routes({ [PROJECT]: { body: reviewOnly } }));
    renderApp(SETTINGS);
    const workflow = await screen.findByRole("region", { name: "Workflow" });
    expect(
      within(workflow).getByText(
        "Review only: pull requests opened outside Saka get a review",
      ),
    ).toBeInTheDocument();
    expect(
      within(workflow).getByText(
        "Pull requests opened outside Saka (on command)",
      ),
    ).toBeInTheDocument();
    expect(within(workflow).queryByText("Approved specs")).toBeNull();
    expect(within(workflow).getByText("Review: automatic")).toBeInTheDocument();
    const filters = within(workflow).getByLabelText(
      "Filters for outside pull requests",
    );
    expect(within(filters).getByText("needs-review")).toBeInTheDocument();
    expect(within(filters).getByText("feature/**")).toBeInTheDocument();
    expect(within(filters).getByText("saka:review")).toBeInTheDocument();
    expect(within(filters).getAllByText("Any")).toHaveLength(2);
    expect(within(filters).getByText("Skipped")).toBeInTheDocument();
    expect(within(filters).getByText("Included")).toBeInTheDocument();
  });

  it("explains a workflow problem in saka.toml with its line", async () => {
    fakeApi(
      routes({
        [RELOAD]: problemBody(422, "project_config_invalid", {
          issues: [
            { code: "workflow_spec_without_dev", field: "workflow", line: 9 },
          ],
        }),
      }),
    );
    renderApp(SETTINGS);
    await userEvent.click(
      await screen.findByRole("button", { name: "Reload config" }),
    );
    const alert = await screen.findByRole("alert");
    expect(
      within(alert).getByText(
        "Line 9: The workflow takes specs but has no development stage. (workflow)",
      ),
    ).toBeInTheDocument();
  });
});

describe("Secrets", () => {
  it("lists names and shows the empty state", async () => {
    fakeApi(routes({ [SECRETS]: { body: { items: SECRET_ITEMS } } }));
    const first = renderApp(SETTINGS);
    const section = await screen.findByRole("region", { name: "Secrets" });
    expect(await within(section).findByText("NPM_TOKEN")).toBeInTheDocument();
    expect(within(section).getByText("API_KEY")).toBeInTheDocument();
    first.unmount();
    fakeApi(routes());
    renderApp(SETTINGS);
    expect(
      await screen.findByText(/No secrets\. A recipe asks for secrets/),
    ).toBeInTheDocument();
  });

  it("refuses a lower-case name before sending", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    await userEvent.click(
      await screen.findByRole("button", { name: "Add secret" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Name"), "npm_token");
    await userEvent.type(
      within(dialog).getByLabelText("Value"),
      "s3cret-value",
    );
    expect(within(dialog).getByRole("button", { name: "Save" })).toBeDisabled();
    expect(within(dialog).getByText(/Use capital letters/)).toBeInTheDocument();
  });

  it("stores a secret write-only and keeps the value out of the DOM and cache", async () => {
    let items = SECRET_ITEMS.slice(0, 1);
    const { requests } = fakeApi(
      routes({
        [SECRETS]: () => ({ body: { items } }),
        [`${SECRETS}/NPM_TOKEN`]: () => {
          items = SECRET_ITEMS;
          return { status: 204, body: undefined };
        },
      }),
    );
    const { queryClient, container } = renderApp(SETTINGS);
    await userEvent.click(
      await screen.findByRole("button", { name: "Add secret" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Name"), "NPM_TOKEN");
    const value = within(dialog).getByLabelText("Value");
    expect(value).toHaveAttribute("type", "password");
    expect(value).toHaveAttribute("autocomplete", "off");
    await userEvent.type(value, "s3cret-value");
    await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const put = requests.find((r) => r.method === "PUT");
    expect(put?.path).toBe(`${SECRETS}/NPM_TOKEN`);
    expect(put?.body).toEqual({ value: "s3cret-value" });
    expect(put?.headers.get("Idempotency-Key")).toBeTruthy();
    expect(await screen.findByText("NPM_TOKEN")).toBeInTheDocument();
    expect(container.ownerDocument.body.innerHTML).not.toContain(
      "s3cret-value",
    );
    const cache = JSON.stringify(
      queryClient
        .getQueryCache()
        .getAll()
        .map((q) => [q.queryKey, q.state.data]),
    );
    expect(cache).not.toContain("s3cret-value");
    expect(
      JSON.stringify(queryClient.getMutationCache().getAll()),
    ).not.toContain("s3cret-value");
  });

  it.each([
    [
      problemBody(422, "secret_too_short"),
      "The value must be at least 8 bytes.",
    ],
    [
      problemBody(422, "secret_name_invalid"),
      "This name is not allowed. Use capital letters, digits and underscores, and not a name Saka sets itself.",
    ],
    [
      problemBody(422, "validation_failed", {
        errors: [{ field: "value", code: "too_long" }],
      }),
      "The value is too long (65,536 bytes at most).",
    ],
    [
      problemBody(422, "validation_failed", {
        errors: [{ field: "value", code: "nul_byte" }],
      }),
      "The value cannot contain a NUL character.",
    ],
    [
      problemBody(422, "validation_failed", {
        errors: [{ field: "name", code: "too_many_secrets" }],
      }),
      "A project can hold 200 secrets at most.",
    ],
  ])("explains a refused secret (%#)", async (failure, sentence) => {
    fakeApi(routes({ [`${SECRETS}/NPM_TOKEN`]: failure }));
    renderApp(SETTINGS);
    await userEvent.click(
      await screen.findByRole("button", { name: "Add secret" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Name"), "NPM_TOKEN");
    await userEvent.type(within(dialog).getByLabelText("Value"), "short");
    await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      sentence,
    );
    expect(within(dialog).getByLabelText("Value")).toHaveValue("short");
  });

  it("switches to a multi-line value", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    await userEvent.click(
      await screen.findByRole("button", { name: "Add secret" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.click(within(dialog).getByLabelText("Multi-line value"));
    expect(within(dialog).getByLabelText("Value").tagName).toBe("TEXTAREA");
  });

  it("locks the name when replacing", async () => {
    const { requests } = fakeApi(
      routes({
        [SECRETS]: { body: { items: SECRET_ITEMS } },
        [`${SECRETS}/API_KEY`]: { status: 204, body: undefined },
      }),
    );
    renderApp(SETTINGS);
    await userEvent.click(
      await screen.findByRole("button", { name: "Replace API_KEY" }),
    );
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByLabelText("Name")).toHaveAttribute("readonly");
    await userEvent.type(
      within(dialog).getByLabelText("Value"),
      "another-value",
    );
    await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => {
      expect(requests.some((r) => r.path === `${SECRETS}/API_KEY`)).toBe(true);
    });
  });

  it("asks before removing a secret", async () => {
    const { requests } = fakeApi(
      routes({
        [SECRETS]: { body: { items: SECRET_ITEMS } },
        [`${SECRETS}/API_KEY`]: { status: 204, body: undefined },
      }),
    );
    renderApp(SETTINGS);
    await userEvent.click(
      await screen.findByRole("button", { name: "Remove API_KEY" }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: "Remove API_KEY?",
    });
    expect(requests.some((r) => r.method === "DELETE")).toBe(false);
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Remove" }),
    );
    await waitFor(() => {
      expect(requests.some((r) => r.method === "DELETE")).toBe(true);
    });
    const del = requests.find((r) => r.method === "DELETE");
    expect(del?.path).toBe(`${SECRETS}/API_KEY`);
    expect(del?.headers.get("Idempotency-Key")).toBeTruthy();
  });

  it("shows an error card for a failed list", async () => {
    fakeApi(routes({ [SECRETS]: problemBody(500, "internal") }));
    renderApp(SETTINGS);
    expect(
      await screen.findByText("Secrets could not be loaded"),
    ).toBeInTheDocument();
  });
});
