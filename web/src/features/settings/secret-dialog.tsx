import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";

import { ApiError } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import { useKeyedRun } from "@/api/use-keyed-run";
import { fieldParams } from "@/features/delivery/action-dialog";
import { text } from "@/features/delivery/text";
import type { MessageKey } from "@/i18n/messages";
import { CATALOGS } from "@/i18n/messages";
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
import { Input } from "@/ui/input";
import { Textarea } from "@/ui/textarea";

import { invalidate, putSecret, SECRET_NAME } from "./queries";
import { settingsErrorText } from "./text";

function secretErrorText(t: ReturnType<typeof useT>, error: unknown): string {
  if (!(error instanceof ApiError)) {
    return settingsErrorText(t, "network_error");
  }
  if (error.code === "validation_failed") {
    const params = fieldParams(error.params);
    const key = `settings.secretError.${text(params.field)}_${text(params.code)}`;
    if (key in CATALOGS.en) {
      return t(key as MessageKey);
    }
  }
  return settingsErrorText(t, error.code, error.params);
}

function SecretForm({
  projectId,
  lockedName,
  onClose,
}: {
  projectId: string;
  lockedName: string | undefined;
  onClose: () => void;
}) {
  const t = useT();
  const client = useQueryClient();
  const action = useKeyedRun();
  const [name, setName] = useState(lockedName ?? "");
  const [value, setValue] = useState("");
  const [multiline, setMultiline] = useState(false);
  const nameId = useId();
  const nameHintId = useId();
  const valueId = useId();
  const nameValid = SECRET_NAME.test(name);
  const showNameHint = lockedName === undefined && name !== "" && !nameValid;
  const blocked = action.pending || !nameValid || value === "";

  const save = async () => {
    const done = await action.run(`${name}\u0000${value}`, async (key) => {
      await putSecret(projectId, name, value, key);
      await invalidate(client, QUERY_ROOTS.secrets);
    });
    if (done) {
      onClose();
    }
  };

  return (
    <form
      className="space-y-3"
      onSubmit={(event) => {
        event.preventDefault();
        if (!blocked) {
          void save();
        }
      }}
    >
      <div className="space-y-1">
        <label htmlFor={nameId} className="text-sm font-medium">
          {t("settings.secret.name")}
        </label>
        <Input
          id={nameId}
          value={name}
          readOnly={lockedName !== undefined}
          autoComplete="off"
          spellCheck={false}
          aria-invalid={showNameHint}
          aria-describedby={showNameHint ? nameHintId : undefined}
          onChange={(event) => {
            setName(event.target.value);
          }}
        />
        {showNameHint && (
          <p id={nameHintId} className="text-sm text-danger-foreground">
            {t("settings.secret.nameHint")}
          </p>
        )}
      </div>
      <div className="space-y-1">
        <label htmlFor={valueId} className="text-sm font-medium">
          {t("settings.secret.value")}
        </label>
        {multiline ? (
          <Textarea
            id={valueId}
            value={value}
            rows={5}
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => {
              setValue(event.target.value);
            }}
          />
        ) : (
          <Input
            id={valueId}
            type="password"
            value={value}
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => {
              setValue(event.target.value);
            }}
          />
        )}
      </div>
      <label className="flex min-h-11 items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={multiline}
          className="size-5"
          onChange={(event) => {
            setMultiline(event.target.checked);
          }}
        />
        {t("settings.secret.multiline")}
      </label>
      {action.error !== null && (
        <p role="alert" className="text-sm text-danger-foreground">
          {secretErrorText(t, action.error)}
        </p>
      )}
      <div className="flex flex-wrap justify-end gap-2">
        <DialogClose asChild>
          <Button type="button" variant="outline">
            {t("dialog.cancel")}
          </Button>
        </DialogClose>
        <Button type="submit" disabled={blocked}>
          {t("settings.secret.save")}
        </Button>
      </div>
    </form>
  );
}

export function SecretDialog({
  projectId,
  lockedName,
  trigger,
}: {
  projectId: string;
  lockedName?: string;
  trigger: string;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="outline">{trigger}</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>
          {lockedName === undefined
            ? t("settings.secret.addTitle")
            : t("settings.secret.replaceTitle", { name: lockedName })}
        </DialogTitle>
        <DialogDescription>
          {t("settings.secret.description")}
        </DialogDescription>
        <SecretForm
          projectId={projectId}
          lockedName={lockedName}
          onClose={() => {
            setOpen(false);
          }}
        />
      </DialogContent>
    </Dialog>
  );
}
