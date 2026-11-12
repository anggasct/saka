import { useNavigate } from "@tanstack/react-router";
import { useMemo, useState } from "react";

import { ApiError } from "@/api/client";
import { PageError } from "@/features/delivery/page-error";
import { useProjects } from "@/features/delivery/queries";
import { DraftDialog } from "@/features/drafts/draft-dialog";
import { DraftsLink } from "@/features/drafts/drafts-link";
import { useDraftContent } from "@/features/drafts/queries";
import { QuickDialog } from "@/features/quick/quick-dialog";
import { useT } from "@/i18n/use-t";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import { FileTree } from "./file-tree";
import { FileView } from "./file-view";
import { NewFileDialog } from "./new-file-dialog";
import { useDocTree } from "./queries";

type DraftBuffer =
  | { kind: "none" }
  | { kind: "loading" }
  | { kind: "exists" }
  | { kind: "missing" }
  | { kind: "error"; error: unknown };

export function DocsPage({
  projectId,
  path,
  draftId,
}: {
  projectId: string;
  path: string | undefined;
  draftId?: string | undefined;
}) {
  const t = useT();
  const navigate = useNavigate();
  const projects = useProjects();
  const tree = useDocTree(projectId);
  const [buffers, setBuffers] = useState<Record<string, string>>({});
  const [loadedDrafts, setLoadedDrafts] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const [filesOpen, setFilesOpen] = useState(false);
  const [shownPath, setShownPath] = useState(path);
  if (shownPath !== path) {
    setShownPath(path);
    setFilesOpen(false);
  }
  const project = projects.data?.find((p) => p.id === projectId);
  const projectPath =
    tree.data?.project_path ?? project?.workspace.project_path;
  const branch = tree.data?.base_branch ?? project?.workspace.base_branch ?? "";
  const existing = useMemo(
    () =>
      new Set([
        ...(tree.data?.files ?? []).map((f) => f.path),
        ...Object.keys(buffers),
      ]),
    [tree.data, buffers],
  );
  const files = useMemo(
    () => (tree.data?.files ?? []).map((f) => f.path),
    [tree.data],
  );

  const draftKey =
    draftId === undefined || path === undefined ? null : `${draftId}\n${path}`;
  const inTree = path !== undefined && files.includes(path);
  const wantsDraft =
    draftKey !== null &&
    tree.data !== undefined &&
    !inTree &&
    !loadedDrafts.has(draftKey) &&
    path !== undefined &&
    !(path in buffers);
  const draftContent = useDraftContent(draftId ?? "", wantsDraft);
  if (wantsDraft && draftContent.data?.path === path) {
    const content = draftContent.data.content;
    setLoadedDrafts((current) => new Set([...current, draftKey]));
    setBuffers((current) => ({ ...current, [path]: content }));
  }
  const draftBuffer: DraftBuffer = (() => {
    if (
      draftKey === null ||
      loadedDrafts.has(draftKey) ||
      (path !== undefined && path in buffers)
    ) {
      return { kind: "none" };
    }
    if (tree.data === undefined) {
      return tree.error === null ? { kind: "loading" } : { kind: "none" };
    }
    if (inTree) {
      return { kind: "exists" };
    }
    if (!wantsDraft) {
      return { kind: "none" };
    }
    if (draftContent.error !== null) {
      return draftContent.error instanceof ApiError &&
        draftContent.error.status === 404
        ? { kind: "missing" }
        : { kind: "error", error: draftContent.error };
    }
    return draftContent.data !== undefined && draftContent.data.path !== path
      ? { kind: "missing" }
      : { kind: "loading" };
  })();

  const create = (newPath: string, content: string) => {
    setBuffers((current) => ({ ...current, [newPath]: content }));
    void navigate({
      to: "/docs/$projectId",
      params: { projectId },
      search: { path: newPath },
    });
  };

  const saved = (savedPath: string) => {
    setBuffers((current) => {
      if (!(savedPath in current)) {
        return current;
      }
      return Object.fromEntries(
        Object.entries(current).filter(([key]) => key !== savedPath),
      );
    });
  };

  const treePanel = (
    <nav aria-labelledby="docs-files" className="space-y-3">
      <h2 id="docs-files" className="text-sm font-semibold">
        {t("docs.tree.title")}
      </h2>
      {projectPath !== undefined && (
        <div className="flex flex-wrap gap-2">
          <NewFileDialog
            kind="file"
            projectPath={projectPath}
            branch={branch}
            existing={existing}
            onCreate={create}
          />
          <NewFileDialog
            kind="spec"
            projectPath={projectPath}
            branch={branch}
            existing={existing}
            onCreate={create}
          />
          <DraftDialog
            projectId={projectId}
            projectPath={projectPath}
            branch={branch}
            files={files}
            existing={existing}
          />
          {project !== undefined && <QuickDialog project={project} />}
          <DraftsLink projectId={projectId} />
        </div>
      )}
      {tree.error !== null ? (
        <PageError
          title="docs.tree.error"
          error={tree.error}
          onRetry={() => void tree.refetch()}
        />
      ) : tree.isPending ? (
        <div role="status" aria-busy="true" aria-label={t("docs.tree.loading")}>
          <Skeleton className="h-40" />
        </div>
      ) : (
        <FileTree
          key={path ?? ""}
          projectId={projectId}
          tree={tree.data}
          openPath={path}
        />
      )}
    </nav>
  );

  return (
    <div className="mx-auto w-full max-w-7xl p-4">
      <h1 className="sr-only">{t("docs.title")}</h1>
      <div className="mb-3 lg:hidden">
        <Button
          variant="outline"
          aria-expanded={filesOpen}
          aria-controls="docs-tree"
          onClick={() => {
            setFilesOpen((v) => !v);
          }}
        >
          {t("docs.tree.title")}
        </Button>
      </div>
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-[16rem_minmax(0,1fr)_18rem]">
        <div
          id="docs-tree"
          className={filesOpen ? "min-w-0" : "hidden min-w-0 lg:block"}
        >
          {treePanel}
        </div>
        {path === undefined ? (
          <>
            <Card className="space-y-2 p-4">
              <h2 className="font-semibold">{t("docs.file.choose")}</h2>
              <p className="text-sm text-muted-foreground">
                {t("docs.file.chooseBody")}
              </p>
            </Card>
            <div />
          </>
        ) : draftBuffer.kind === "loading" ? (
          <>
            <div
              role="status"
              aria-busy="true"
              aria-label={t("docs.file.loading")}
              className="min-w-0"
            >
              <Skeleton className="h-64" />
            </div>
            <div />
          </>
        ) : draftBuffer.kind === "missing" || draftBuffer.kind === "error" ? (
          <>
            <div className="min-w-0">
              {draftBuffer.kind === "missing" ? (
                <Card className="space-y-2 p-4">
                  <h2 className="font-semibold">
                    {t("docs.fromDraft.missing")}
                  </h2>
                  <p className="text-sm text-muted-foreground">
                    {t("docs.fromDraft.missingBody", { path })}
                  </p>
                </Card>
              ) : (
                <PageError
                  title="docs.fromDraft.error"
                  error={draftBuffer.error}
                  onRetry={() => void draftContent.refetch()}
                />
              )}
            </div>
            <div />
          </>
        ) : (
          <FileView
            key={path}
            projectId={projectId}
            path={path}
            branch={branch}
            buffer={buffers[path]}
            notice={
              draftBuffer.kind === "exists"
                ? t("docs.fromDraft.exists")
                : undefined
            }
            onSaved={saved}
          />
        )}
      </div>
    </div>
  );
}
