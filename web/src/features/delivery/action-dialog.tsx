import { useId, useRef, useState } from "react";
import type { ReactNode } from "react";

import { ApiError, newIdempotencyKey } from "@/api/client";
import { useT } from "@/i18n/use-t";
import { Button } from "@/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogTitle,
  DialogTrigger,
} from "@/ui/dialog";

import { codeOf } from "./page-error";
import { errorText } from "./text";

export const REASON_MAX = 500;

export type Alternative = {
  label: string;
  run: (idempotencyKey: string) => Promise<void>;
};

export function ActionDialog({
  trigger,
  triggerVariant = "outline",
  title,
  description,
  submit,
  reason,
  note,
  children,
  explain,
  alternative,
  onSubmit,
}: {
  trigger: string;
  triggerVariant?: "default" | "outline";
  title: string;
  description: string;
  submit: string;
  reason?: { label: string; required: boolean };
  note?: { label: string; max: number };
  children?: ReactNode;
  explain?: (error: unknown) => string | null;
  alternative?: (error: unknown) => Alternative | null;
  onSubmit: (
    reason: string | null,
    idempotencyKey: string,
    note: string | null,
  ) => Promise<void>;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [noteValue, setNoteValue] = useState("");
  const retained = useRef<{
    key: string;
    reason: string | null;
    note: string | null;
  } | null>(null);
  const fieldId = useId();
  const countId = useId();
  const noteId = useId();
  const noteCountId = useId();
  const trimmed = value.trim();
  const noteTrimmed = noteValue.trim();
  const tooLong =
    value.length > REASON_MAX ||
    (note !== undefined && noteValue.length > note.max);
  const blocked =
    pending || tooLong || (reason?.required === true && trimmed === "");
  const other = error === null ? null : (alternative?.(error) ?? null);

  const change = (next: boolean) => {
    if (next) {
      setValue("");
      setNoteValue("");
      setError(null);
      retained.current = null;
    }
    setOpen(next);
  };

  const attempt = async (run: (key: string) => Promise<void>, key: string) => {
    setPending(true);
    setError(null);
    try {
      await run(key);
      setOpen(false);
      return null;
    } catch (e) {
      setError(e);
      return e;
    } finally {
      setPending(false);
    }
  };

  const send = async () => {
    const sent = trimmed === "" ? null : trimmed;
    const sentNote = noteTrimmed === "" ? null : noteTrimmed;
    const kept = retained.current;
    // The server stores every answer it gave under the key, so only a request
    // that never reached it may be sent again with the same key.
    const key =
      kept !== null && kept.reason === sent && kept.note === sentNote
        ? kept.key
        : newIdempotencyKey();
    retained.current = null;
    const failed = await attempt((k) => onSubmit(sent, k, sentNote), key);
    if (failed !== null && !(failed instanceof ApiError)) {
      retained.current = { key, reason: sent, note: sentNote };
    }
  };

  return (
    <Dialog open={open} onOpenChange={change}>
      <DialogTrigger asChild>
        <Button variant={triggerVariant}>{trigger}</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>{description}</DialogDescription>
        {children}
        <form
          className="space-y-3"
          onSubmit={(event) => {
            event.preventDefault();
            if (!blocked) {
              void send();
            }
          }}
        >
          {reason !== undefined && (
            <div className="space-y-1">
              <label htmlFor={fieldId} className="text-sm font-medium">
                {reason.label}
              </label>
              <textarea
                id={fieldId}
                value={value}
                aria-describedby={countId}
                aria-invalid={tooLong}
                onChange={(event) => {
                  setValue(event.target.value);
                }}
                rows={3}
                className="w-full rounded-md border border-border bg-background p-2 text-sm"
              />
              <p id={countId} className="text-xs text-muted-foreground">
                {t("dialog.count", { count: value.length, max: REASON_MAX })}
              </p>
            </div>
          )}
          {note !== undefined && (
            <div className="space-y-1">
              <label htmlFor={noteId} className="text-sm font-medium">
                {note.label}
              </label>
              <textarea
                id={noteId}
                value={noteValue}
                aria-describedby={noteCountId}
                aria-invalid={noteValue.length > note.max}
                onChange={(event) => {
                  setNoteValue(event.target.value);
                }}
                rows={4}
                className="w-full rounded-md border border-border bg-background p-2 text-sm"
              />
              <p id={noteCountId} className="text-xs text-muted-foreground">
                {t("dialog.count", { count: noteValue.length, max: note.max })}
              </p>
            </div>
          )}
          {error !== null && (
            <p role="alert" className="text-sm text-danger-foreground">
              {explain?.(error) ??
                errorText(
                  t,
                  codeOf(error),
                  error instanceof ApiError ? fieldParams(error.params) : {},
                )}
            </p>
          )}
          <div className="flex flex-wrap justify-end gap-2">
            <DialogClose asChild>
              <Button type="button" variant="outline">
                {t("dialog.cancel")}
              </Button>
            </DialogClose>
            {other !== null && (
              <Button
                type="button"
                variant="outline"
                disabled={pending}
                onClick={() => {
                  void attempt(other.run, newIdempotencyKey());
                }}
              >
                {other.label}
              </Button>
            )}
            <Button type="submit" disabled={blocked}>
              {submit}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function fieldParams(
  params: Record<string, unknown>,
): Record<string, unknown> {
  const errors = params.errors;
  if (
    Array.isArray(errors) &&
    typeof errors[0] === "object" &&
    errors[0] !== null
  ) {
    return { ...params, ...(errors[0] as Record<string, unknown>) };
  }
  return params;
}
