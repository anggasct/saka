import { Link, Navigate } from "@tanstack/react-router";

import { PageError } from "@/features/delivery/page-error";
import { useProjects } from "@/features/delivery/queries";
import { useT } from "@/i18n/use-t";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

const INIT_COMMAND = "saka init";

export function SettingsIndexPage() {
  const t = useT();
  const projects = useProjects();
  const items = projects.data ?? [];
  const only = items.length === 1 ? items[0] : undefined;
  if (only !== undefined) {
    return (
      <Navigate
        to="/settings/$projectId"
        params={{ projectId: only.id }}
        replace
      />
    );
  }
  return (
    <div className="mx-auto w-full max-w-4xl space-y-4 p-4">
      <h1 className="text-2xl font-semibold">{t("settings.title")}</h1>
      {projects.error !== null ? (
        <PageError
          title="settings.error.title"
          error={projects.error}
          onRetry={() => void projects.refetch()}
        />
      ) : projects.isPending ? (
        <div role="status" aria-busy="true" aria-label={t("settings.loading")}>
          <Skeleton className="h-24" />
        </div>
      ) : items.length === 0 ? (
        <Card className="space-y-3 p-4">
          <h2 className="font-semibold">{t("settings.empty.title")}</h2>
          <p className="text-sm text-muted-foreground">
            {t("settings.empty.body")}
          </p>
          <code className="block overflow-x-auto rounded bg-muted p-2 text-sm">
            {INIT_COMMAND}
          </code>
        </Card>
      ) : (
        <section aria-labelledby="settings-projects" className="space-y-2">
          <h2 id="settings-projects" className="font-semibold">
            {t("settings.projects.title")}
          </h2>
          <ul className="space-y-2">
            {items.map((p) => (
              <li key={p.id}>
                <Link
                  to="/settings/$projectId"
                  params={{ projectId: p.id }}
                  className="block min-h-11 rounded-lg border border-border p-3 hover:bg-muted"
                >
                  <span className="block font-medium">{p.name}</span>
                  <span className="block text-sm break-words text-muted-foreground">
                    {t("settings.projects.folder", {
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
