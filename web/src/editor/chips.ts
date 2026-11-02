import { StateEffect, StateField, type Range } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  WidgetType,
} from "@codemirror/view";

export type ChipTone = "done" | "blocked" | "active" | "waiting";

export type Chip = {
  line: number;
  text: string;
  tone: ChipTone;
};

export const setChips = StateEffect.define<readonly Chip[]>();

class ChipWidget extends WidgetType {
  constructor(readonly chip: Chip) {
    super();
  }

  override eq(other: ChipWidget): boolean {
    return (
      other.chip.text === this.chip.text && other.chip.tone === this.chip.tone
    );
  }

  override toDOM(view: EditorView): HTMLElement {
    const span = view.dom.ownerDocument.createElement("span");
    span.className = `cm-chip cm-chip-${this.chip.tone}`;
    span.setAttribute("aria-hidden", "true");
    span.title = this.chip.text;
    span.textContent = this.chip.text;
    return span;
  }

  override ignoreEvent(): boolean {
    return false;
  }
}

export const chipField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(value, tr) {
    let next = value.map(tr.changes);
    for (const effect of tr.effects) {
      if (effect.is(setChips)) {
        const ranges: Range<Decoration>[] = [];
        for (const chip of effect.value) {
          if (chip.line >= 1 && chip.line <= tr.state.doc.lines) {
            const line = tr.state.doc.line(chip.line);
            ranges.push(
              Decoration.widget({
                widget: new ChipWidget(chip),
                side: 1,
              }).range(line.to),
            );
          }
        }
        next = Decoration.set(ranges, true);
      }
    }
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});
