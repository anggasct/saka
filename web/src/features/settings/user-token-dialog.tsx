import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";

import { QUERY_ROOTS } from "@/api/events";
import { useKeyedRun } from "@/api/use-keyed-run";
import { fieldParams } from "@/features/delivery/action-dialog";
import { codeOf } from "@/features/delivery/page-error";
import { useT } from "@/i18n/use-t";
import { ApiError } from "@/api/client";
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

import { invalidate, putUserToken } from "./queries";
import { settingsErrorText } from "./text";

function TokenForm({
  projectId,
  onClose,
}: {
  projectId: string;
  onClose: () => void;
}) {
  const t = useT();
  const client = useQueryClient();
  const action = useKeyedRun();
  const [value, setValue] = useState("");
  const valueId = useId();
  const blocked = action.pending || value === "";

  const save = async () => {
    const done = await action.run(value, async (key) => {
      await putUserToken(projectId, value, key);
      await invalidate(client, QUERY_ROOTS.authorship);
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
        <label htmlFor={valueId} className="text-sm font-medium">
          {t("settings.token.value")}
        </label>
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
      </div>
      {action.error !== null && (
        <p role="alert" className="text-sm text-danger-foreground">
          {settingsErrorText(
            t,
            codeOf(action.error),
            action.error instanceof ApiError
              ? fieldParams(action.error.params)
              : {},
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
          {t("settings.token.save")}
        </Button>
      </div>
    </form>
  );
}

export function UserTokenDialog({
  projectId,
  replacing,
}: {
  projectId: string;
  replacing: boolean;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="outline">
          {replacing ? t("settings.token.replace") : t("settings.token.set")}
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>{t("settings.token.title")}</DialogTitle>
        <DialogDescription>{t("settings.token.description")}</DialogDescription>
        <TokenForm
          projectId={projectId}
          onClose={() => {
            setOpen(false);
          }}
        />
      </DialogContent>
    </Dialog>
  );
}
