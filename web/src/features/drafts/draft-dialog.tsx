import { useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useId, useRef, useState } from "react";

import { ApiError, newIdempotencyKey } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import { fieldParams } from "@/features/delivery/action-dialog";
import { errorText, text } from "@/features/delivery/text";
import { checkNewPath, folderOf } from "@/features/docs/new-file-dialog";
import { useLocale } from "@/i18n/locale-context";
import { LOCALES, type Locale } from "@/i18n/messages";
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

import { createDraft, type CreateSpecDraftBody } from "./queries";

export const IDEA_MAX = 8000;
export const CONTEXT_MAX = 16;

type Problems = {
  idea: "empty" | "too_long" | null;
  path: "empty" | "invalid" | "exists" | null;
  context: boolean;
};

const NO_PROBLEMS: Problems = { idea: null, path: null, context: false };

function withExtension(typed: string, folder: string): string {
  return typed.toLowerCase().endsWith(".md") || typed === folder
    ? typed
    : `${typed}.md`;
}

function sameBody(a: CreateSpecDraftBody, b: CreateSpecDraftBody): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

export function DraftDialog({
  projectId,
  projectPath,
  branch,
  files,
  existing,
  triggerVariant = "outline",
}: {
  projectId: string;
  projectPath: string;
  branch: string;
  files: readonly string[];
  existing: ReadonlySet<string>;
  triggerVariant?: "default" | "outline";
}) {
  const t = useT();
  const { locale: uiLocale } = useLocale();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const folder = folderOf(projectPath, "spec");
  const [open, setOpen] = useState(false);
  const [idea, setIdea] = useState("");
  const [path, setPath] = useState(folder);
  const [context, setContext] = useState<readonly string[]>([]);
  const [locale, setLocale] = useState<Locale>(uiLocale);
  const [problems, setProblems] = useState<Problems>(NO_PROBLEMS);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const retained = useRef<{ key: string; body: CreateSpecDraftBody } | null>(
    null,
  );
  const ideaId = useId();
  const ideaHelpId = useId();
  const ideaErrorId = useId();
  const pathId = useId();
  const pathErrorId = useId();
  const contextHelpId = useId();
  const contextErrorId = useId();
  const localeId = useId();
  const markdown = files.filter((f) => f.toLowerCase().endsWith(".md"));

  const change = (next: boolean) => {
    if (next) {
      setIdea("");
      setPath(folder);
      setContext([]);
      setLocale(uiLocale);
      setProblems(NO_PROBLEMS);
      setError(null);
      retained.current = null;
    }
    setOpen(next);
  };

  const submit = async () => {
    const target = withExtension(path.trim(), folder);
    const found: Problems = {
      idea:
        idea.trim() === ""
          ? "empty"
          : idea.length > IDEA_MAX
            ? "too_long"
            : null,
      path: checkNewPath(target, folder, existing),
      context: context.length > CONTEXT_MAX,
    };
    setProblems(found);
    if (found.idea !== null || found.path !== null || found.context) {
      return;
    }
    const body: CreateSpecDraftBody = {
      idea,
      path: target,
      context_paths: [...context],
      locale,
    };
    const kept = retained.current;
    // The server stores every answer under its key, so only a request that never
    // reached it may be sent again with the same key.
    const key =
      kept !== null && sameBody(kept.body, body)
        ? kept.key
        : newIdempotencyKey();
    retained.current = null;
    setPending(true);
    setError(null);
    try {
      const draft = await createDraft(projectId, body, key);
      setOpen(false);
      void queryClient.invalidateQueries({
        queryKey: [QUERY_ROOTS.specDrafts],
      });
      await navigate({
        to: "/docs/$projectId/drafts/$draftId",
        params: { projectId, draftId: draft.id },
      });
    } catch (e) {
      setError(e);
      if (!(e instanceof ApiError)) {
        retained.current = { key, body };
      }
    } finally {
      setPending(false);
    }
  };

  const toggle = (file: string, checked: boolean) => {
    setContext((current) =>
      checked ? [...current, file] : current.filter((c) => c !== file),
    );
    setProblems((current) => ({ ...current, context: false }));
  };

  const ideaMessage =
    problems.idea === "empty"
      ? t("draft.new.ideaEmpty")
      : problems.idea === "too_long"
        ? t("draft.new.ideaTooLong", { max: IDEA_MAX })
        : null;
  const pathMessage =
    problems.path === "empty"
      ? t("docs.new.empty")
      : problems.path === "invalid"
        ? t("docs.new.invalid", { folder })
        : problems.path === "exists"
          ? t("docs.new.exists", { path: withExtension(path.trim(), folder) })
          : null;

  return (
    <Dialog open={open} onOpenChange={change}>
      <DialogTrigger asChild>
        <Button variant={triggerVariant}>{t("draft.new.trigger")}</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>{t("draft.new.title")}</DialogTitle>
        <DialogDescription>
          {t("draft.new.description", { branch })}
        </DialogDescription>
        <form
          noValidate
          className="space-y-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (!pending) {
              void submit();
            }
          }}
        >
          <div className="space-y-1">
            <label htmlFor={ideaId} className="text-sm font-medium">
              {t("draft.new.idea")}
            </label>
            <textarea
              id={ideaId}
              value={idea}
              rows={5}
              required
              aria-invalid={ideaMessage !== null}
              aria-describedby={
                ideaMessage === null
                  ? ideaHelpId
                  : `${ideaHelpId} ${ideaErrorId}`
              }
              onChange={(event) => {
                setIdea(event.target.value);
                setProblems((current) => ({ ...current, idea: null }));
              }}
              className="w-full rounded-md border border-border bg-background p-2 text-sm"
            />
            <p id={ideaHelpId} className="text-xs text-muted-foreground">
              {t("draft.new.ideaLeft", { count: IDEA_MAX - idea.length })}
            </p>
            {ideaMessage !== null && (
              <p
                id={ideaErrorId}
                role="alert"
                className="text-sm text-danger-foreground"
              >
                {ideaMessage}
              </p>
            )}
          </div>
          <div className="space-y-1">
            <label htmlFor={pathId} className="text-sm font-medium">
              {t("docs.new.path")}
            </label>
            <input
              id={pathId}
              value={path}
              required
              aria-invalid={pathMessage !== null}
              aria-describedby={pathMessage === null ? undefined : pathErrorId}
              onChange={(event) => {
                setPath(event.target.value);
                setProblems((current) => ({ ...current, path: null }));
              }}
              className="min-h-11 w-full rounded-md border border-border bg-background px-2 font-mono text-sm"
            />
            {pathMessage !== null && (
              <p
                id={pathErrorId}
                role="alert"
                className="text-sm text-danger-foreground"
              >
                {pathMessage}
              </p>
            )}
          </div>
          <fieldset
            className="space-y-1"
            aria-describedby={
              problems.context
                ? `${contextHelpId} ${contextErrorId}`
                : contextHelpId
            }
          >
            <legend className="text-sm font-medium">
              {t("draft.new.context")}
            </legend>
            <p id={contextHelpId} className="text-xs text-muted-foreground">
              {t("draft.new.contextCount", {
                count: context.length,
                max: CONTEXT_MAX,
              })}
            </p>
            {markdown.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                {t("draft.new.contextNone")}
              </p>
            ) : (
              <ul className="max-h-48 space-y-0 overflow-y-auto rounded-md border border-border p-1">
                {markdown.map((file) => (
                  <li key={file}>
                    <label className="flex min-h-11 items-center gap-2 px-1 text-sm break-all">
                      <input
                        type="checkbox"
                        className="size-6 shrink-0"
                        checked={context.includes(file)}
                        onChange={(event) => {
                          toggle(file, event.target.checked);
                        }}
                      />
                      <span className="font-mono">{file}</span>
                    </label>
                  </li>
                ))}
              </ul>
            )}
            {problems.context && (
              <p
                id={contextErrorId}
                role="alert"
                className="text-sm text-danger-foreground"
              >
                {t("draft.new.contextTooMany", { max: CONTEXT_MAX })}
              </p>
            )}
          </fieldset>
          <div className="space-y-1">
            <label htmlFor={localeId} className="text-sm font-medium">
              {t("draft.new.language")}
            </label>
            <select
              id={localeId}
              value={locale}
              onChange={(event) => {
                const next = LOCALES.find((l) => l === event.target.value);
                if (next !== undefined) {
                  setLocale(next);
                }
              }}
              className="block min-h-11 w-full rounded-md border border-border bg-background px-2 text-sm"
            >
              <option value="en" lang="en">
                {t("language.en")}
              </option>
              <option value="id" lang="id">
                {t("language.id")}
              </option>
            </select>
          </div>
          {error !== null && (
            <CreateError
              error={error}
              projectId={projectId}
              onFollow={() => {
                setOpen(false);
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
              {t("draft.new.submit")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function CreateError({
  error,
  projectId,
  onFollow,
}: {
  error: unknown;
  projectId: string;
  onFollow: () => void;
}) {
  const t = useT();
  if (!(error instanceof ApiError)) {
    return (
      <p role="alert" className="text-sm text-danger-foreground">
        {errorText(t, "network_error")}
      </p>
    );
  }
  if (error.code !== "spec_draft_target_taken") {
    return (
      <p role="alert" className="text-sm text-danger-foreground">
        {errorText(t, error.code, fieldParams(error.params))}
      </p>
    );
  }
  const params = error.params;
  const draftId = text(params.spec_draft_id);
  return (
    <div role="alert" className="space-y-1 text-sm text-danger-foreground">
      <p>
        {t("draft.taken", {
          reason: text(params.reason) || "other",
          path: text(params.path),
        })}
      </p>
      {text(params.reason) === "draft_active" && draftId !== "" && (
        <Link
          to="/docs/$projectId/drafts/$draftId"
          params={{ projectId, draftId }}
          onClick={onFollow}
          className="inline-flex min-h-11 items-center font-medium text-primary underline underline-offset-4"
        >
          {t("draft.taken.open")}
        </Link>
      )}
    </div>
  );
}
