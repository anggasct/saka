import { Link, Navigate } from "@tanstack/react-router";

import { PageError } from "@/features/delivery/page-error";
import { useProjects } from "@/features/delivery/queries";
import { useT } from "@/i18n/use-t";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

const ADD_PROJECT = "saka project add <repo-url> --path docs";

export function DocsIndexPage() {
  const t = useT();
  const projects = useProjects();
  const items = projects.data ?? [];
  const only = items.length === 1 ? items[0] : undefined;
  if (only !== undefined) {
    return (
      <Navigate
        to="/docs/$projectId"
        params={{ projectId: only.id }}
        search={{}}
        replace
      />
    );
  }
  return (
    <div className="mx-auto w-full max-w-4xl space-y-4 p-4">
      <h1 className="text-2xl font-semibold">{t("docs.title")}</h1>
      {projects.error !== null ? (
        <PageError
          title="docs.error.title"
          error={projects.error}
          onRetry={() => void projects.refetch()}
        />
      ) : projects.isPending ? (
        <div role="status" aria-busy="true" aria-label={t("docs.loading")}>
          <Skeleton className="h-24" />
        </div>
      ) : items.length === 0 ? (
        <Card className="space-y-3 p-4">
          <h2 className="font-semibold">{t("docs.empty.title")}</h2>
          <p className="text-sm text-muted-foreground">
            {t("docs.empty.body")}
          </p>
          <code className="block overflow-x-auto rounded bg-muted p-2 text-sm">
            {ADD_PROJECT}
          </code>
        </Card>
      ) : (
        <section aria-labelledby="docs-projects" className="space-y-2">
          <h2 id="docs-projects" className="font-semibold">
            {t("docs.projects.title")}
          </h2>
          <ul className="space-y-2">
            {items.map((p) => (
              <li key={p.id}>
                <Link
                  to="/docs/$projectId"
                  params={{ projectId: p.id }}
                  search={{}}
                  className="block min-h-11 rounded-lg border border-border p-3 hover:bg-muted"
                >
                  <span className="block font-medium">{p.name}</span>
                  <span className="block text-sm text-muted-foreground">
                    {t("docs.projects.folder", {
                      repo: p.workspace.full_name,
                      path: p.workspace.project_path,
                    })}
                  </span>
                </Link>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}
