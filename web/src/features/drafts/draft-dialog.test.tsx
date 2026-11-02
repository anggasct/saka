import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import {
  FILES,
  fileTree,
  PROJECTS,
  SPECS,
  WORK_ITEMS,
} from "@/test/docs-fixtures";
import { DRAFTS, draft, OTHER_DRAFT_ID } from "@/test/draft-fixtures";
import { fakeApi, page, type Recorded, type Routes } from "@/test/fake-api";
import { project, PROJECT_ID } from "@/test/fixtures";
import { renderApp } from "@/test/render";

const DOCS = `/docs/${PROJECT_ID}`;

function problem(status: number, code: string, params: object = {}) {
  return {
    status,
    body: { type: "", title: code, status, code, params, correlation_id: null },
  };
}

function routes(extra: Routes = {}): Routes {
  return {
    [PROJECTS]: { body: page([project]) },
    [FILES]: { body: fileTree() },
    [SPECS]: { body: page([]) },
    [WORK_ITEMS]: { body: page([]) },
    [DRAFTS]: { body: page([]) },
    ...extra,
  };
}

function posts(requests: Recorded[]): Recorded[] {
  return requests.filter((r) => r.method === "POST" && r.path === DRAFTS);
}

async function openDialog(user: ReturnType<typeof userEvent.setup>) {
  const tree = await screen.findByRole("navigation", { name: "Files" });
  await user.click(
    await within(tree).findByRole("button", { name: "Draft with architect" }),
  );
  return screen.findByRole("dialog", {
    name: "Draft a spec with the architect",
  });
}

describe("Draft with architect dialog", () => {
  it("sits in the tree toolbar next to New spec with the active draft count", async () => {
    fakeApi(routes({ [DRAFTS]: { body: page([draft()]) } }));
    renderApp(DOCS);
    const tree = await screen.findByRole("navigation", { name: "Files" });
    const buttons = await within(tree).findAllByRole("button", {
      name: /New spec|Draft with architect/,
    });
    expect(buttons.map((b) => b.textContent)).toEqual([
      "New spec",
      "Draft with architect",
    ]);
    expect(
      await within(tree).findByRole("link", { name: "Drafts (1)" }),
    ).toHaveAttribute("href", `${DOCS}/drafts`);
  });

  it("checks the idea, the path, and the context before sending", async () => {
    const many = Array.from({ length: 17 }, (_, i) => ({
      path: `docs/notes/n${String(i).padStart(2, "0")}.md`,
      blob: "a".repeat(40),
      bytes: 10,
    }));
    const api = fakeApi(
      routes({
        [FILES]: {
          body: fileTree({ files: [...fileTree().files, ...many] }),
        },
      }),
    );
    const user = userEvent.setup();
    renderApp(DOCS);
    const dialog = await openDialog(user);
    const submit = within(dialog).getByRole("button", {
      name: "Start drafting",
    });
    const path = within(dialog).getByRole("textbox", { name: "Path" });
    expect(path).toHaveValue("docs/specs/");

    await user.click(submit);
    expect(within(dialog).getByText("Describe the idea.")).toBeInTheDocument();
    expect(within(dialog).getByText("Enter a file name.")).toBeInTheDocument();

    await user.type(
      within(dialog).getByRole("textbox", { name: "Idea" }),
      "Avatars",
    );
    expect(
      within(dialog).getByText("7,993 characters left"),
    ).toBeInTheDocument();
    await user.type(path, "tasks-done");
    await user.click(submit);
    expect(
      within(dialog).getByText("docs/specs/tasks-done.md already exists."),
    ).toBeInTheDocument();

    await user.clear(path);
    await user.type(path, "docs/specs/avatar-upload");
    const boxes = within(dialog).getAllByRole("checkbox");
    for (const box of boxes.slice(0, 17)) {
      await user.click(box);
    }
    expect(within(dialog).getByText("17 of 16 selected")).toBeInTheDocument();
    await user.click(submit);
    expect(
      within(dialog).getByText("Choose at most 16 context files."),
    ).toBeInTheDocument();
    expect(posts(api.requests)).toEqual([]);
  });

  it("sends the draft with an idempotency key and opens its page", async () => {
    let failOnce = true;
    const api = fakeApi(
      routes({
        [DRAFTS]: (request: Recorded) => {
          if (request.method !== "POST") {
            return { body: page([]) };
          }
          if (failOnce) {
            failOnce = false;
            return Promise.reject(new TypeError("offline"));
          }
          return {
            status: 201,
            body: draft({ state: "queued", attempt: null }),
          };
        },
        [`/api/v1/spec-drafts/${draft().id}`]: { body: draft() },
      }),
    );
    const user = userEvent.setup();
    const { router } = renderApp(DOCS);
    const dialog = await openDialog(user);
    await user.type(
      within(dialog).getByRole("textbox", { name: "Idea" }),
      "Let users upload an avatar.",
    );
    await user.type(
      within(dialog).getByRole("textbox", { name: "Path" }),
      "avatar-upload",
    );
    await user.click(
      within(dialog).getByRole("checkbox", { name: "docs/architecture.md" }),
    );
    await user.selectOptions(
      within(dialog).getByRole("combobox", { name: "Language" }),
      "id",
    );
    const submit = within(dialog).getByRole("button", {
      name: "Start drafting",
    });
    await user.click(submit);
    expect(
      await within(dialog).findByText("The server could not be reached."),
    ).toBeInTheDocument();
    await user.click(submit);

    await waitFor(() => {
      expect(router.state.location.pathname).toBe(
        `${DOCS}/drafts/${draft().id}`,
      );
    });
    const sent = posts(api.requests);
    expect(sent).toHaveLength(2);
    expect(sent[1]?.body).toEqual({
      idea: "Let users upload an avatar.",
      path: "docs/specs/avatar-upload.md",
      context_paths: ["docs/architecture.md"],
      locale: "id",
    });
    const key = sent[1]?.headers.get("Idempotency-Key") ?? "";
    expect(key).not.toBe("");
    expect(sent[0]?.headers.get("Idempotency-Key")).toBe(key);
  });

  it("defaults the language to the UI locale and uses a new key per intent", async () => {
    const api = fakeApi(
      routes({
        [DRAFTS]: (request: Recorded) =>
          request.method === "POST"
            ? problem(422, "validation_failed", {
                errors: [{ field: "idea", code: "too_long" }],
              })
            : { body: page([]) },
      }),
    );
    const user = userEvent.setup();
    renderApp(DOCS);
    const dialog = await openDialog(user);
    expect(
      within(dialog).getByRole("combobox", { name: "Language" }),
    ).toHaveValue("en");
    await user.type(
      within(dialog).getByRole("textbox", { name: "Idea" }),
      "Avatars",
    );
    await user.type(
      within(dialog).getByRole("textbox", { name: "Path" }),
      "avatars",
    );
    const submit = within(dialog).getByRole("button", {
      name: "Start drafting",
    });
    await user.click(submit);
    expect(
      await within(dialog).findByText(
        "Some input was refused (idea: too_long).",
      ),
    ).toBeInTheDocument();
    await user.click(submit);
    await waitFor(() => {
      expect(posts(api.requests)).toHaveLength(2);
    });
    const [first, second] = posts(api.requests);
    expect(first?.headers.get("Idempotency-Key")).not.toBe(
      second?.headers.get("Idempotency-Key"),
    );
  });

  const TAKEN = [
    [
      "file_exists",
      { blob: "b".repeat(40), commit: "c".repeat(40) },
      "docs/specs/avatar-upload.md already exists on the base branch.",
    ],
    [
      "spec_registered",
      { spec_id: "s" },
      "A spec is already registered at docs/specs/avatar-upload.md.",
    ],
    [
      "slug_taken",
      { spec_id: "s" },
      "Another spec already uses the slug of docs/specs/avatar-upload.md.",
    ],
    [
      "draft_active",
      { spec_draft_id: OTHER_DRAFT_ID },
      "Another draft for docs/specs/avatar-upload.md is still open.",
    ],
  ] as const;

  for (const [reason, params, sentence] of TAKEN) {
    it(`explains a taken target: ${reason}`, async () => {
      fakeApi(
        routes({
          [DRAFTS]: (request: Recorded) =>
            request.method === "POST"
              ? problem(409, "spec_draft_target_taken", {
                  path: "docs/specs/avatar-upload.md",
                  reason,
                  ...params,
                })
              : { body: page([]) },
        }),
      );
      const user = userEvent.setup();
      renderApp(DOCS);
      const dialog = await openDialog(user);
      await user.type(
        within(dialog).getByRole("textbox", { name: "Idea" }),
        "Avatars",
      );
      await user.type(
        within(dialog).getByRole("textbox", { name: "Path" }),
        "avatar-upload",
      );
      await user.click(
        within(dialog).getByRole("button", { name: "Start drafting" }),
      );
      expect(await within(dialog).findByText(sentence)).toBeInTheDocument();
      const link = within(dialog).queryByRole("link", {
        name: "Open that draft",
      });
      if (reason === "draft_active") {
        expect(link).toHaveAttribute(
          "href",
          `${DOCS}/drafts/${OTHER_DRAFT_ID}`,
        );
      } else {
        expect(link).toBeNull();
      }
    });
  }
});
