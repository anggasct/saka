import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import {
  defaultHighlightStyle,
  syntaxHighlighting,
} from "@codemirror/language";
import { type Diagnostic, linter, lintGutter } from "@codemirror/lint";
import { Compartment, EditorState, type Extension } from "@codemirror/state";
import {
  drawSelection,
  EditorView,
  highlightSpecialChars,
  keymap,
} from "@codemirror/view";

import { chipField } from "./chips";
import { Frontmatter } from "./frontmatter";
import { livePreview } from "./live-preview";
import type { LineSeparator } from "./lossless";

export type LintSource = (state: EditorState) => readonly Diagnostic[];

export type EditorConfig = {
  lineSeparator: LineSeparator;
  label: string;
  preview: boolean;
  taskLabel: string;
  lint: LintSource | null;
  onSave: () => void;
  onChange: (state: EditorState) => void;
};

export const LINT_DELAY_MS = 300;

export class EditorCompartments {
  readonly preview = new Compartment();
  readonly lint = new Compartment();
  readonly label = new Compartment();

  previewExtension(preview: boolean, taskLabel: string): Extension {
    return preview ? livePreview(taskLabel) : [];
  }

  lintExtension(source: LintSource | null): Extension {
    return source === null
      ? []
      : [
          linter((view) => source(view.state), { delay: LINT_DELAY_MS }),
          lintGutter(),
        ];
  }

  labelExtension(label: string): Extension {
    return EditorView.contentAttributes.of({ "aria-label": label });
  }
}

export function editorExtensions(
  config: EditorConfig,
  compartments: EditorCompartments,
): Extension[] {
  return [
    EditorState.lineSeparator.of(config.lineSeparator),
    markdown({ base: markdownLanguage, extensions: [Frontmatter] }),
    history(),
    drawSelection(),
    highlightSpecialChars(),
    syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
    EditorView.lineWrapping,
    keymap.of([
      {
        key: "Mod-s",
        preventDefault: true,
        run: () => {
          config.onSave();
          return true;
        },
      },
      ...defaultKeymap,
      ...historyKeymap,
    ]),
    chipField,
    compartments.preview.of(
      compartments.previewExtension(config.preview, config.taskLabel),
    ),
    compartments.lint.of(compartments.lintExtension(config.lint)),
    compartments.label.of(compartments.labelExtension(config.label)),
    EditorView.updateListener.of((update) => {
      if (update.docChanged) {
        config.onChange(update.state);
      }
    }),
  ];
}
