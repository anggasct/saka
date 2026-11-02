import type { EditorState } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import { useQueryClient } from "@tanstack/react-query";
import { useBlocker } from "@tanstack/react-router";
import { useDeferredValue, useMemo, useRef, useState } from "react";
import { useIntl } from "react-intl";

import { ApiError, newIdempotencyKey } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { Chip } from "@/editor/chips";
import { MarkdownEditor } from "@/editor/markdown-editor";
import { openDocument, saveDocument } from "@/editor/lossless";
import { outline } from "@/editor/outline";
import { useT } from "@/i18n/use-t";
import { platform } from "@/platform";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";
import { useSpecTools } from "@/wasm/use-spec-tools";

import { ConfirmDialog } from "./confirm-dialog";
import { OutlinePanel } from "./outline-panel";
import {
  docsKeys,
  fetchDocFile,
  saveFile,
  useDocFile,
  useSpecByPath,
  useSpecTasks,
  type FileContent,
  type FileWrite,
} from "./queries";
import { SpecPanel, type StepRow } from "./spec-panel";
import { contextIssues, specLintSource } from "./spec-lint";
import { chipOf, fileErrorText, saveErrorText, shortSha } from "./text";

const MODE_KEY = "docs.mode";

type Source = { content: string; blob: string | null; commit: string | null };

type Status =
  | { kind: "idle" }
  | { kind: "saving" }
  | { kind: "saved"; commit: string; committed: boolean };

function fromServer(file: FileContent): Source {
  return { content: file.content, blob: file.blob, commit: file.commit };
}

function serverSource(
  data: FileContent | undefined,
  error: unknown,
): Source | null | undefined {
  if (data !== undefined) {
    return fromServer(data);
  }
  if (error instanceof ApiError && error.status === 404) {
    return null;
  }
  return undefined;
}

function fileName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

export function FileView({
  projectId,
  path,
  branch,
  draft,
  onSaved,
}: {
  projectId: string;
  path: string;
  branch: string;
  draft: string | undefined;
  onSaved: (path: string) => void;
}) {
  const t = useT();
  const intl = useIntl();
  const translate = useMemo(
    () => (key: string, values?: Record<string, string | number>) =>
      intl.formatMessage({ id: key }, values),
    [intl],
  );
  const queryClient = useQueryClient();
  const isDraft = draft !== undefined;
  const file = useDocFile(projectId, path, !isDraft);
  const server = useMemo(
    () => (isDraft ? undefined : serverSource(file.data, file.error)),
    [isDraft, file.data, file.error],
  );

  const [base, setBase] = useState<Source | null>(() =>
    isDraft ? { content: draft, blob: null, commit: null } : null,
  );
  const [opened, setOpened] = useState<Source | null>(base);
  const [seen, setSeen] = useState<Source | null | undefined>(undefined);
  const [content, setContent] = useState<string | null>(null);
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  const [saveError, setSaveError] = useState<unknown>(null);
  const [problem, setProblem] = useState<FileWrite["spec_problem"]>(null);
  const [staleBlob, setStaleBlob] = useState<string | null | undefined>(
    undefined,
  );
  const [preview, setPreview] = useState(
    () => platform.preferences.get(MODE_KEY) !== "source",
  );
  const retained = useRef<{ key: string; content: string } | null>(null);
  const view = useRef<EditorView | null>(null);

  const dirty =
    base !== null &&
    content !== null &&
    (base.blob === null || content !== base.content);

  if (server !== seen) {
    setSeen(server);
    if (server !== undefined && (base === null || !dirty)) {
      setBase(server);
      setOpened(server);
      setStaleBlob(undefined);
    }
  }

  const serverBlob = server === undefined ? undefined : (server?.blob ?? null);
  const conflictBlob =
    staleBlob !== undefined
      ? staleBlob
      : dirty && serverBlob !== undefined && serverBlob !== base.blob
        ? serverBlob
        : undefined;

  const doc = useMemo(
    () => (opened === null ? null : openDocument(opened.content)),
    [opened],
  );

  const tools = useSpecTools();
  const deferred = useDeferredValue(content);
  const report = useMemo(
    () =>
      tools.data && deferred !== null
        ? tools.data.lint(fileName(path), deferred)
        : null,
    [tools.data, deferred, path],
  );
  const isSpec = report?.isSpec === true;
  const hash = useMemo(
    () =>
      tools.data && deferred !== null && isSpec
        ? tools.data.contentHash(deferred)
        : null,
    [tools.data, deferred, isSpec],
  );
  const spec = useSpecByPath(projectId, path, isSpec);
  const tasks = useSpecTasks(spec.data?.id);
  const serverIssues = useMemo(
    () => contextIssues(spec.data, hash),
    [spec.data, hash],
  );
  const lint = useMemo(
    () =>
      tools.data && doc
        ? specLintSource(
            tools.data,
            fileName(path),
            doc.bom,
            serverIssues,
            translate,
          )
        : null,
    [tools.data, doc, path, serverIssues, translate],
  );

  const steps: StepRow[] = useMemo(() => {
    if (!report?.isSpec) {
      return [];
    }
    const byStep = new Map(
      (spec.data?.steps ?? []).map((s) => [s.id, s.work_item_id] as const),
    );
    const byId = new Map((tasks.data ?? []).map((w) => [w.id, w] as const));
    return report.steps.map((step) => {
      const id = byStep.get(step.id);
      const task = id ? byId.get(id) : undefined;
      return {
        id: step.id,
        line: step.line,
        task: task ?? null,
        chip: task ? chipOf(translate, task) : null,
      };
    });
  }, [report, spec.data, tasks.data, translate]);

  const chips: Chip[] = useMemo(
    () =>
      steps.flatMap((s) =>
        s.chip === null ? [] : [{ line: s.line, ...s.chip }],
      ),
    [steps],
  );

  const headings = useMemo(
    () => (editor === null ? [] : outline(editor)),
    [editor],
  );

  const blocker = useBlocker({
    shouldBlockFn: () => dirty,
    enableBeforeUnload: () => dirty,
    withResolver: true,
  });

  const onChange = (state: EditorState) => {
    setEditor(state);
    setContent(saveDocument(doc?.bom ?? false, state));
    if (status.kind === "saved") {
      setStatus({ kind: "idle" });
    }
  };

  const save = async (expect: string | null) => {
    if (content === null || status.kind === "saving") {
      return;
    }
    const sent = content;
    const kept = retained.current;
    // The server stores every answer under its key, so only a request that never
    // reached it may be sent again with the same key.
    const key =
      kept !== null && kept.content === sent ? kept.key : newIdempotencyKey();
    retained.current = null;
    setStatus({ kind: "saving" });
    setSaveError(null);
    let written: FileWrite;
    try {
      written = await saveFile({
        projectId,
        path,
        content: sent,
        blob: expect,
        idempotencyKey: key,
      });
    } catch (error) {
      setStatus({ kind: "idle" });
      if (error instanceof ApiError && error.code === "file_stale") {
        const blob = error.params.blob;
        setStaleBlob(typeof blob === "string" ? blob : null);
      } else {
        setSaveError(error);
      }
      if (!(error instanceof ApiError)) {
        retained.current = { key, content: sent };
      }
      return;
    }
    const savedSource = {
      content: sent,
      blob: written.blob,
      commit: written.commit,
    };
    setBase(savedSource);
    setSeen(savedSource);
    setStaleBlob(undefined);
    setProblem(written.spec_problem ?? null);
    setStatus({
      kind: "saved",
      commit: written.commit,
      committed: written.committed,
    });
    queryClient.setQueryData<FileContent>(docsKeys.file(projectId, path), {
      path,
      commit: written.commit,
      blob: written.blob,
      bytes: written.bytes,
      content: sent,
    });
    if (written.spec) {
      queryClient.setQueryData(
        docsKeys.specByPath(projectId, path),
        written.spec,
      );
    }
    void queryClient.invalidateQueries({ queryKey: docsKeys.tree(projectId) });
    void queryClient.invalidateQueries({
      queryKey: [QUERY_ROOTS.specs],
      predicate: (q) =>
        JSON.stringify(q.queryKey) !==
        JSON.stringify(docsKeys.specByPath(projectId, path)),
    });
    onSaved(path);
  };

  const takeTheirs = async () => {
    try {
      const latest = await queryClient.query({
        queryKey: docsKeys.file(projectId, path),
        queryFn: ({ signal }) => fetchDocFile(projectId, path, signal),
        staleTime: 0,
      });
      const source = fromServer(latest);
      setBase(source);
      setOpened({ ...source });
      setSeen(source);
    } catch (error) {
      if (error instanceof ApiError && error.status === 404) {
        setBase(null);
        setOpened(null);
        setSeen(null);
      } else {
        setSaveError(error);
        return;
      }
    }
    setStaleBlob(undefined);
    setStatus({ kind: "idle" });
  };

  const moveTo = (from: number) => {
    const current = view.current;
    if (current !== null) {
      current.dispatch({ selection: { anchor: from }, scrollIntoView: true });
      current.focus();
    }
  };

  const togglePreview = () => {
    const next = !preview;
    setPreview(next);
    platform.preferences.set(MODE_KEY, next ? "preview" : "source");
  };

  const leaveDialog = (
    <ConfirmDialog
      open={blocker.status === "blocked"}
      title={t("docs.leave.title")}
      body={t("docs.leave.body", { path })}
      cancel={t("docs.leave.stay")}
      confirm={t("docs.leave.discard")}
      onCancel={() => blocker.reset?.()}
      onConfirm={() => blocker.proceed?.()}
    />
  );

  if (doc === null || base === null) {
    return (
      <>
        <div className="min-w-0">
          <FileState
            loading={!isDraft && server === undefined && file.isPending}
            error={server === undefined ? file.error : null}
            missing={server === null}
            path={path}
            branch={branch}
            onRetry={() => void file.refetch()}
          />
        </div>
        <div />
      </>
    );
  }

  const statusText = (() => {
    if (status.kind === "saving") {
      return t("docs.status.saving");
    }
    if (dirty) {
      return t("docs.status.dirty");
    }
    if (status.kind === "saved") {
      return status.committed
        ? t("docs.status.saved", { commit: shortSha(status.commit) })
        : t("docs.status.unchanged");
    }
    return null;
  })();

  const approved = spec.data?.approved_revision;
  const changedSince =
    approved && hash !== null && hash !== approved.content_hash
      ? approved.source_commit
      : null;

  return (
    <>
      <div className="min-w-0 space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="min-w-0 flex-1 truncate font-mono text-sm">{path}</h2>
          <Button
            variant="outline"
            aria-pressed={!preview}
            onClick={togglePreview}
          >
            {t("docs.mode.source")}
          </Button>
          <Button
            disabled={!dirty || status.kind === "saving"}
            onClick={() => void save(base.blob)}
          >
            {t("docs.save")}
          </Button>
        </div>
        <p role="status" className="min-h-5 text-sm text-muted-foreground">
          {statusText}
        </p>
        {conflictBlob !== undefined && (
          <Card role="alert" className="space-y-3 border-attention p-3">
            <p className="text-sm">
              {conflictBlob === null
                ? t("docs.conflict.deleted", { branch })
                : t("docs.conflict.changed", { branch })}
            </p>
            <div className="flex flex-wrap gap-2">
              <Button onClick={() => void save(conflictBlob)}>
                {t("docs.conflict.keep")}
              </Button>
              <ConfirmDialog
                trigger={t("docs.conflict.take")}
                title={t("docs.conflict.confirmTitle")}
                body={t("docs.conflict.confirmBody", { branch })}
                cancel={t("dialog.cancel")}
                confirm={t("docs.conflict.take")}
                onConfirm={() => void takeTheirs()}
              />
            </div>
          </Card>
        )}
        {saveError !== null && (
          <p role="alert" className="text-sm text-danger-foreground">
            {saveErrorText(t, saveError)}
          </p>
        )}
        {changedSince !== null && (
          <p className="rounded-md border border-border bg-muted p-3 text-sm">
            {t("docs.changed", { commit: shortSha(changedSince) })}
          </p>
        )}
        <div className="rounded-md border border-border">
          <MarkdownEditor
            document={doc}
            label={t("docs.editor.label", { path })}
            preview={preview}
            taskLabel={t("docs.editor.task")}
            lint={lint}
            chips={chips}
            onChange={onChange}
            onSave={() => {
              if (dirty) {
                void save(
                  conflictBlob !== undefined ? conflictBlob : base.blob,
                );
              }
            }}
            onView={(v) => {
              view.current = v;
            }}
          />
        </div>
      </div>
      <aside className="min-w-0 space-y-4">
        <OutlinePanel headings={headings} onSelect={moveTo} />
        {report?.isSpec === true && (
          <SpecPanel
            spec={spec.data}
            loading={spec.isPending}
            localIssues={report.issues}
            serverIssues={serverIssues}
            steps={steps}
            problem={problem}
          />
        )}
        {tools.isError && (
          <p className="text-sm text-muted-foreground">
            {t("docs.panel.checksUnavailable")}
          </p>
        )}
      </aside>
      {leaveDialog}
    </>
  );
}

function FileState({
  loading,
  error,
  missing,
  path,
  branch,
  onRetry,
}: {
  loading: boolean;
  error: unknown;
  missing: boolean;
  path: string;
  branch: string;
  onRetry: () => void;
}) {
  const t = useT();
  if (loading) {
    return (
      <div role="status" aria-busy="true" aria-label={t("docs.file.loading")}>
        <Skeleton className="h-64" />
      </div>
    );
  }
  if (missing) {
    return (
      <Card className="space-y-2 p-4">
        <h2 className="font-semibold">{t("docs.file.notFound")}</h2>
        <p className="text-sm text-muted-foreground">
          {t("docs.file.notFoundBody", { path, branch })}
        </p>
      </Card>
    );
  }
  if (error instanceof ApiError && error.code === "file_unsupported") {
    const reason = String(error.params.reason);
    const body =
      reason === "too_large"
        ? t("docs.file.too_large", {
            bytes: Number(error.params.bytes),
            max: Number(error.params.max_bytes),
          })
        : reason === "not_utf8"
          ? t("docs.file.not_utf8")
          : t("docs.file.not_regular");
    return (
      <Card className="space-y-2 p-4">
        <h2 className="font-semibold">{t("docs.file.unsupported")}</h2>
        <p className="text-sm text-muted-foreground">{body}</p>
      </Card>
    );
  }
  return (
    <Card role="alert" className="space-y-3 p-4">
      <h2 className="font-semibold">{t("docs.file.error")}</h2>
      <p className="text-sm text-muted-foreground">
        {error instanceof ApiError
          ? fileErrorText(t, error.code, error.params)
          : fileErrorText(t, "network_error", {})}
      </p>
      <Button onClick={onRetry}>{t("delivery.error.retry")}</Button>
    </Card>
  );
}
