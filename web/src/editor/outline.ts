import { ensureSyntaxTree, syntaxTree } from "@codemirror/language";
import type { EditorState } from "@codemirror/state";

export type Heading = {
  level: number;
  text: string;
  from: number;
};

const ATX = /^ATXHeading([1-6])$/;
const SETEXT = /^SetextHeading([12])$/;
const PARSE_BUDGET_MS = 200;

function atxText(line: string): string {
  return line
    .replace(/^\s{0,3}#{1,6}(\s+|$)/, "")
    .replace(/\s+#+\s*$/, "")
    .trim();
}

export function outline(state: EditorState): Heading[] {
  const tree =
    ensureSyntaxTree(state, state.doc.length, PARSE_BUDGET_MS) ??
    syntaxTree(state);
  const headings: Heading[] = [];
  tree.iterate({
    enter: (node) => {
      const atx = ATX.exec(node.name);
      const setext = SETEXT.exec(node.name);
      if (atx === null && setext === null) {
        return node.name !== "FencedCode" && node.name !== "Frontmatter";
      }
      const line = state.doc.lineAt(node.from);
      const text = atx !== null ? atxText(line.text) : line.text.trim();
      headings.push({
        level: Number((atx ?? setext)?.[1]),
        text,
        from: line.from,
      });
      return false;
    },
  });
  return headings;
}
