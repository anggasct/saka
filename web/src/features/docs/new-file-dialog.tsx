import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";

import { useLocale } from "@/i18n/locale-context";
import {
  DEFAULT_KIND,
  WORK_KINDS,
  type WorkKind,
} from "@/features/delivery/kinds";
import { label } from "@/features/delivery/text";
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
import { RadioGroup } from "@/ui/radio-group";

import { docsKeys, fetchTemplate } from "./queries";

export type NewKind = "file" | "spec";

export function folderOf(projectPath: string, kind: NewKind): string {
  const base = projectPath === "." ? "" : `${projectPath}/`;
  return kind === "spec" ? `${base}specs/` : base;
}

type Problem = "empty" | "invalid" | "exists";

export function checkNewPath(
  path: string,
  folder: string,
  existing: ReadonlySet<string>,
): Problem | null {
  const name = path.startsWith(folder) ? path.slice(folder.length) : null;
  if (name === null || name === "" || name === ".md") {
    return name === null ? "invalid" : "empty";
  }
  const segments = path.split("/");
  if (
    !path.toLowerCase().endsWith(".md") ||
    segments.some(
      (s) => s === "" || s === "." || s === ".." || s.toLowerCase() === ".git",
    ) ||
    path.includes("\\")
  ) {
    return "invalid";
  }
  return existing.has(path) ? "exists" : null;
}

export function NewFileDialog({
  kind,
  projectPath,
  branch,
  existing,
  onCreate,
}: {
  kind: NewKind;
  projectPath: string;
  branch: string;
  existing: ReadonlySet<string>;
  onCreate: (path: string, content: string) => void;
}) {
  const t = useT();
  const { locale } = useLocale();
  const queryClient = useQueryClient();
  const folder = folderOf(projectPath, kind);
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState(folder);
  const [problem, setProblem] = useState<Problem | "template" | null>(null);
  const [pending, setPending] = useState(false);
  const [workKind, setWorkKind] = useState<WorkKind>(DEFAULT_KIND);
  const fieldId = useId();
  const kindName = useId();
  const errorId = useId();
  const title = t(kind === "spec" ? "docs.new.spec" : "docs.new.file");

  const submit = async () => {
    const typed = value.trim();
    const path =
      typed.toLowerCase().endsWith(".md") || typed === folder
        ? typed
        : `${typed}.md`;
    const found = checkNewPath(path, folder, existing);
    if (found !== null) {
      setProblem(found);
      return;
    }
    let content = "";
    if (kind === "spec") {
      setPending(true);
      try {
        content = await queryClient.query({
          queryKey: docsKeys.template(locale, workKind),
          queryFn: () => fetchTemplate(locale, workKind),
          staleTime: Infinity,
        });
      } catch {
        setProblem("template");
        return;
      } finally {
        setPending(false);
      }
    }
    setOpen(false);
    onCreate(path, content);
  };

  const message =
    problem === null
      ? null
      : problem === "empty"
        ? t("docs.new.empty")
        : problem === "invalid"
          ? t("docs.new.invalid", { folder })
          : problem === "exists"
            ? t("docs.new.exists", { path: value.trim() })
            : t("docs.new.templateError");

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (next) {
          setValue(folder);
          setProblem(null);
          setWorkKind(DEFAULT_KIND);
        }
        setOpen(next);
      }}
    >
      <DialogTrigger asChild>
        <Button variant="outline">{title}</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>
          {t("docs.new.description", { branch })}
        </DialogDescription>
        <form
          className="space-y-3"
          onSubmit={(event) => {
            event.preventDefault();
            void submit();
          }}
        >
          <div className="space-y-1">
            <label htmlFor={fieldId} className="text-sm font-medium">
              {t("docs.new.path")}
            </label>
            <input
              id={fieldId}
              value={value}
              aria-invalid={problem !== null}
              aria-describedby={message === null ? undefined : errorId}
              onChange={(event) => {
                setValue(event.target.value);
                setProblem(null);
              }}
              className="min-h-11 w-full rounded-md border border-border bg-background px-2 font-mono text-sm"
            />
            {message !== null && (
              <p
                id={errorId}
                role="alert"
                className="text-sm text-danger-foreground"
              >
                {message}
              </p>
            )}
          </div>
          {kind === "spec" && (
            <RadioGroup
              legend={t("docs.new.kind")}
              name={kindName}
              value={workKind}
              options={WORK_KINDS.map((value) => ({
                value,
                label: label(t, "kind", value),
                description: label(t, "kind.hint", value),
              }))}
              onChange={(next) => {
                setWorkKind(WORK_KINDS.find((k) => k === next) ?? DEFAULT_KIND);
              }}
            />
          )}
          <div className="flex flex-wrap justify-end gap-2">
            <DialogClose asChild>
              <Button type="button" variant="outline">
                {t("dialog.cancel")}
              </Button>
            </DialogClose>
            <Button type="submit" disabled={pending}>
              {t("docs.new.open")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
