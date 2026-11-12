import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useId, useState } from "react";

import { QUERY_ROOTS } from "@/api/events";
import { useRetainedKey } from "@/api/use-keyed-run";
import {
  DEFAULT_KIND,
  WORK_KINDS,
  type WorkKind,
} from "@/features/delivery/kinds";
import type { ProjectItem } from "@/features/delivery/queries";
import { label } from "@/features/delivery/text";
import { folderOf } from "@/features/docs/new-file-dialog";
import { useLocale } from "@/i18n/locale-context";
import { useT } from "@/i18n/use-t";
import type { QuickInput } from "@/wasm/spec-tools";
import { useSpecTools } from "@/wasm/use-spec-tools";
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
import { RadioGroup } from "@/ui/radio-group";
import { Textarea } from "@/ui/textarea";

import { createQuickSpec, type QuickSpecBody } from "./queries";
import {
  issueText,
  lintLines,
  needsStartApproval,
  quickErrorLines,
} from "./text";

function lines(value: string): string[] {
  return value
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

type Fields = {
  kind: WorkKind;
  title: string;
  description: string;
  doneWhen: string;
  regression: string;
  criteria: string;
  repo: string;
};

const EMPTY: Fields = {
  kind: DEFAULT_KIND,
  title: "",
  description: "",
  doneWhen: "",
  regression: "",
  criteria: "",
  repo: "",
};

function Field({
  id,
  label: text,
  hint,
  problems,
  children,
}: {
  id: string;
  label: string;
  hint?: string;
  problems: readonly string[];
  children: React.ReactNode;
}) {
  return (
    <div className="space-y-1">
      <label htmlFor={id} className="text-sm font-medium">
        {text}
      </label>
      {children}
      {hint !== undefined && (
        <p id={`${id}-hint`} className="text-xs text-muted-foreground">
          {hint}
        </p>
      )}
      {problems.length > 0 && (
        <ul
          id={`${id}-problems`}
          role="alert"
          className="text-sm text-danger-foreground"
        >
          {problems.map((problem) => (
            <li key={problem}>{problem}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function QuickDialog({
  project,
  triggerVariant = "outline",
}: {
  project: ProjectItem;
  triggerVariant?: "default" | "outline";
}) {
  const projectId = project.id;
  const projectPath = project.workspace.project_path;
  const branch = project.workspace.base_branch;
  const repos = project.repos.map((r) => r.name);
  const t = useT();
  const { locale } = useLocale();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const tools = useSpecTools();
  const keys = useRetainedKey();
  const [open, setOpen] = useState(false);
  const [fields, setFields] = useState<Fields>(EMPTY);
  const [attempted, setAttempted] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const kindName = useId();
  const titleId = useId();
  const descriptionId = useId();
  const doneWhenId = useId();
  const regressionId = useId();
  const criteriaId = useId();
  const repoId = useId();
  const folder = folderOf(projectPath, "spec");

  const input: QuickInput = {
    kind: fields.kind,
    title: fields.title,
    description: fields.description,
    done_when: lines(fields.doneWhen),
    regression: lines(fields.regression),
    ac: lines(fields.criteria),
    locale,
    repo: fields.repo,
  };
  const preview = tools.data?.quickPreview(input);
  const bugfix = fields.kind === "bugfix";
  const strict = needsStartApproval(project, fields.kind, fields.repo);

  const problemsOf = (...names: string[]): string[] =>
    !attempted || preview === undefined
      ? []
      : preview.issues
          .filter((issue) =>
            names.some(
              (name) =>
                issue.field === name || issue.field.startsWith(`${name}[`),
            ),
          )
          .map((issue) => issueText(t, issue));
  const structureProblems =
    attempted && preview !== undefined && !preview.structure
      ? [t("quick.error.structure")]
      : [];
  const lintProblems = preview === undefined ? [] : lintLines(t, preview.lint);

  const set = (patch: Partial<Fields>) => {
    setFields((current) => ({ ...current, ...patch }));
    setError(null);
  };

  const change = (next: boolean) => {
    if (next) {
      setFields(EMPTY);
      setAttempted(false);
      setError(null);
      keys.forget();
    }
    setOpen(next);
  };

  const submit = async (approve: boolean) => {
    setAttempted(true);
    if (
      preview === undefined ||
      !preview.valid ||
      preview.issues.length > 0 ||
      !preview.structure ||
      lintProblems.length > 0 ||
      pending
    ) {
      return;
    }
    const body: QuickSpecBody = {
      kind: input.kind,
      title: input.title.trim(),
      description: input.description,
      done_when: [...input.done_when],
      regression: [...input.regression],
      ac: [...input.ac],
      locale: input.locale,
      repo: input.repo === "" ? null : input.repo,
      approve,
    };
    const fingerprint = JSON.stringify(body);
    const key = keys.take(fingerprint);
    setPending(true);
    setError(null);
    try {
      const created = await createQuickSpec(projectId, body, key);
      setOpen(false);
      for (const root of [
        QUERY_ROOTS.specs,
        QUERY_ROOTS.workItems,
        QUERY_ROOTS.docs,
      ]) {
        void queryClient.invalidateQueries({ queryKey: [root] });
      }
      await navigate({
        to: "/docs/$projectId",
        params: { projectId },
        search: { path: created.path },
      });
    } catch (e) {
      setError(e);
      keys.keep(key, fingerprint, e);
    } finally {
      setPending(false);
    }
  };

  const target = preview?.slug == null ? null : `${folder}${preview.slug}.md`;

  return (
    <Dialog open={open} onOpenChange={change}>
      <DialogTrigger asChild>
        <Button variant={triggerVariant}>{t("quick.trigger")}</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>{t("quick.title")}</DialogTitle>
        <DialogDescription>
          {t("quick.description", { branch })}
        </DialogDescription>
        <form
          noValidate
          className="space-y-4"
          onSubmit={(event) => {
            event.preventDefault();
            void submit(true);
          }}
        >
          <RadioGroup
            legend={t("quick.field.kind")}
            name={kindName}
            value={fields.kind}
            options={WORK_KINDS.map((value) => ({
              value,
              label: label(t, "kind", value),
              description: label(t, "kind.hint", value),
            }))}
            onChange={(next) => {
              set({
                kind: WORK_KINDS.find((k) => k === next) ?? DEFAULT_KIND,
              });
            }}
          />
          <Field
            id={titleId}
            label={t("quick.field.title")}
            hint={t("quick.titleHint")}
            problems={problemsOf("title")}
          >
            <Input
              id={titleId}
              value={fields.title}
              required
              aria-invalid={problemsOf("title").length > 0}
              aria-describedby={`${titleId}-hint`}
              onChange={(event) => {
                set({ title: event.target.value });
              }}
            />
          </Field>
          <Field
            id={descriptionId}
            label={t("quick.descriptionLabel")}
            problems={[...problemsOf("description"), ...structureProblems]}
          >
            <Textarea
              id={descriptionId}
              rows={3}
              value={fields.description}
              onChange={(event) => {
                set({ description: event.target.value });
              }}
            />
          </Field>
          <Field
            id={doneWhenId}
            label={t("quick.field.done_when")}
            hint={t("quick.doneWhenHint")}
            problems={problemsOf("done_when")}
          >
            <Textarea
              id={doneWhenId}
              rows={3}
              required
              className="font-mono"
              value={fields.doneWhen}
              aria-describedby={`${doneWhenId}-hint`}
              onChange={(event) => {
                set({ doneWhen: event.target.value });
              }}
            />
          </Field>
          {bugfix && (
            <Field
              id={regressionId}
              label={t("quick.field.regression")}
              hint={t("quick.regressionHint")}
              problems={problemsOf("regression")}
            >
              <Textarea
                id={regressionId}
                rows={2}
                required
                className="font-mono"
                value={fields.regression}
                aria-describedby={`${regressionId}-hint`}
                onChange={(event) => {
                  set({ regression: event.target.value });
                }}
              />
            </Field>
          )}
          <Field
            id={criteriaId}
            label={t("quick.criteriaLabel")}
            hint={t("quick.criteriaHint")}
            problems={problemsOf("ac")}
          >
            <Textarea
              id={criteriaId}
              rows={2}
              value={fields.criteria}
              aria-describedby={`${criteriaId}-hint`}
              onChange={(event) => {
                set({ criteria: event.target.value });
              }}
            />
          </Field>
          {repos.length > 1 && (
            <Field
              id={repoId}
              label={t("quick.field.repo")}
              problems={problemsOf("repo")}
            >
              <select
                id={repoId}
                value={fields.repo}
                onChange={(event) => {
                  set({ repo: event.target.value });
                }}
                className="block min-h-11 w-full rounded-md border border-border bg-background px-2 text-sm"
              >
                <option value="">{t("quick.repoDefault")}</option>
                {repos.map((name) => (
                  <option key={name} value={name}>
                    {name}
                  </option>
                ))}
              </select>
            </Field>
          )}
          <section aria-label={t("quick.preview")} className="space-y-1">
            <h3 className="text-sm font-medium">{t("quick.preview")}</h3>
            {preview === undefined ||
            fields.title.trim() === "" ||
            input.done_when.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                {t("quick.previewLoading")}
              </p>
            ) : (
              <>
                {target !== null && (
                  <p className="text-xs break-all text-muted-foreground">
                    {t("quick.previewPath", { path: target })}
                  </p>
                )}
                <Textarea
                  readOnly
                  rows={10}
                  value={preview.text}
                  aria-label={t("quick.previewFile")}
                  className="font-mono text-xs"
                />
              </>
            )}
            {attempted && lintProblems.length > 0 && (
              <ul role="alert" className="text-sm text-danger-foreground">
                {lintProblems.map((problem) => (
                  <li key={problem}>{problem}</li>
                ))}
              </ul>
            )}
          </section>
          {error !== null && (
            <ul role="alert" className="text-sm text-danger-foreground">
              {quickErrorLines(t, error).map((line) => (
                <li key={line}>{line}</li>
              ))}
            </ul>
          )}
          <div className="flex flex-wrap justify-end gap-2">
            <DialogClose asChild>
              <Button type="button" variant="outline">
                {t("dialog.cancel")}
              </Button>
            </DialogClose>
            <Button
              type="button"
              variant="outline"
              disabled={pending}
              onClick={() => {
                void submit(false);
              }}
            >
              {t("quick.createOnly")}
            </Button>
            <Button type="submit" disabled={pending}>
              {strict ? t("quick.createApprove") : t("quick.create")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
