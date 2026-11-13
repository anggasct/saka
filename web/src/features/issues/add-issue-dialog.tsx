import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";

import { ApiError } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import { useRetainedKey } from "@/api/use-keyed-run";
import { fieldParams } from "@/features/delivery/action-dialog";
import type { ProjectItem } from "@/features/delivery/queries";
import { codeOf } from "@/features/delivery/page-error";
import { errorText } from "@/features/delivery/text";
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

import { createIssueIntake } from "./queries";

export function AddIssueDialog({ project }: { project: ProjectItem }) {
  const t = useT();
  const queryClient = useQueryClient();
  const keys = useRetainedKey();
  const numberId = useId();
  const repoId = useId();
  const [open, setOpen] = useState(false);
  const [number, setNumber] = useState("");
  const [repo, setRepo] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const several = project.repos.length > 1;
  const parsed = /^[1-9]\d{0,9}$/.test(number.trim())
    ? Number(number.trim())
    : null;
  const blocked = pending || parsed === null || (several && repo === "");

  const change = (next: boolean) => {
    if (next) {
      setNumber("");
      setRepo("");
      setError(null);
      keys.forget();
    }
    setOpen(next);
  };

  const send = async () => {
    if (parsed === null) {
      return;
    }
    const body = { number: parsed, repo: several ? repo : null };
    const fingerprint = JSON.stringify(body);
    const key = keys.take(fingerprint);
    setPending(true);
    setError(null);
    try {
      await createIssueIntake(project.id, body, key);
      await Promise.all(
        [
          QUERY_ROOTS.issueIntakes,
          QUERY_ROOTS.inbox,
          QUERY_ROOTS.specDrafts,
        ].map((root) => queryClient.invalidateQueries({ queryKey: [root] })),
      );
      setOpen(false);
    } catch (e) {
      keys.keep(key, fingerprint, e);
      setError(e);
    } finally {
      setPending(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={change}>
      <DialogTrigger asChild>
        <Button variant="outline">{t("issues.add.trigger")}</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>{t("issues.add.title")}</DialogTitle>
        <DialogDescription>{t("issues.add.description")}</DialogDescription>
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
            <label htmlFor={numberId} className="text-sm font-medium">
              {t("issues.add.number")}
            </label>
            <Input
              id={numberId}
              inputMode="numeric"
              value={number}
              onChange={(event) => {
                setNumber(event.target.value);
              }}
            />
          </div>
          {several && (
            <div className="space-y-1">
              <label htmlFor={repoId} className="text-sm font-medium">
                {t("issues.add.repo")}
              </label>
              <select
                id={repoId}
                value={repo}
                onChange={(event) => {
                  setRepo(event.target.value);
                }}
                className="min-h-11 w-full rounded-md border border-border bg-background px-3 text-sm"
              >
                <option value="">{t("issues.add.repoChoose")}</option>
                {project.repos.map((r) => (
                  <option key={r.name} value={r.name}>
                    {r.name}
                  </option>
                ))}
              </select>
            </div>
          )}
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
              {t("issues.add.submit")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
