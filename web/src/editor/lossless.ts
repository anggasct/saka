import type { EditorState } from "@codemirror/state";

const BOM = "﻿";
const BREAK = /\r\n|\r|\n/g;

export type LineSeparator = "\n" | "\r\n";

export type OpenedDocument = {
  bom: boolean;
  text: string;
  lineSeparator: LineSeparator;
};

function separatorOf(text: string): LineSeparator {
  const breaks = text.match(BREAK) ?? [];
  // CRLF only when every break is CRLF; otherwise a lone CR, or the CR of a
  // CRLF in a mixed file, stays an ordinary character of its line.
  return breaks.length > 0 && breaks.every((b) => b === "\r\n") ? "\r\n" : "\n";
}

export function openDocument(content: string): OpenedDocument {
  const bom = content.startsWith(BOM);
  const text = bom ? content.slice(BOM.length) : content;
  return { bom, text, lineSeparator: separatorOf(text) };
}

export function saveDocument(bom: boolean, state: EditorState): string {
  return (bom ? BOM : "") + state.sliceDoc();
}
