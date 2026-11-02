import { syntaxTree } from "@codemirror/language";
import type { EditorState, Range } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  type EditorView,
  ViewPlugin,
  type ViewUpdate,
  WidgetType,
} from "@codemirror/view";
import type { SyntaxNodeRef } from "@lezer/common";

const hidden = Decoration.replace({});
const LINK_PARTS = new Set(["LinkMark", "URL", "LinkTitle", "LinkLabel"]);

function activeLines(state: EditorState): Set<number> {
  const lines = new Set<number>();
  for (const range of state.selection.ranges) {
    const first = state.doc.lineAt(range.from).number;
    const last = state.doc.lineAt(range.to).number;
    for (let n = first; n <= last; n++) {
      lines.add(n);
    }
  }
  return lines;
}

function touches(
  state: EditorState,
  active: Set<number>,
  from: number,
  to: number,
): boolean {
  const first = state.doc.lineAt(from).number;
  const last = state.doc.lineAt(to).number;
  for (let n = first; n <= last; n++) {
    if (active.has(n)) {
      return true;
    }
  }
  return false;
}

class TaskWidget extends WidgetType {
  constructor(
    readonly checked: boolean,
    readonly pos: number,
    readonly label: string,
  ) {
    super();
  }

  override eq(other: TaskWidget): boolean {
    return (
      other.checked === this.checked &&
      other.pos === this.pos &&
      other.label === this.label
    );
  }

  override toDOM(view: EditorView): HTMLElement {
    const box = view.dom.ownerDocument.createElement("input");
    box.type = "checkbox";
    box.className = "cm-task";
    box.checked = this.checked;
    box.setAttribute("aria-label", this.label);
    box.addEventListener("mousedown", (event) => {
      event.preventDefault();
      view.dispatch({
        changes: {
          from: this.pos + 1,
          to: this.pos + 2,
          insert: this.checked ? " " : "x",
        },
      });
    });
    return box;
  }

  override ignoreEvent(): boolean {
    return true;
  }
}

function lineClass(
  out: Range<Decoration>[],
  state: EditorState,
  from: number,
  to: number,
  cls: string,
) {
  for (let pos = from; pos <= to;) {
    const line = state.doc.lineAt(pos);
    out.push(Decoration.line({ class: cls }).range(line.from));
    pos = line.to + 1;
  }
}

function build(view: EditorView, taskLabel: string): DecorationSet {
  const { state } = view;
  const active = activeLines(state);
  const out: Range<Decoration>[] = [];
  const tree = syntaxTree(state);
  for (const { from, to } of view.visibleRanges) {
    tree.iterate({
      from,
      to,
      enter: (node: SyntaxNodeRef) => {
        const name = node.name;
        if (name === "Frontmatter" || name === "Table") {
          return false;
        }
        const heading = /^(ATX|Setext)Heading([1-6])$/.exec(name);
        if (heading !== null) {
          const line = state.doc.lineAt(node.from);
          out.push(
            Decoration.line({ class: `cm-h cm-h${heading[2] ?? ""}` }).range(
              line.from,
            ),
          );
          return heading[1] === "ATX";
        }
        if (name === "FencedCode" || name === "CodeBlock") {
          lineClass(out, state, node.from, node.to, "cm-codeblock");
          return false;
        }
        if (name === "Blockquote") {
          lineClass(out, state, node.from, node.to, "cm-quote");
          return;
        }
        const on = touches(state, active, node.from, node.to);
        switch (name) {
          case "HeaderMark": {
            if (!on) {
              const space =
                state.sliceDoc(node.to, node.to + 1) === " " ? 1 : 0;
              out.push(hidden.range(node.from, node.to + space));
            }
            return;
          }
          case "EmphasisMark":
          case "StrikethroughMark":
          case "CodeMark":
            if (!on && node.to > node.from) {
              out.push(hidden.range(node.from, node.to));
            }
            return;
          case "Link": {
            if (on) {
              return false;
            }
            const marks: [number, number][] = [];
            for (let c = node.node.firstChild; c; c = c.nextSibling) {
              if (LINK_PARTS.has(c.name)) {
                marks.push([c.from, c.to]);
              }
            }
            const [open, close] = marks;
            if (open !== undefined && close !== undefined) {
              out.push(hidden.range(open[0], open[1]));
              out.push(
                Decoration.mark({ class: "cm-link" }).range(open[1], close[0]),
              );
              out.push(hidden.range(close[0], node.to));
            }
            return false;
          }
          case "TaskMarker": {
            if (!on) {
              const checked =
                state.sliceDoc(node.from + 1, node.from + 2).toLowerCase() ===
                "x";
              out.push(
                Decoration.replace({
                  widget: new TaskWidget(checked, node.from, taskLabel),
                }).range(node.from, node.to),
              );
            }
            return;
          }
        }
        return;
      },
    });
  }
  return Decoration.set(out, true);
}

export function livePreview(taskLabel: string) {
  return ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;

      constructor(view: EditorView) {
        this.decorations = build(view, taskLabel);
      }

      update(update: ViewUpdate) {
        if (
          update.docChanged ||
          update.viewportChanged ||
          update.selectionSet ||
          syntaxTree(update.startState) !== syntaxTree(update.state)
        ) {
          this.decorations = build(update.view, taskLabel);
        }
      }
    },
    { decorations: (plugin) => plugin.decorations },
  );
}
