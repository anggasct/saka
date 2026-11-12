import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import {
  FILE,
  FILES,
  fileContent,
  fileTree,
  PROJECTS,
  SPECS,
  WORK_ITEMS,
} from "@/test/docs-fixtures";
import {
  fakeApi,
  page,
  type Recorded,
  type Route,
  type Routes,
} from "@/test/fake-api";
import { project, PROJECT_ID } from "@/test/fixtures";
import { renderApp } from "@/test/render";

const DOCS = `/docs/${PROJECT_ID}`;
const QUICK = `/api/v1/projects/${PROJECT_ID}/quick-specs`;
const PATH = "docs/specs/bugfix-login-redirect-loop.md";

function problemBody(status: number, code: string, params: object = {}) {
  return {
    status,
    body: { type: "", title: code, status, code, params, correlation_id: null },
  };
}

function routes(extra: Routes = {}): Routes {
  return {
    [PROJECTS]: { body: page([project]) },
    [FILES]: { body: fileTree() },
    [FILE]: (request: Recorded): Route => {
      const path = request.search.get("path") ?? "";
      return path === PATH
        ? { body: fileContent(path, "---\nsaka: spec\n---\n# Login\n") }
        : problemBody(404, "not_found");
    },
    [SPECS]: { body: page([]) },
    [WORK_ITEMS]: { body: page([]) },
    "/api/v1/permission-requests": { body: page([]) },
    ...extra,
  };
}

const created = {
  spec: {},
  path: PATH,
  commit_sha: "c0ffee".padEnd(40, "1"),
  work_items: [],
};

async function open() {
  const user = userEvent.setup();
  const rendered = renderApp(DOCS);
  await user.click(await screen.findByRole("button", { name: "Quick task" }));
  const dialog = await screen.findByRole("dialog", { name: "Quick task" });
  return { user, dialog, ...rendered };
}

async function fillBugfix(
  user: ReturnType<typeof userEvent.setup>,
  dialog: HTMLElement,
) {
  await user.click(within(dialog).getByRole("radio", { name: /Bugfix/ }));
  await user.type(
    within(dialog).getByRole("textbox", { name: "Title" }),
    "Login redirect loop",
  );
  await user.type(
    within(dialog).getByRole("textbox", { name: /^Done when/ }),
    "make test",
  );
  await user.type(
    within(dialog).getByRole("textbox", { name: /^Regression/ }),
    "cargo test --test login",
  );
}

describe("Quick task dialog", () => {
  it("shows the file Saka will write as the form is filled in", async () => {
    fakeApi(routes());
    const { user, dialog } = await open();
    const preview = within(dialog).getByRole("region", { name: "Preview" });
    expect(
      within(preview).getByText(
        "Fill in the title and a command to see the file.",
      ),
    ).toBeInTheDocument();
    expect(
      within(dialog).queryByRole("textbox", { name: /^Regression/ }),
    ).toBeNull();

    await fillBugfix(user, dialog);
    expect(
      within(dialog).getByRole("textbox", { name: /^Regression/ }),
    ).toBeInTheDocument();
    await waitFor(() => {
      expect(within(preview).getByText(`File: ${PATH}`)).toBeInTheDocument();
    });
    const source = within(preview).getByRole<HTMLTextAreaElement>("textbox", {
      name: "Contents of the file",
    }).value;
    expect(source).toContain("kind: bugfix");
    expect(source).toContain("# Login redirect loop");
    expect(source).toContain("## Regression\n\n- `cargo test --test login`");
    expect(source).toContain("## Done when\n\n- `make test`");
  });

  it("creates and starts the task, sends the form once, and opens the spec", async () => {
    const api = fakeApi(routes({ [QUICK]: { status: 201, body: created } }));
    const { user, dialog, router } = await open();
    await fillBugfix(user, dialog);
    await user.click(
      within(dialog).getByRole("button", { name: "Create and start" }),
    );
    await waitFor(() => {
      expect(router.state.location.search).toEqual({ path: PATH });
    });
    const posts = api.requests.filter((r) => r.method === "POST");
    expect(posts).toHaveLength(1);
    expect(posts[0]?.headers.get("idempotency-key")).toBeTruthy();
    expect(posts[0]?.body).toEqual({
      kind: "bugfix",
      title: "Login redirect loop",
      description: "",
      done_when: ["make test"],
      regression: ["cargo test --test login"],
      ac: [],
      locale: "en",
      repo: null,
      approve: true,
    });
    expect(screen.queryByRole("dialog", { name: "Quick task" })).toBeNull();
  });

  it("creates only when asked to and asks the project repo only when there are several", async () => {
    const api = fakeApi(
      routes({
        [PROJECTS]: {
          body: page([
            {
              ...project,
              repos: [
                {
                  name: "shop",
                  is_default: true,
                  autonomy: { effective: "standard" },
                },
                {
                  name: "web",
                  is_default: false,
                  autonomy: { effective: "standard" },
                },
              ],
            },
          ]),
        },
        [QUICK]: { status: 201, body: created },
      }),
    );
    const { user, dialog } = await open();
    await user.type(
      within(dialog).getByRole("textbox", { name: "Title" }),
      "Bump deps",
    );
    await user.type(
      within(dialog).getByRole("textbox", { name: /^Done when/ }),
      "make test{Enter}make lint",
    );
    await user.selectOptions(
      within(dialog).getByRole("combobox", { name: "Repository" }),
      "web",
    );
    await user.click(
      within(dialog).getByRole("button", { name: "Create only" }),
    );
    await waitFor(() => {
      expect(api.requests.some((r) => r.method === "POST")).toBe(true);
    });
    expect(api.requests.find((r) => r.method === "POST")?.body).toMatchObject({
      kind: "feature",
      done_when: ["make test", "make lint"],
      repo: "web",
      approve: false,
    });
  });

  it("names every field that is wrong before anything is sent", async () => {
    const api = fakeApi(routes());
    const { user, dialog } = await open();
    await user.click(
      within(dialog).getByRole("button", { name: "Create and start" }),
    );
    expect(await within(dialog).findByText("Title is required.")).toBeVisible();
    expect(within(dialog).getByText("Done when is required.")).toBeVisible();
    expect(api.requests.some((r) => r.method === "POST")).toBe(false);

    await user.click(within(dialog).getByRole("radio", { name: /Bugfix/ }));
    await user.type(
      within(dialog).getByRole("textbox", { name: "Title" }),
      "Loop",
    );
    await user.click(
      within(dialog).getByRole("textbox", { name: /^Done when/ }),
    );
    await user.paste("x".repeat(501));
    await user.click(
      within(dialog).getByRole("button", { name: "Create and start" }),
    );
    expect(
      await within(dialog).findByText("Done when, line 1 is too long."),
    ).toBeVisible();
    expect(
      within(dialog).getByText(
        "A bugfix needs a Regression section with at least one command.",
      ),
    ).toBeVisible();
    expect(api.requests.some((r) => r.method === "POST")).toBe(false);
  });

  it("explains what the server refused and keeps the form", async () => {
    fakeApi(
      routes({
        [QUICK]: problemBody(409, "spec_slug_conflict", {
          slug: "bugfix-login-redirect-loop",
          path: PATH,
        }),
      }),
    );
    const { user, dialog } = await open();
    await fillBugfix(user, dialog);
    await user.click(
      within(dialog).getByRole("button", { name: "Create and start" }),
    );
    expect(
      await within(dialog).findByText(
        "another spec already uses the slug bugfix-login-redirect-loop.",
      ),
    ).toBeVisible();
    expect(within(dialog).getByRole("textbox", { name: "Title" })).toHaveValue(
      "Login redirect loop",
    );
  });

  it("says when the spec was written but could not be approved", async () => {
    fakeApi(
      routes({
        [QUICK]: problemBody(409, "quick_spec_not_approved", {
          code: "spec_not_ready",
          spec: { path: PATH },
        }),
      }),
    );
    const { user, dialog } = await open();
    await fillBugfix(user, dialog);
    await user.click(
      within(dialog).getByRole("button", { name: "Create and start" }),
    );
    expect(
      await within(dialog).findByText(
        `The spec was committed to ${PATH} but could not be approved (spec_not_ready). Fix it in Docs and approve it there.`,
      ),
    ).toBeVisible();
  });

  it("asks for the approval of the start when the task runs at the strict level", async () => {
    fakeApi(
      routes({
        [PROJECTS]: {
          body: page([
            {
              ...project,
              repos: [
                {
                  name: "shop",
                  is_default: true,
                  autonomy: { effective: "standard" },
                },
                {
                  name: "web",
                  is_default: false,
                  autonomy: { effective: "strict" },
                },
              ],
              workflow: {
                ...project.workflow,
                kinds: {
                  refactor: { stages: {}, autonomy: "strict", test_paths: [] },
                },
              },
            },
          ]),
        },
      }),
    );
    const { user, dialog } = await open();
    const start = () =>
      within(dialog).queryByRole("button", { name: "Create and start" });
    const approve = () =>
      within(dialog).queryByRole("button", { name: "Create and approve" });
    expect(start()).toBeInTheDocument();
    expect(approve()).toBeNull();

    await user.click(
      within(dialog).getByRole("radio", { name: /Security patch/ }),
    );
    expect(approve()).toBeInTheDocument();
    expect(start()).toBeNull();

    await user.click(within(dialog).getByRole("radio", { name: /Refactor/ }));
    expect(approve()).toBeInTheDocument();

    await user.click(within(dialog).getByRole("radio", { name: /Chore/ }));
    expect(start()).toBeInTheDocument();
    await user.selectOptions(
      within(dialog).getByRole("combobox", { name: "Repository" }),
      "web",
    );
    expect(approve()).toBeInTheDocument();
  });
});
