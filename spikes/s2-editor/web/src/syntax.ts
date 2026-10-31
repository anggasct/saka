// Obsidian syntax on top of the CommonMark/GFM parser: [[wiki-links]],
// ![[embeds]] and YAML frontmatter at the top of a note.
import type { BlockContext, InlineContext, Line, MarkdownConfig } from "@lezer/markdown";

const BANG = 33;
const OPEN = 91;
const CLOSE = 93;
const PIPE = 124;
const BACKSLASH = 92;
const NEWLINE = 10;

export const WikiLink: MarkdownConfig = {
  defineNodes: ["WikiLink", "Embed", "WikiLinkMark", "WikiLinkTarget", "WikiLinkAlias"],
  parseInline: [
    {
      name: "WikiLink",
      before: "Link",
      parse(cx: InlineContext, next: number, pos: number): number {
        const embed = next === BANG;
        const open = embed ? pos + 3 : pos + 2;
        if (embed ? cx.char(pos + 1) !== OPEN || cx.char(pos + 2) !== OPEN : next !== OPEN || cx.char(pos + 1) !== OPEN) {
          return -1;
        }
        let end = -1;
        for (let i = open; i < cx.end - 1; i++) {
          const c = cx.char(i);
          if (c === NEWLINE || (c === OPEN && cx.char(i + 1) === OPEN)) return -1;
          if (c === CLOSE && cx.char(i + 1) === CLOSE) {
            end = i;
            break;
          }
        }
        if (end <= open) return -1;
        let pipe = -1;
        for (let i = open; i < end; i++) {
          if (cx.char(i) === PIPE) {
            pipe = i;
            break;
          }
        }
        // Inside tables the pipe is escaped: [[target\|alias]].
        const targetEnd = pipe < 0 ? end : cx.char(pipe - 1) === BACKSLASH ? pipe - 1 : pipe;
        const children = [cx.elt("WikiLinkMark", pos, open), cx.elt("WikiLinkTarget", open, targetEnd)];
        if (pipe >= 0) {
          children.push(cx.elt("WikiLinkMark", targetEnd, pipe + 1), cx.elt("WikiLinkAlias", pipe + 1, end));
        }
        children.push(cx.elt("WikiLinkMark", end, end + 2));
        return cx.addElement(cx.elt(embed ? "Embed" : "WikiLink", pos, end + 2, children));
      },
    },
  ],
};

const FENCE = /^(---|\.\.\.)[ \t]*$/;

export const Frontmatter: MarkdownConfig = {
  defineNodes: [{ name: "Frontmatter", block: true }],
  parseBlock: [
    {
      name: "Frontmatter",
      before: "HorizontalRule",
      parse(cx: BlockContext, line: Line): boolean {
        if (cx.lineStart !== 0 || !/^---[ \t]*$/.test(line.text)) return false;
        // The parser cannot look ahead, so an unclosed block runs to the end
        // of the note; the Properties widget only renders closed blocks.
        let to = line.text.length;
        while (cx.nextLine()) {
          to = cx.lineStart + line.text.length;
          if (FENCE.test(line.text)) {
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
