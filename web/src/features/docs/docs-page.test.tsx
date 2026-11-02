import { forEachDiagnostic } from "@codemirror/lint";
import { EditorView } from "@codemirror/view";
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { startEventStream } from "@/api/events";
import { platform } from "@/platform";
import {
  editorSpec,
  FILE,
  fileContent,
  FILES,
  fileTree,
  fileWrite,
  hashOf,
  NOTES_PATH,
  NOTES_TEXT,
  PROJECTS,
  SPEC_PATH,
  SPEC_TEXT,
  SPECS,
  stepTasks,
  TEMPLATE,
  WORK_ITEMS,
} from "@/test/docs-fixtures";
import {
  fakeApi,
  page,
  pending,
  type Recorded,
  type Route,
  type Routes,
} from "@/test/fake-api";
import { fakeStream } from "@/test/fake-stream";
import { project, PROJECT_ID } from "@/test/fixtures";
import { renderApp } from "@/test/render";

const DOCS = `/docs/${PROJECT_ID}`;

function problemBody(status: number, code: string, params: object = {}) {
  return {
    status,
    body: { type: "", title: code, status, code, params, correlation_id: null },
  };
}

function docsRoutes(
  files: Record<string, string> = {
    [NOTES_PATH]: NOTES_TEXT,
    [SPEC_PATH]: SPEC_TEXT,
  },
  extra: Routes = {},
): Routes {
  return {
    [PROJECTS]: { body: page([project]) },
    [FILES]: { body: fileTree() },
    [FILE]: (request: Recorded): Route => {
      const path = request.search.get("path") ?? "";
      const content = files[path];
      return content === undefined
        ? problemBody(404, "not_found")
        : { body: fileContent(path, content) };
    },
    [SPECS]: { body: page([]) },
    [WORK_ITEMS]: { body: page([]) },
    "/api/v1/permission-requests": { body: page([]) },
    ...extra,
  };
}

async function editorFor(path: string): Promise<EditorView> {
  const content = await screen.findByRole("textbox", {
    name: `Editor for ${path}`,
  });
  const view = EditorView.findFromDOM(content);
  if (view === null) {
    throw new Error("no editor view");
  }
  return view;
}

function type(view: EditorView, text: string, at = view.state.doc.length) {
  act(() => {
    view.dispatch({ changes: { from: at, insert: text } });
  });
}

function puts(requests: Recorded[]): Recorded[] {
  return requests.filter((r) => r.method === "PUT");
}

describe("Docs area", () => {
  it("goes straight to the only project", async () => {
    fakeApi(docsRoutes());
    const { router } = renderApp("/docs");
    await waitFor(() => {
      expect(router.state.location.pathname).toBe(DOCS);
    });
    expect(
      await screen.findByRole("heading", { name: "Choose a file" }),
    ).toBeInTheDocument();
  });

  it("lists several projects and says when there is none", async () => {
    const other = {
      ...project,
      id: "0193f0c4-1b2a-7c3d-9e8f-0011223344ff",
      name: "web",
    };
    fakeApi({ ...docsRoutes(), [PROJECTS]: { body: page([project, other]) } });
    const first = renderApp("/docs");
    const list = await screen.findByRole("heading", {
      name: "Choose a project",
    });
    expect(list).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /^web/ })).toHaveAttribute(
      "href",
      `/docs/${other.id}`,
    );
    first.unmount();

    fakeApi({ ...docsRoutes(), [PROJECTS]: { body: page([]) } });
    renderApp("/docs");
    expect(
      await screen.findByRole("heading", { name: "No projects yet" }),
    ).toBeInTheDocument();
  });

  it("shows the tree relative to the project folder", async () => {
    fakeApi({
      ...docsRoutes(),
      [FILES]: { body: fileTree({ truncated: true }) },
    });
    const user = userEvent.setup();
    renderApp(`${DOCS}?path=${SPEC_PATH}`);
    const tree = await screen.findByRole("navigation", { name: "Files" });
    const specs = await within(tree).findByRole("button", { name: "specs" });
    expect(specs).toHaveAttribute("aria-expanded", "true");
    const open = within(tree).getByRole("link", { name: "tasks-done.md" });
    expect(open).toHaveAttribute("aria-current", "page");
    expect(
      within(tree).getByRole("link", { name: "notes.md" }),
    ).not.toHaveAttribute("aria-current");
    const old = within(tree).getByRole("button", { name: "old" });
    expect(old).toHaveAttribute("aria-expanded", "false");
    expect(within(tree).queryByRole("link", { name: "legacy.md" })).toBeNull();
    await user.click(old);
    expect(
      within(tree).getByRole("link", { name: "legacy.md" }),
    ).toBeInTheDocument();
    await user.click(specs);
    expect(
      within(tree).queryByRole("link", { name: "tasks-done.md" }),
    ).toBeNull();
    expect(
      within(tree).getByText("Showing the first 4 files"),
    ).toBeInTheDocument();
  });

  it("explains every file state", async () => {
    fakeApi(
      docsRoutes(
        {},
        {
          [FILE]: (r: Recorded): Route => {
            const path = r.search.get("path");
            if (path === "docs/big.md") {
              return problemBody(422, "file_unsupported", {
                reason: "too_large",
                bytes: 2097152,
                max_bytes: 1048576,
              });
            }
            if (path === "docs/latin1.md") {
              return problemBody(422, "file_unsupported", {
                reason: "not_utf8",
              });
            }
            if (path === "docs/link.md") {
              return problemBody(422, "file_unsupported", {
                reason: "not_regular",
              });
            }
            if (path === "docs/broken.md") {
              return problemBody(502, "scm_unavailable");
            }
            return problemBody(404, "not_found");
          },
        },
      ),
    );
    const app = renderApp(`${DOCS}?path=docs/none.md`);
    expect(
      await screen.findByRole("heading", { name: "File not found" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("docs/none.md is not on main."),
    ).toBeInTheDocument();

    const go = async (path: string) => {
      await act(() =>
        app.router.navigate({
          to: "/docs/$projectId",
          params: { projectId: PROJECT_ID },
          search: { path },
        }),
      );
    };
    await go("docs/big.md");
    expect(
      await screen.findByText(
        "It is 2,097,152 bytes; the editor opens files up to 1,048,576 bytes.",
      ),
    ).toBeInTheDocument();
    await go("docs/latin1.md");
    expect(
      await screen.findByText("It is not UTF-8 text."),
    ).toBeInTheDocument();
    await go("docs/link.md");
    expect(
      await screen.findByText(
        "It is a link or a submodule, not a regular file.",
      ),
    ).toBeInTheDocument();
    await go("docs/broken.md");
    const alert = await screen.findByRole("alert");
    expect(
      within(alert).getByText("The code host did not answer. Try again."),
    ).toBeInTheDocument();
    expect(
      within(alert).getByRole("button", { name: "Try again" }),
    ).toBeInTheDocument();
  });

  it("shows a busy state while the file loads", async () => {
    fakeApi(docsRoutes({}, { [FILE]: pending }));
    renderApp(`${DOCS}?path=${NOTES_PATH}`);
    expect(
      await screen.findByRole("status", { name: "Loading the file" }),
    ).toHaveAttribute("aria-busy", "true");
  });

  it("outlines the headings and jumps to one", async () => {
    fakeApi(docsRoutes());
    const user = userEvent.setup();
    renderApp(`${DOCS}?path=${NOTES_PATH}`);
    const view = await editorFor(NOTES_PATH);
    const outline = screen.getByRole("navigation", { name: "Outline" });
    const items = within(outline).getAllByRole("button");
    expect(items.map((b) => [b.textContent, b.dataset.level])).toEqual([
      ["Notes", "1"],
      ["Plan", "2"],
      ["Details", "3"],
    ]);
    await user.click(within(outline).getByRole("button", { name: "Details" }));
    const line = view.state.doc.lineAt(view.state.selection.main.head);
    expect(line.text).toBe("### Details");
    expect(line.from).toBe(view.state.selection.main.head);
  });

  it("turns live preview off in source mode and remembers it", async () => {
    fakeApi(docsRoutes());
    const user = userEvent.setup();
    renderApp(`${DOCS}?path=${NOTES_PATH}`);
    const view = await editorFor(NOTES_PATH);
    expect(view.dom.querySelectorAll("input.cm-task")).toHaveLength(1);
    const source = screen.getByRole("button", { name: "Source" });
    expect(source).toHaveAttribute("aria-pressed", "false");
    await user.click(source);
    expect(source).toHaveAttribute("aria-pressed", "true");
    expect(view.dom.querySelectorAll("input.cm-task")).toHaveLength(0);
    expect(platform.preferences.get("docs.mode")).toBe("source");
    await user.click(source);
    expect(platform.preferences.get("docs.mode")).toBe("preview");
  });
});

describe("saving", () => {
  it("sends the exact content with the blob it was based on", async () => {
    const written = NOTES_TEXT + "more\n";
    const api = fakeApi(
      docsRoutes(undefined, {
        [FILE]: (r: Recorded): Route =>
          r.method === "PUT"
            ? { body: fileWrite(NOTES_PATH, written) }
            : { body: fileContent(NOTES_PATH, NOTES_TEXT) },
      }),
    );
    const user = userEvent.setup();
    renderApp(`${DOCS}?path=${NOTES_PATH}`);
    const view = await editorFor(NOTES_PATH);
    const save = screen.getByRole("button", { name: "Save" });
    expect(save).toBeDisabled();
    type(view, "more\n");
    expect(screen.getByRole("status")).toHaveTextContent("Unsaved changes");
    const treeCalls = api.calls.filter((c) => c === FILES).length;
    await user.click(save);
    expect(
      await screen.findByText(`Saved · ${"c0ffee".padEnd(12, "1")}`),
    ).toBeInTheDocument();
    const [put] = puts(api.requests);
    expect(put?.search.get("path")).toBe(NOTES_PATH);
    expect(put?.headers.get("if-match")).toBe(
      `"${fileContent(NOTES_PATH, NOTES_TEXT).blob}"`,
    );
    expect(put?.headers.get("idempotency-key")).toMatch(/^[0-9a-f-]{36}$/);
    expect(put?.body).toEqual({ content: written });
    expect(save).toBeDisabled();
    await waitFor(() => {
      expect(api.calls.filter((c) => c === FILES).length).toBe(treeCalls + 1);
    });

    type(view, "x");
    act(() => {
      view.contentDOM.dispatchEvent(
        new KeyboardEvent("keydown", {
          key: "s",
          ctrlKey: true,
          bubbles: true,
        }),
      );
    });
    await waitFor(() => {
      expect(puts(api.requests)).toHaveLength(2);
    });
    const second = puts(api.requests)[1];
    expect(second?.body).toEqual({ content: written + "x" });
    expect(second?.headers.get("if-match")).toBe(
      `"${fileWrite(NOTES_PATH, written).blob}"`,
    );
  });

  it("says when there was nothing to commit", async () => {
    fakeApi(
      docsRoutes(undefined, {
        [FILE]: (r: Recorded): Route =>
          r.method === "PUT"
            ? {
                body: fileWrite(NOTES_PATH, NOTES_TEXT + "!", {
                  committed: false,
                }),
              }
            : { body: fileContent(NOTES_PATH, NOTES_TEXT) },
      }),
    );
    const user = userEvent.setup();
    renderApp(`${DOCS}?path=${NOTES_PATH}`);
    const view = await editorFor(NOTES_PATH);
    type(view, "!");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText("No changes to save")).toBeInTheDocument();
  });

  it("resends with the same key only after a network failure", async () => {
    let attempt = 0;
    const api = fakeApi(
      docsRoutes(undefined, {
        [FILE]: (r: Recorded): Route | Promise<never> => {
          if (r.method !== "PUT") {
            return { body: fileContent(NOTES_PATH, NOTES_TEXT) };
          }
          attempt += 1;
          if (attempt === 1) {
            return Promise.reject(new TypeError("Failed to fetch"));
          }
          if (attempt === 2) {
            return problemBody(412, "file_stale", {
              blob: "b".repeat(40),
              commit: "c".repeat(40),
            });
          }
          return { body: fileWrite(NOTES_PATH, NOTES_TEXT + "!") };
        },
      }),
    );
    const user = userEvent.setup();
    renderApp(`${DOCS}?path=${NOTES_PATH}`);
    const view = await editorFor(NOTES_PATH);
    type(view, "!");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(
      await screen.findByText("The server could not be reached."),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save" }));
    const banner = await screen.findByText(
      "This file changed on main since you opened it.",
    );
    expect(banner).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Keep mine" }));
    await waitFor(() => {
      expect(puts(api.requests)).toHaveLength(3);
    });
    const [first, second, third] = puts(api.requests);
    const key = (r: Recorded | undefined) => r?.headers.get("idempotency-key");
    expect(key(second)).toBe(key(first));
    expect(key(third)).not.toBe(key(second));
    expect(third?.headers.get("if-match")).toBe(`"${"b".repeat(40)}"`);
    expect(
      await screen.findByText(`Saved · ${"c0ffee".padEnd(12, "1")}`),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("This file changed on main since you opened it."),
    ).toBeNull();
  });

  it("takes the latest version after a confirmation", async () => {
    let server = NOTES_TEXT;
    fakeApi(
      docsRoutes(undefined, {
        [FILE]: (r: Recorded): Route =>
          r.method === "PUT"
            ? problemBody(412, "file_stale", {
                blob: "b".repeat(40),
                commit: "c".repeat(40),
              })
            : { body: fileContent(NOTES_PATH, server) },
      }),
    );
    const user = userEvent.setup();
    renderApp(`${DOCS}?path=${NOTES_PATH}`);
    const view = await editorFor(NOTES_PATH);
    type(view, "mine");
    server = "# Theirs\n";
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.click(
      await screen.findByRole("button", { name: "Take theirs" }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: "Take the latest version?",
    });
    await user.click(
      within(dialog).getByRole("button", { name: "Take theirs" }),
    );
    await waitFor(() => {
      expect(view.state.sliceDoc()).toBe("# Theirs\n");
    });
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  });

  it("follows the server while clean and warns while dirty", async () => {
    let server = NOTES_TEXT;
    fakeApi(
      docsRoutes(undefined, {
        [FILE]: (): Route => ({ body: fileContent(NOTES_PATH, server) }),
      }),
    );
    const { queryClient } = renderApp(`${DOCS}?path=${NOTES_PATH}`);
    const view = await editorFor(NOTES_PATH);
    server = "# Changed elsewhere\n";
    await act(() => queryClient.invalidateQueries({ queryKey: ["docs"] }));
    await waitFor(() => {
      expect(view.state.sliceDoc()).toBe("# Changed elsewhere\n");
    });
    type(view, "mine");
    server = "# Changed again\n";
    await act(() => queryClient.invalidateQueries({ queryKey: ["docs"] }));
    expect(
      await screen.findByText("This file changed on main since you opened it."),
    ).toBeInTheDocument();
    expect(view.state.sliceDoc()).toBe("# Changed elsewhere\nmine");
  });

  it("keeps the edits when the branch refuses the push", async () => {
    fakeApi(
      docsRoutes(undefined, {
        [FILE]: (r: Recorded): Route =>
          r.method === "PUT"
            ? problemBody(409, "file_push_rejected", { branch: "main" })
            : { body: fileContent(NOTES_PATH, NOTES_TEXT) },
      }),
    );
    const user = userEvent.setup();
    renderApp(`${DOCS}?path=${NOTES_PATH}`);
    const view = await editorFor(NOTES_PATH);
    type(view, "!");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(
      await screen.findByText(
        "main refused the commit, for example because it is protected. Nothing was saved and your edits are still here.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Unsaved changes");
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
  });

  it("asks before leaving unsaved changes", async () => {
    fakeApi(docsRoutes());
    const user = userEvent.setup();
    const { router } = renderApp(`${DOCS}?path=${NOTES_PATH}`);
    const view = await editorFor(NOTES_PATH);
    type(view, "!");
    const tree = screen.getByRole("navigation", { name: "Files" });
    await user.click(
      within(tree).getByRole("link", { name: "architecture.md" }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: "Discard unsaved changes?",
    });
    await user.click(within(dialog).getByRole("button", { name: "Stay" }));
    expect(router.state.location.search).toEqual({ path: NOTES_PATH });
    await user.click(
      within(tree).getByRole("link", { name: "architecture.md" }),
    );
    await user.click(
      within(
        await screen.findByRole("dialog", { name: "Discard unsaved changes?" }),
      ).getByRole("button", { name: "Discard" }),
    );
    await waitFor(() => {
      expect(router.state.location.search).toEqual({
        path: "docs/architecture.md",
      });
    });
  });
});

describe("specs in the editor", () => {
  it("opens a new spec from the template of the UI language", async () => {
    const api = fakeApi(
      docsRoutes(undefined, {
        [TEMPLATE]: (r: Recorded): Route => ({
          body: {
            content: `---\nsaka: spec\n---\n# Template ${r.search.get("locale") ?? ""}\n`,
            locale: r.search.get("locale"),
            parser_version: 4,
          },
        }),
        [FILE]: (r: Recorded): Route =>
          r.method === "PUT"
            ? {
                body: fileWrite("docs/specs/export.md", "x", {
                  spec: editorSpec("x", {
                    path: "docs/specs/export.md",
                    state: "ready",
                    approved_revision: null,
                  }),
                }),
              }
            : problemBody(404, "not_found"),
      }),
    );
    const user = userEvent.setup();
    const { router } = renderApp(DOCS);
    await user.click(await screen.findByRole("button", { name: "New spec" }));
    const dialog = await screen.findByRole("dialog", { name: "New spec" });
    const field = within(dialog).getByRole("textbox", { name: "Path" });
    expect(field).toHaveValue("docs/specs/");
    await user.type(field, "tasks-done.md");
    await user.click(within(dialog).getByRole("button", { name: "Open" }));
    expect(
      within(dialog).getByText("docs/specs/tasks-done.md already exists."),
    ).toBeInTheDocument();
    await user.clear(field);
    await user.type(field, "docs/specs/export");
    await user.click(within(dialog).getByRole("button", { name: "Open" }));
    await waitFor(() => {
      expect(router.state.location.search).toEqual({
        path: "docs/specs/export.md",
      });
    });
    const view = await editorFor("docs/specs/export.md");
    expect(view.state.sliceDoc()).toBe("---\nsaka: spec\n---\n# Template en\n");
    expect(api.calls).toContain(`${TEMPLATE}?locale=en`);
    expect(screen.getByRole("status")).toHaveTextContent("Unsaved changes");
    expect(await screen.findByText("Not registered yet")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Save" }));
    const [put] = puts(api.requests);
    expect(put?.headers.get("if-none-match")).toBe("*");
    expect(put?.headers.get("if-match")).toBeNull();
    const panel = await screen.findByRole("region", { name: "Spec" });
    expect(await within(panel).findByText("Ready")).toBeInTheDocument();
  });

  it("shows the spec, its checks, and a live chip per step", async () => {
    let tasks = stepTasks;
    const fake = fakeStream();
    fakeApi(
      docsRoutes(undefined, {
        [SPECS]: { body: page([editorSpec(SPEC_TEXT)]) },
        [WORK_ITEMS]: () => ({ body: page(tasks) }),
      }),
    );
    const { queryClient } = renderApp(`${DOCS}?path=${SPEC_PATH}`);
    startEventStream(queryClient, () => fake.stream);
    const view = await editorFor(SPEC_PATH);
    const panel = await screen.findByRole("region", { name: "Spec" });
    expect(
      await within(panel).findByText("Mark tasks as done"),
    ).toBeInTheDocument();
    expect(within(panel).getByText("Approved")).toBeInTheDocument();
    expect(within(panel).getByText("9f1c00000000")).toBeInTheDocument();

    await waitFor(() => {
      expect(view.dom.querySelectorAll(".cm-chip")).toHaveLength(4);
    });
    const chips = [...view.dom.querySelectorAll<HTMLElement>(".cm-chip")];
    expect(chips.map((c) => [c.textContent, c.className])).toEqual([
      ["Queued", "cm-chip cm-chip-waiting"],
      ["Development · agent working", "cm-chip cm-chip-active"],
      ["Blocked · ci_red · #12", "cm-chip cm-chip-blocked"],
      ["Done", "cm-chip cm-chip-done"],
    ]);
    expect(chips[0]).toHaveAttribute("aria-hidden", "true");
    const row = view.state.doc.line(16).text;
    expect(row).toBe("| S1 | Store the flag | AC-01 | `make test` |");
    expect(
      within(panel).getByRole("link", { name: "Blocked · ci_red · #12" }),
    ).toHaveAttribute("href", `/delivery/tasks/${stepTasks[2]?.id ?? ""}`);

    tasks = stepTasks.map((t) =>
      t.step_id === "S1"
        ? {
            ...t,
            state: "blocked",
            blocker: {
              id: "0193f0c4-1b2a-7c3d-9e8f-0011223344b1",
              code: "session_failed",
              params: {},
              opened_at: "2026-10-31T10:06:00.000Z",
              stage: "dev",
            },
          }
        : t,
    );
    act(() => {
      fake.emit("delivery.work_item.blocked.v1", {
        stream_type: "work_item",
      });
    });
    await waitFor(() => {
      expect(view.dom.querySelector(".cm-chip")?.textContent).toBe(
        "Blocked · session_failed",
      );
    });
    expect(view.state.sliceDoc()).toBe(SPEC_TEXT);
    expect(view.state.lineBreak).toBe("\r\n");
  });

  it("explains why a saved spec was not registered", async () => {
    fakeApi(
      docsRoutes(undefined, {
        [FILE]: (r: Recorded): Route =>
          r.method === "PUT"
            ? {
                body: fileWrite(SPEC_PATH, SPEC_TEXT + "!", {
                  spec_problem: {
                    code: "spec_slug_conflict",
                    params: { slug: "tasks-done" },
                  },
                }),
              }
            : { body: fileContent(SPEC_PATH, SPEC_TEXT) },
      }),
    );
    const user = userEvent.setup();
    renderApp(`${DOCS}?path=${SPEC_PATH}`);
    const view = await editorFor(SPEC_PATH);
    type(view, " Now.", view.state.doc.line(10).to);
    await user.click(screen.getByRole("button", { name: "Save" }));
    const panel = await screen.findByRole("region", { name: "Spec" });
    expect(
      await within(panel).findByText(
        "Saved, but not registered as a spec: another spec already uses the slug tasks-done.",
      ),
    ).toBeInTheDocument();
  });

  it("warns when the buffer differs from the approved revision", async () => {
    fakeApi(
      docsRoutes(undefined, {
        [SPECS]: { body: page([editorSpec(SPEC_TEXT)]) },
      }),
    );
    renderApp(`${DOCS}?path=${SPEC_PATH}`);
    const view = await editorFor(SPEC_PATH);
    await screen.findByRole("region", { name: "Spec" });
    const banner = /^Changed since the approved revision a7e500000000/;
    expect(screen.queryByText(banner)).toBeNull();
    type(view, "More.\r\n");
    expect(await screen.findByText(banner)).toBeInTheDocument();
    act(() => {
      view.dispatch({
        changes: { from: view.state.doc.length - 6, to: view.state.doc.length },
      });
    });
    await waitFor(() => {
      expect(screen.queryByText(banner)).toBeNull();
    });
  });

  it("adds server context issues only while the text is the registered one", async () => {
    fakeApi(
      docsRoutes(undefined, {
        [SPECS]: {
          body: page([
            editorSpec(SPEC_TEXT, {
              lint: {
                ok: false,
                issues: [
                  {
                    code: "reference_missing",
                    severity: "error",
                    line: 6,
                    params: { path: "context.md" },
                  },
                ],
              },
            }),
          ]),
        },
      }),
    );
    renderApp(`${DOCS}?path=${SPEC_PATH}`);
    const view = await editorFor(SPEC_PATH);
    const panel = await screen.findByRole("region", { name: "Spec" });
    expect(
      await within(panel).findByText("1 error · 0 warnings"),
    ).toBeInTheDocument();
    const messages = () => {
      const found: string[] = [];
      forEachDiagnostic(view.state, (d) => found.push(d.message));
      return found;
    };
    await waitFor(
      () => {
        expect(messages()).toEqual([
          "The linked document context.md does not exist on the base branch.",
        ]);
      },
      { timeout: 2000 },
    );
    type(view, " Quickly.", view.state.doc.line(10).to);
    await waitFor(
      () => {
        expect(panel).toHaveTextContent("Ready");
      },
      { timeout: 2000 },
    );
    await waitFor(
      () => {
        expect(messages()).toEqual([]);
      },
      { timeout: 2000 },
    );
    expect(hashOf(SPEC_TEXT)).toMatch(/^sha256:/);
  });
});
