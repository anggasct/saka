import type { BlockContext, Line, MarkdownConfig } from "@lezer/markdown";

const OPEN = /^---[ \t]*$/;
const CLOSE = /^(---|\.\.\.)[ \t]*$/;

export const Frontmatter: MarkdownConfig = {
  defineNodes: [{ name: "Frontmatter", block: true }],
  parseBlock: [
    {
      name: "Frontmatter",
      before: "HorizontalRule",
      parse(cx: BlockContext, line: Line): boolean {
        if (cx.lineStart !== 0 || !OPEN.test(line.text)) {
          return false;
        }
        // The block parser cannot look ahead, so an unclosed block runs to the end.
        let to = line.text.length;
        while (cx.nextLine()) {
          to = cx.lineStart + line.text.length;
          if (CLOSE.test(line.text)) {
            cx.nextLine();
            break;
          }
        }
        cx.addElement(cx.elt("Frontmatter", 0, to));
        return true;
      },
    },
  ],
};
