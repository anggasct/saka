import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { useEffect, useLayoutEffect, useRef, useState } from "react";

import { type Chip, setChips } from "./chips";
import {
  EditorCompartments,
  editorExtensions,
  type LintSource,
} from "./extensions";
import type { OpenedDocument } from "./lossless";

export type MarkdownEditorProps = {
  document: OpenedDocument;
  label: string;
  preview: boolean;
  taskLabel: string;
  lint: LintSource | null;
  chips: readonly Chip[];
  onChange: (state: EditorState) => void;
  onSave: () => void;
  onView: (view: EditorView | null) => void;
};

function sameText(state: EditorState, opened: OpenedDocument): boolean {
  return (
    state.lineBreak === opened.lineSeparator && state.sliceDoc() === opened.text
  );
}

export function MarkdownEditor(props: MarkdownEditorProps) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const latest = useRef(props);
  useLayoutEffect(() => {
    latest.current = props;
  });
  const [compartments] = useState(() => new EditorCompartments());
  const { document: opened } = props;

  useEffect(() => {
    const current = latest.current;
    const create = () =>
      EditorState.create({
        doc: opened.text,
        extensions: editorExtensions(
          {
            lineSeparator: opened.lineSeparator,
            label: current.label,
            preview: current.preview,
            taskLabel: current.taskLabel,
            lint: current.lint,
            onSave: () => {
              latest.current.onSave();
            },
            onChange: (state) => {
              latest.current.onChange(state);
            },
          },
          compartments,
        ),
      });
    const existing = view.current;
    if (existing !== null) {
      // The same text again (after a save, or a refetch) keeps cursor and undo history.
      if (!sameText(existing.state, opened)) {
        existing.setState(create());
        existing.dispatch({ effects: setChips.of(current.chips) });
        current.onChange(existing.state);
      }
      return;
    }
    const parent = host.current;
    if (parent === null) {
      return;
    }
    const created = new EditorView({ parent, state: create() });
    view.current = created;
    current.onView(created);
    current.onChange(created.state);
  }, [opened, compartments]);

  useEffect(
    () => () => {
      latest.current.onView(null);
      view.current?.destroy();
      view.current = null;
    },
    [],
  );

  const { preview, taskLabel, lint, label, chips } = props;

  useEffect(() => {
    view.current?.dispatch({
      effects: compartments.preview.reconfigure(
        compartments.previewExtension(preview, taskLabel),
      ),
    });
  }, [compartments, preview, taskLabel]);

  useEffect(() => {
    view.current?.dispatch({
      effects: compartments.lint.reconfigure(compartments.lintExtension(lint)),
    });
  }, [compartments, lint]);

  useEffect(() => {
    view.current?.dispatch({
      effects: compartments.label.reconfigure(
        compartments.labelExtension(label),
      ),
    });
  }, [compartments, label]);

  useEffect(() => {
    view.current?.dispatch({ effects: setChips.of(chips) });
  }, [chips]);

  return <div ref={host} className="docs-editor" />;
}
