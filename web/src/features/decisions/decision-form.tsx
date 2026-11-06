import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import type { SyntheticEvent } from "react";

import { newIdempotencyKey } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import { codeOf } from "@/features/delivery/page-error";
import { errorText } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Button } from "@/ui/button";
import { RadioGroup } from "@/ui/radio-group";
import { Textarea } from "@/ui/textarea";

import type { DecisionOption } from "./queries";
import { answerDecision } from "./queries";

const NOTE_MAX = 2000;
const REFRESH_ON = [
  "decision_already_answered",
  "decision_not_found",
  "inbox_item_closed",
];

export function DecisionForm({
  id,
  options,
}: {
  id: string;
  options: readonly DecisionOption[];
}) {
  const t = useT();
  const queryClient = useQueryClient();
  const name = useId();
  const noteId = useId();
  const [choice, setChoice] = useState("");
  const [note, setNote] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const ready = choice !== "" || note.trim() !== "";

  const refresh = () =>
    Promise.all(
      [
        QUERY_ROOTS.decisions,
        QUERY_ROOTS.inbox,
        QUERY_ROOTS.workItems,
        QUERY_ROOTS.specDrafts,
      ].map((root) => queryClient.invalidateQueries({ queryKey: [root] })),
    );

  const submit = async (event: SyntheticEvent) => {
    event.preventDefault();
    if (!ready || pending) {
      return;
    }
    setPending(true);
    setError(null);
    try {
      await answerDecision(
        id,
        {
          option: choice === "" ? null : Number(choice),
          note: note.trim() === "" ? null : note.trim(),
        },
        newIdempotencyKey(),
      );
      await refresh();
    } catch (e) {
      setError(e);
      if (REFRESH_ON.includes(codeOf(e))) {
        await refresh();
      }
    } finally {
      setPending(false);
    }
  };

  return (
    <form
      onSubmit={(event) => {
        void submit(event);
      }}
      className="space-y-3"
    >
      <RadioGroup
        legend={t("decision.choose")}
        name={name}
        value={choice}
        onChange={setChoice}
        options={options.map((option, index) => ({
          value: String(index),
          label: (
            <>
              <span className="break-words">{option.label}</span>
              {index === 0 && (
                <Badge tone="success" className="ml-2 align-middle">
                  {t("decision.recommended")}
                </Badge>
              )}
            </>
          ),
          description: option.detail ?? undefined,
        }))}
      />
      <div className="space-y-1">
        <label htmlFor={noteId} className="block text-sm font-medium">
          {t("decision.note")}
        </label>
        <Textarea
          id={noteId}
          rows={3}
          maxLength={NOTE_MAX}
          value={note}
          onChange={(event) => {
            setNote(event.target.value);
          }}
        />
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <Button type="submit" disabled={!ready || pending}>
          {t("decision.submit")}
        </Button>
      </div>
      {error !== null && (
        <p role="alert" className="text-danger-foreground">
          {errorText(t, codeOf(error))}
        </p>
      )}
    </form>
  );
}
