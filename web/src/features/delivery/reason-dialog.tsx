import { useId, useState } from "react";
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

export function ReasonDialog({
  trigger,
  title,
  description,
  reasonLabel,
  submit,
  required,
  children,
  onSubmit,
}: {
  trigger: string;
  title: string;
  description: string;
  reasonLabel: string;
  submit: string;
  required: boolean;
  children?: ReactNode;
  onSubmit: (reason: string | null, idempotencyKey: string) => Promise<void>;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const [key, setKey] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const fieldId = useId();
  const countId = useId();
  const trimmed = reason.trim();
  const tooLong = reason.length > REASON_MAX;
  const blocked = pending || tooLong || (required && trimmed === "");

  const change = (next: boolean) => {
    if (next) {
      setReason("");
      setError(null);
      setKey(newIdempotencyKey());
    }
    setOpen(next);
  };

  const send = async () => {
    setPending(true);
    setError(null);
    try {
      await onSubmit(trimmed === "" ? null : trimmed, key);
      setOpen(false);
    } catch (e) {
      setError(e);
    } finally {
      setPending(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={change}>
      <DialogTrigger asChild>
        <Button variant="outline">{trigger}</Button>
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
          <div className="space-y-1">
            <label htmlFor={fieldId} className="text-sm font-medium">
              {reasonLabel}
            </label>
            <textarea
              id={fieldId}
              value={reason}
              aria-describedby={countId}
              aria-invalid={tooLong}
              onChange={(event) => {
                setReason(event.target.value);
              }}
              rows={3}
              className="w-full rounded-md border border-border bg-background p-2 text-sm"
            />
            <p id={countId} className="text-xs text-muted-foreground">
              {t("dialog.count", { count: reason.length, max: REASON_MAX })}
            </p>
          </div>
          {error !== null && (
            <p role="alert" className="text-sm text-danger-foreground">
              {errorText(
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
