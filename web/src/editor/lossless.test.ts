import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { afterEach, describe, expect, it } from "vitest";

import { EditorCompartments, editorExtensions } from "./extensions";
import { openDocument, saveDocument } from "./lossless";

const CORPUS: Record<string, string> = {
  lf: "# A\nline\n",
  crlf: "# A\r\nline\r\n",
  mixed: "# A\r\nline\nlast\r\n",
  loneCr: "# A\rline\r",
  bom: "﻿# A\nline\n",
  bomCrlf: "﻿---\r\nsaka: spec\r\n---\r\n# T\r\n",
  noNewline: "# A\nlast line",
  empty: "",
  unicode: "# Ünïcödé 🚀\né combining\n",
  spaces: "\tindented\ntrailing   \n\n\n",
  tasks: "- [ ] one\r\n- [x] two\r\n",
};

const views: EditorView[] = [];

afterEach(() => {
  for (const view of views.splice(0)) {
    view.destroy();
  }
});

function stateOf(content: string, onSave: () => void = () => undefined) {
  const opened = openDocument(content);
  const state = EditorState.create({
    doc: opened.text,
    extensions: editorExtensions(
      {
        lineSeparator: opened.lineSeparator,
        label: "Editor",
        preview: true,
        taskLabel: "Task done",
        lint: null,
        onSave,
        onChange: () => undefined,
      },
      new EditorCompartments(),
    ),
  });
  return { opened, state };
}

function mount(state: EditorState) {
  const view = new EditorView({ state });
  views.push(view);
  return view;
}

describe("lossless editing", () => {
  it.each(Object.entries(CORPUS))("saves %s back unchanged", (_, content) => {
    const { opened, state } = stateOf(content);
    expect(saveDocument(opened.bom, state)).toBe(content);
  });

  it.each(Object.entries(CORPUS))(
    "typing one character in %s changes only that place",
    (_, content) => {
      const { opened, state } = stateOf(content);
      const start = state.update({ changes: { from: 0, insert: "Z" } }).state;
      const bom = opened.bom ? "﻿" : "";
      expect(saveDocument(opened.bom, start)).toBe(
        bom + "Z" + content.slice(bom.length),
      );
      const end = state.update({
        changes: { from: state.doc.length, insert: "Z" },
      }).state;
      expect(saveDocument(opened.bom, end)).toBe(content + "Z");
      const firstLine = state.doc.line(1);
      const mid = state.update({
        changes: { from: firstLine.to, insert: "Z" },
      }).state;
      const at = bom.length + firstLine.to;
      expect(saveDocument(opened.bom, mid)).toBe(
        content.slice(0, at) + "Z" + content.slice(at),
      );
    },
  );

  it("a task checkbox changes exactly one character", () => {
    const content = CORPUS.tasks ?? "";
    const { opened, state } = stateOf(content);
    const view = mount(
      state.update({ selection: { anchor: state.doc.length } }).state,
    );
    const boxes = view.dom.querySelectorAll<HTMLInputElement>("input.cm-task");
    expect(boxes).toHaveLength(2);
    expect(boxes[0]?.getAttribute("aria-label")).toBe("Task done");
    boxes[0]?.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    expect(saveDocument(opened.bom, view.state)).toBe(
      "- [x] one\r\n- [x] two\r\n",
    );
  });

  it("Mod-s saves the exact text", () => {
    const content = CORPUS.bomCrlf ?? "";
    const saved: string[] = [];
    let view: EditorView | null = null;
    const { opened, state } = stateOf(content, () => {
      if (view !== null) {
        saved.push(saveDocument(opened.bom, view.state));
      }
    });
    view = mount(state);
    view.contentDOM.dispatchEvent(
      new KeyboardEvent("keydown", { key: "s", ctrlKey: true, bubbles: true }),
    );
    expect(saved).toEqual([content]);
  });
});
