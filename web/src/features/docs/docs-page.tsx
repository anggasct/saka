import { useNavigate } from "@tanstack/react-router";
import { useMemo, useState } from "react";

import { PageError } from "@/features/delivery/page-error";
import { useProjects } from "@/features/delivery/queries";
import { useT } from "@/i18n/use-t";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import { FileTree } from "./file-tree";
import { FileView } from "./file-view";
import { NewFileDialog } from "./new-file-dialog";
import { useDocTree } from "./queries";

export function DocsPage({
  projectId,
  path,
}: {
  projectId: string;
  path: string | undefined;
}) {
  const t = useT();
  const navigate = useNavigate();
  const projects = useProjects();
  const tree = useDocTree(projectId);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [filesOpen, setFilesOpen] = useState(false);
  const [shownPath, setShownPath] = useState(path);
  if (shownPath !== path) {
    // On a narrow screen the list folds away once a file is chosen.
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
        ...Object.keys(drafts),
      ]),
    [tree.data, drafts],
  );

  const create = (newPath: string, content: string) => {
    setDrafts((current) => ({ ...current, [newPath]: content }));
    void navigate({
      to: "/docs/$projectId",
      params: { projectId },
      search: { path: newPath },
    });
  };

  const saved = (savedPath: string) => {
    setDrafts((current) => {
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
        ) : (
          <FileView
            key={path}
            projectId={projectId}
            path={path}
            branch={branch}
            draft={drafts[path]}
            onSaved={saved}
          />
        )}
      </div>
    </div>
  );
}
