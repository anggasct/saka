// Live preview: markdown syntax is hidden except on the lines the cursor
// is on. Every decoration is visual; the document text is never rewritten
// unless the user acts (ticking a task, editing a property).
import { syntaxTree } from "@codemirror/language";
import { type EditorState, type Range, StateField } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate, WidgetType } from "@codemirror/view";
import type { SyntaxNode, SyntaxNodeRef } from "@lezer/common";
import type { Resolver } from "./resolve";

export interface NoteContext {
  path: string;
  resolver: Resolver;
  open(path: string): void;
}

const hidden = Decoration.replace({});

function activeLines(state: EditorState): Set<number> {
  const lines = new Set<number>();
  for (const r of state.selection.ranges) {
    const first = state.doc.lineAt(r.from).number;
    const last = state.doc.lineAt(r.to).number;
    for (let n = first; n <= last; n++) lines.add(n);
  }
  return lines;
}

function touches(state: EditorState, active: Set<number>, from: number, to: number): boolean {
  const first = state.doc.lineAt(from).number;
  const last = state.doc.lineAt(to).number;
  for (let n = first; n <= last; n++) if (active.has(n)) return true;
  return false;
}

class CheckboxWidget extends WidgetType {
  constructor(
    readonly checked: boolean,
    readonly pos: number,
  ) {
    super();
  }
  eq(other: CheckboxWidget): boolean {
    return other.checked === this.checked && other.pos === this.pos;
  }
  toDOM(view: EditorView): HTMLElement {
    const box = document.createElement("input");
    box.type = "checkbox";
    box.className = "cm-task";
    box.checked = this.checked;
    box.addEventListener("mousedown", (e) => {
      e.preventDefault();
      view.dispatch({ changes: { from: this.pos + 1, to: this.pos + 2, insert: this.checked ? " " : "x" } });
    });
    return box;
  }
  ignoreEvent(): boolean {
    return true;
  }
}

function build(view: EditorView, ctx: NoteContext): DecorationSet {
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
        if (name.startsWith("ATXHeading")) {
          out.push(Decoration.line({ class: `cm-h${name.slice(-1)}` }).range(state.doc.lineAt(node.from).from));
          return;
        }
        if (name === "FencedCode") {
          for (let pos = node.from; pos <= node.to; ) {
            const line = state.doc.lineAt(pos);
            out.push(Decoration.line({ class: "cm-codeblock" }).range(line.from));
            pos = line.to + 1;
          }
          return false;
        }
        if (name === "Blockquote") {
          const line = state.doc.lineAt(node.from);
          out.push(Decoration.line({ class: "cm-quote" }).range(line.from));
          return;
        }
        const on = touches(state, active, node.from, node.to);
        switch (name) {
          case "HeaderMark": {
            if (on) return;
            const space = state.sliceDoc(node.to, node.to + 1) === " " ? 1 : 0;
            out.push(hidden.range(node.from, node.to + space));
            return;
          }
          case "EmphasisMark":
          case "StrikethroughMark":
          case "CodeMark":
            if (!on && node.to > node.from) out.push(hidden.range(node.from, node.to));
            return;
          case "Link": {
            if (on) return false;
            const marks: [number, number][] = [];
            const n = node.node;
            for (let c = n.firstChild; c; c = c.nextSibling) {
              if (c.name === "LinkMark" || c.name === "URL" || c.name === "LinkTitle" || c.name === "LinkLabel") {
                marks.push([c.from, c.to]);
              }
            }
            // Hide `[`, and everything from `]` to the end: `](url "title")`.
            if (marks.length >= 2) {
              out.push(hidden.range(marks[0][0], marks[0][1]));
              out.push(Decoration.mark({ class: "cm-link" }).range(marks[0][1], marks[1][0]));
              out.push(hidden.range(marks[1][0], node.to));
            }
            return false;
          }
          case "WikiLink":
          case "Embed": {
            const n = node.node;
            const target = n.getChild("WikiLinkTarget");
            const alias = n.getChild("WikiLinkAlias");
            const text = target ? state.sliceDoc(target.from, target.to) : "";
            const dest = ctx.resolver.resolve(text, ctx.path);
            const cls = dest ? "cm-wikilink" : "cm-wikilink cm-unresolved";
            if (on) {
              out.push(Decoration.mark({ class: cls }).range(node.from, node.to));
              return false;
            }
            const shown = alias ?? target;
            if (!shown || shown.to <= shown.from) return false;
            if (node.from < shown.from) out.push(hidden.range(node.from, shown.from));
            out.push(Decoration.mark({ class: cls, attributes: { "data-dest": dest ?? "" } }).range(shown.from, shown.to));
            if (shown.to < node.to) out.push(hidden.range(shown.to, node.to));
            return false;
          }
          case "TaskMarker": {
            if (on) return;
            const checked = state.sliceDoc(node.from + 1, node.from + 2).toLowerCase() === "x";
            out.push(Decoration.replace({ widget: new CheckboxWidget(checked, node.from) }).range(node.from, node.to));
            return;
          }
        }
      },
    });
  }
  return Decoration.set(out, true);
}

export function livePreview(ctx: NoteContext) {
  return [
    ViewPlugin.fromClass(
      class {
        decorations: DecorationSet;
        constructor(view: EditorView) {
          this.decorations = build(view, ctx);
        }
        update(u: ViewUpdate) {
          if (u.docChanged || u.viewportChanged || u.selectionSet || syntaxTree(u.startState) !== syntaxTree(u.state)) {
            this.decorations = build(u.view, ctx);
          }
        }
      },
      { decorations: (p) => p.decorations },
    ),
    EditorView.domEventHandlers({
      mousedown(e, view) {
        if (!(e.ctrlKey || e.metaKey)) return false;
        const pos = view.posAtCoords(e);
        if (pos == null) return false;
        for (let n: SyntaxNode | null = syntaxTree(view.state).resolveInner(pos, 1); n; n = n.parent) {
          if (n.name === "WikiLink" || n.name === "Embed") {
            const t = n.getChild("WikiLinkTarget");
            const dest = t && ctx.resolver.resolve(view.state.sliceDoc(t.from, t.to), ctx.path);
            if (dest) ctx.open(dest);
            return true;
          }
        }
        return false;
      },
    }),
    properties,
  ];
}

interface Prop {
  key: string;
  value: string;
  from: number;
  to: number;
}

/** Top-level `key: value` lines and `- item` lists; enough for Properties. */
function parseProps(text: string, base: number): Prop[] {
  const props: Prop[] = [];
  let offset = 0;
  for (const line of text.split("\n")) {
    const start = base + offset;
    offset += line.length + 1;
    if (/^(---|\.\.\.)\s*$/.test(line)) continue;
    const kv = /^([^\s#:][^:]*):[ \t]*(.*?)\r?$/.exec(line);
    if (kv) {
      const valueFrom = start + line.length - kv[2].length - (line.endsWith("\r") ? 1 : 0);
      props.push({ key: kv[1], value: kv[2], from: valueFrom, to: valueFrom + kv[2].length });
      continue;
    }
    const item = /^\s+-\s+(.*?)\r?$/.exec(line);
    if (item && props.length) {
      const p = props[props.length - 1];
      p.value = p.value ? `${p.value}, ${item[1]}` : item[1];
      p.from = p.to = -1;
    }
  }
  return props;
}

class PropertiesWidget extends WidgetType {
  constructor(
    readonly text: string,
    readonly from: number,
  ) {
    super();
  }
  eq(other: PropertiesWidget): boolean {
    return other.text === this.text && other.from === this.from;
  }
  toDOM(view: EditorView): HTMLElement {
    const box = document.createElement("div");
    box.className = "cm-properties";
    const title = document.createElement("div");
    title.className = "cm-properties-title";
    title.textContent = "Properties";
    title.title = "Click to edit the raw frontmatter";
    title.addEventListener("mousedown", (e) => {
      e.preventDefault();
      view.dispatch({ selection: { anchor: this.from + 4 } });
      view.focus();
    });
    box.append(title);
    for (const p of parseProps(this.text, this.from)) {
      const row = document.createElement("div");
      row.className = "cm-prop";
      const k = document.createElement("span");
      k.className = "cm-prop-key";
      k.textContent = p.key;
      const v = document.createElement("span");
      v.className = "cm-prop-value";
      v.textContent = p.value || "—";
      if (p.from >= 0) {
        v.title = "Double-click to edit";
        v.addEventListener("dblclick", () => {
          const next = window.prompt(p.key, p.value);
          if (next !== null && next !== p.value) view.dispatch({ changes: { from: p.from, to: p.to, insert: next } });
        });
      }
      row.append(k, v);
      box.append(row);
    }
    return box;
  }
  ignoreEvent(): boolean {
    return true;
  }
}

function buildProperties(state: EditorState): DecorationSet {
  const first = syntaxTree(state).topNode.firstChild;
  if (!first || first.name !== "Frontmatter") return Decoration.none;
  const last = state.doc.lineAt(first.to);
  if (last.number === 1 || !/^(---|\.\.\.)[ \t]*\r?$/.test(last.text)) return Decoration.none;
  const sel = state.selection.main;
  if (sel.from <= first.to && sel.to >= first.from) return Decoration.none;
  // Positions count a line break as one character, so slice with "\n".
  const widget = new PropertiesWidget(state.doc.sliceString(first.from, first.to, "\n"), first.from);
  return Decoration.set([Decoration.replace({ widget, block: true }).range(first.from, first.to)]);
}

const properties = StateField.define<DecorationSet>({
  create: buildProperties,
  update: (deco, tr) => (tr.docChanged || tr.selection || syntaxTree(tr.startState) !== syntaxTree(tr.state) ? buildProperties(tr.state) : deco),
  provide: (f) => EditorView.decorations.from(f),
});
