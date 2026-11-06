import { forEachDiagnostic } from "@codemirror/lint";
import { EditorView } from "@codemirror/view";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import {
  FILE,
  fileContent,
  FILES,
  fileTree,
  fileWrite,
  PROJECTS,
  SPEC_PATH,
  SPEC_TEXT,
  SPECS,
  WORK_ITEMS,
} from "@/test/docs-fixtures";
import {
  DRAFT_CONTENT,
  DRAFT_ID,
  DRAFT_PATH,
  DRAFT_TEXT,
  DRAFTS,
  draftContent,
} from "@/test/draft-fixtures";
import { fakeApi, page, type Recorded, type Routes } from "@/test/fake-api";
import { project, PROJECT_ID } from "@/test/fixtures";
import { renderApp } from "@/test/render";

const DOCS = `/docs/${PROJECT_ID}`;
const OPEN_DECISION = "Decision Q1 is not answered yet: check one option.";

function routes(extra: Routes = {}): Routes {
  return {
    [PROJECTS]: { body: page([project]) },
    [FILES]: { body: fileTree() },
    [FILE]: (request: Recorded) => {
      if (request.method === "PUT") {
        const body = request.body as { content: string };
        return { body: fileWrite(DRAFT_PATH, body.content) };
      }
      const path = request.search.get("path") ?? "";
      return path === SPEC_PATH
        ? { body: fileContent(path, SPEC_TEXT) }
        : {
            status: 404,
            body: { code: "not_found", params: {}, status: 404 },
          };
    },
    [SPECS]: { body: page([]) },
    [WORK_ITEMS]: { body: page([]) },
    [DRAFTS]: { body: page([]) },
    [DRAFT_CONTENT]: { body: draftContent() },
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

function messages(view: EditorView): string[] {
  const found: string[] = [];
  forEachDiagnostic(view.state, (d) => found.push(d.message));
  return found;
}

function draftUrl(path: string): string {
  return `${DOCS}?path=${encodeURIComponent(path)}&draft=${DRAFT_ID}`;
}

describe("Buffer from a draft", () => {
  it("opens the written bytes unsaved and saves them as a new file", async () => {
    const api = fakeApi(routes());
    const user = userEvent.setup();
    renderApp(draftUrl(DRAFT_PATH));
    const view = await editorFor(DRAFT_PATH);
    expect(view.state.sliceDoc()).toBe(DRAFT_TEXT);
    expect(screen.getByRole("status")).toHaveTextContent("Unsaved changes");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => {
      expect(api.requests.some((r) => r.method === "PUT")).toBe(true);
    });
    const put = api.requests.find((r) => r.method === "PUT");
    expect(put?.headers.get("If-None-Match")).toBe("*");
    expect(put?.headers.get("If-Match")).toBeNull();
    expect(put?.body).toEqual({ content: DRAFT_TEXT });
    expect(put?.search.get("path")).toBe(DRAFT_PATH);
    expect(api.requests.filter((r) => r.path === DRAFT_CONTENT)).toHaveLength(
      1,
    );
  });

  it("opens an existing file normally and says the draft was not loaded", async () => {
    const api = fakeApi(routes());
    renderApp(draftUrl(SPEC_PATH));
    const view = await editorFor(SPEC_PATH);
    expect(view.state.sliceDoc()).toBe(SPEC_TEXT);
    expect(
      screen.getByText(
        "A file already exists at this path, so the draft was not loaded.",
      ),
    ).toBeInTheDocument();
    expect(api.requests.some((r) => r.path === DRAFT_CONTENT)).toBe(false);
  });

  it("says when the draft has nothing written", async () => {
    fakeApi(
      routes({
        [DRAFT_CONTENT]: {
          status: 404,
          body: { code: "not_found", params: {}, status: 404 },
        },
      }),
    );
    renderApp(draftUrl(DRAFT_PATH));
    expect(
      await screen.findByRole("heading", { name: "Nothing written yet" }),
    ).toBeInTheDocument();
  });

  it("counts open decisions and answers one with a single character", async () => {
    fakeApi(routes());
    const user = userEvent.setup();
    renderApp(draftUrl(DRAFT_PATH));
    const view = await editorFor(DRAFT_PATH);
    const panel = await screen.findByRole("region", { name: "Spec" });
    expect(
      await within(panel).findByText("1 error · 0 warnings · 1 open decision"),
    ).toBeInTheDocument();
    await waitFor(() => {
      expect(messages(view)).toContain(OPEN_DECISION);
    });

    const before = view.state.sliceDoc();
    const boxes = view.dom.querySelectorAll<HTMLInputElement>("input.cm-task");
    expect(boxes).toHaveLength(2);
    const first = boxes.item(0);
    await user.click(first);
    const after = view.state.sliceDoc();
    expect(after).toHaveLength(before.length);
    const changed = Array.from({ length: after.length }, (_, i) => i).filter(
      (i) => after[i] !== before[i],
    );
    expect(changed).toHaveLength(1);
    const at = changed[0] ?? 0;
    expect(after.slice(at - 1, at + 2)).toBe("[x]");
    expect(before.slice(at - 1, at + 2)).toBe("[ ]");

    await waitFor(() => {
      expect(messages(view)).not.toContain(OPEN_DECISION);
    });
    expect(await within(panel).findByText("Ready")).toBeInTheDocument();
  });
});
